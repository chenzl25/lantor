//! Data for rendering `#123` / `rw#123` references in messages: every channel
//! binding (with its short alias) plus the state of PRs and issues that are
//! already in the synced queues. Read-only and local; never calls GitHub
//! (references outside the queues go through `reference_states`). The same
//! bindings tell agents, in each turn header, how to write references.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Serialize;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::app::{to_string, CommandResult};

const MAX_ALIAS_LEN: usize = 16;

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct GithubReferenceRepository {
    pub(crate) channel_id: Uuid,
    pub(crate) name_with_owner: String,
    pub(crate) url: String,
    pub(crate) reference_alias: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct GithubReferenceItem {
    pub(crate) name_with_owner: String,
    pub(crate) number: i64,
    /// `pull` or `issue`.
    pub(crate) kind: &'static str,
    pub(crate) title: String,
    pub(crate) url: String,
    /// `open`, `closed` or `merged`.
    pub(crate) state: String,
    /// For closed issues: `completed` or `not_planned`; empty otherwise.
    pub(crate) state_reason: String,
    pub(crate) is_draft: bool,
    pub(crate) checks_status: String,
    pub(crate) updated_at: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct GithubReferences {
    pub(crate) repositories: Vec<GithubReferenceRepository>,
    pub(crate) items: Vec<GithubReferenceItem>,
}

pub(crate) async fn load_github_references(pool: &SqlitePool) -> CommandResult<GithubReferences> {
    let repositories = load_reference_bindings(pool).await?;

    // A repository can be bound to several channels and queued for several
    // review logins; keep the most recently updated copy of each item.
    let rows = sqlx::query(
        r#"
        select
            binding.name_with_owner, 'pull' as kind, cache.pull_number as number,
            cache.title, cache.url, cache.state, cache.is_draft, cache.checks_status,
            cache.github_updated_at
        from github_review_request_cache cache
        join channel_github_repositories binding
          on binding.channel_id = cache.channel_id
         and binding.repository_id = cache.repository_id
        union all
        select
            binding.name_with_owner, 'issue' as kind, cache.issue_number as number,
            cache.title, cache.url, cache.state, 0 as is_draft, 'none' as checks_status,
            cache.github_updated_at
        from github_issue_cache cache
        join channel_github_repositories binding
          on binding.channel_id = cache.channel_id
         and binding.repository_id = cache.repository_id
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    let mut items: HashMap<(String, i64), GithubReferenceItem> = HashMap::new();
    for row in rows {
        let kind: String = row.get("kind");
        let item = GithubReferenceItem {
            name_with_owner: row.get("name_with_owner"),
            number: row.get("number"),
            kind: if kind == "pull" { "pull" } else { "issue" },
            title: row.get("title"),
            url: row.get("url"),
            state: row.get::<String, _>("state").to_ascii_lowercase(),
            state_reason: String::new(),
            is_draft: row.get("is_draft"),
            checks_status: row.get("checks_status"),
            updated_at: row.get("github_updated_at"),
        };
        let key = (item.name_with_owner.to_ascii_lowercase(), item.number);
        match items.get(&key) {
            Some(existing) if existing.updated_at >= item.updated_at => {}
            _ => {
                items.insert(key, item);
            }
        }
    }
    let mut items: Vec<_> = items.into_values().collect();
    items.sort_by(|left, right| {
        (&left.name_with_owner, left.number).cmp(&(&right.name_with_owner, right.number))
    });

    Ok(GithubReferences {
        repositories,
        items,
    })
}

/// Turn-header lines that tell an agent how to write GitHub references: the
/// repository a bare `#123` links to in this channel, and a short name for
/// every bound repository. Empty when no channel has a repository.
pub(crate) async fn load_agent_github_reference_lines(
    pool: &SqlitePool,
    channel_id: Option<Uuid>,
) -> CommandResult<Vec<String>> {
    let bindings = load_reference_bindings(pool).await?;
    Ok(agent_github_reference_lines(&bindings, channel_id))
}

async fn load_reference_bindings(
    pool: &SqlitePool,
) -> CommandResult<Vec<GithubReferenceRepository>> {
    Ok(sqlx::query(
        r#"
        select channel_id, name_with_owner, url, reference_alias
        from channel_github_repositories
        order by name_with_owner, channel_id
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(to_string)?
    .iter()
    .map(|row| GithubReferenceRepository {
        channel_id: row.get("channel_id"),
        name_with_owner: row.get("name_with_owner"),
        url: row.get("url"),
        reference_alias: row.get("reference_alias"),
    })
    .collect())
}

/// The short name for each repository is its explicit alias, else its
/// repository name when no other bound repository has that name or alias,
/// else just `owner/repo`. These are the names the web UI resolves in `rw#123`
/// and `rw #123` (`buildGithubReferenceIndex` in src/github-references.ts).
pub(crate) fn agent_github_reference_lines(
    bindings: &[GithubReferenceRepository],
    channel_id: Option<Uuid>,
) -> Vec<String> {
    if bindings.is_empty() {
        return Vec::new();
    }
    // Lower-cased `owner/repo` → (canonical `owner/repo`, alias).
    let mut repositories: BTreeMap<String, (&str, &str)> = BTreeMap::new();
    for binding in bindings {
        let entry = repositories
            .entry(binding.name_with_owner.to_ascii_lowercase())
            .or_insert((&binding.name_with_owner, ""));
        if entry.1.is_empty() {
            entry.1 = binding.reference_alias.trim();
        }
    }
    let aliases: HashSet<String> = repositories
        .values()
        .filter(|(_, alias)| !alias.is_empty())
        .map(|(_, alias)| alias.to_ascii_lowercase())
        .collect();
    let mut name_counts: HashMap<String, usize> = HashMap::new();
    for (name_with_owner, _) in repositories.values() {
        *name_counts
            .entry(repository_name(name_with_owner).to_ascii_lowercase())
            .or_default() += 1;
    }
    let short_names: Vec<String> = repositories
        .values()
        .map(|(name_with_owner, alias)| {
            if !alias.is_empty() {
                return format!("{alias}={name_with_owner}");
            }
            let name = repository_name(name_with_owner);
            let key = name.to_ascii_lowercase();
            if name_counts.get(&key) == Some(&1) && !aliases.contains(&key) && key != "task" {
                format!("{name}={name_with_owner}")
            } else {
                (*name_with_owner).to_owned()
            }
        })
        .collect();

    let channel_repository = channel_id.and_then(|channel_id| {
        bindings
            .iter()
            .find(|binding| binding.channel_id == channel_id)
            .map(|binding| binding.name_with_owner.as_str())
    });
    vec![
        match channel_repository {
            Some(repository) => format!("github_repo: {repository} (a bare #123 here links to it)"),
            None => "github_repo: none (a bare #123 here is not linked)".to_owned(),
        },
        format!("github_names: {}", short_names.join(", ")),
    ]
}

fn repository_name(name_with_owner: &str) -> &str {
    name_with_owner
        .split_once('/')
        .map_or(name_with_owner, |(_, name)| name)
}

/// Normalizes a user-entered alias: lower-case ASCII letter first, then
/// letters, digits or `-`. An empty alias clears it.
pub(crate) fn normalized_reference_alias(alias: &str) -> CommandResult<String> {
    let alias = alias.trim().trim_end_matches('#').to_ascii_lowercase();
    if alias.is_empty() {
        return Ok(alias);
    }
    let valid = alias.len() <= MAX_ALIAS_LEN
        && alias.starts_with(|ch: char| ch.is_ascii_lowercase())
        && alias
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-');
    if !valid {
        return Err(format!(
            "short name must start with a letter and use at most {MAX_ALIAS_LEN} letters, digits or '-'"
        ));
    }
    if alias == "task" {
        return Err("`task` is reserved for Lantor task references".to_owned());
    }
    Ok(alias)
}

pub(crate) async fn set_github_reference_alias(
    pool: &SqlitePool,
    channel_id: Uuid,
    alias: &str,
) -> CommandResult<GithubReferences> {
    let alias = normalized_reference_alias(alias)?;
    if !alias.is_empty() {
        let taken: Option<String> = sqlx::query_scalar(
            r#"
            select name_with_owner
            from channel_github_repositories
            where reference_alias = $1
              and channel_id <> $2
              and lower(name_with_owner) <> (
                  select lower(name_with_owner)
                  from channel_github_repositories
                  where channel_id = $2
              )
            limit 1
            "#,
        )
        .bind(&alias)
        .bind(channel_id)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?;
        if let Some(repository) = taken {
            return Err(format!(
                "short name `{alias}` is already used by {repository}"
            ));
        }
    }
    let updated = sqlx::query(
        r#"
        update channel_github_repositories
        set reference_alias = $2,
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where channel_id = $1
        "#,
    )
    .bind(channel_id)
    .bind(&alias)
    .execute(pool)
    .await
    .map_err(to_string)?;
    if updated.rows_affected() == 0 {
        return Err("channel has no GitHub repository binding".to_owned());
    }
    load_github_references(pool).await
}

#[cfg(test)]
mod tests;
