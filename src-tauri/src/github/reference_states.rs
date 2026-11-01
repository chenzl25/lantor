//! State of PRs and issues that messages reference but the synced queues do not
//! hold, typically merged or closed ones. Looked up on demand with one
//! `gh api graphql` query per repository batch and cached in
//! `github_reference_state_cache`: merged PRs for good, closed items and
//! unknown numbers for a day, open items for ten minutes.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    future::Future,
};

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Row, SqlitePool};

use super::{parse_json, references::GithubReferenceItem, run_github_graphql_partial};
use crate::app::{to_string, CommandResult};

/// Upper bound per request; extra references are ignored.
const MAX_LOOKUP_REFERENCES: usize = 200;
const GRAPHQL_BATCH_SIZE: usize = 50;
const MISSING_KIND: &str = "missing";

fn open_ttl() -> Duration {
    Duration::minutes(10)
}

/// Closed items can be reopened and unknown numbers can be created later.
fn closed_ttl() -> Duration {
    Duration::hours(24)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GithubReferenceKey {
    pub(crate) name_with_owner: String,
    pub(crate) number: i64,
}

#[derive(Debug, Serialize)]
pub(crate) struct GithubReferenceLookup {
    /// Found PRs and issues; numbers GitHub does not know are left out.
    pub(crate) items: Vec<GithubReferenceItem>,
    /// Set when GitHub could not be queried for some references; their
    /// previously cached state, if any, is returned instead.
    pub(crate) error: Option<String>,
}

struct CachedState {
    /// `None` when GitHub reported the number as not found.
    item: Option<GithubReferenceItem>,
    fetched_at: DateTime<Utc>,
}

enum Outcome {
    Found(GithubReferenceItem),
    Missing,
    Failed,
}

type ReferenceKey = (String, i64);

pub(crate) async fn lookup_github_reference_states(
    pool: &SqlitePool,
    references: Vec<GithubReferenceKey>,
) -> CommandResult<GithubReferenceLookup> {
    lookup_with(pool, references, Utc::now(), run_github_graphql_partial).await
}

async fn lookup_with<F, Fut>(
    pool: &SqlitePool,
    references: Vec<GithubReferenceKey>,
    now: DateTime<Utc>,
    run: F,
) -> CommandResult<GithubReferenceLookup>
where
    F: Fn(Vec<String>) -> Fut,
    Fut: Future<Output = CommandResult<Vec<u8>>>,
{
    let wanted = normalized_references(references);
    if wanted.is_empty() {
        return Ok(GithubReferenceLookup {
            items: Vec::new(),
            error: None,
        });
    }
    let cached = load_cached(pool, &wanted).await?;
    let cached_item = |key: &ReferenceKey| cached.get(key).and_then(|entry| entry.item.clone());

    let mut items = Vec::new();
    // Repository key → (name as referenced, stale numbers).
    let mut stale: BTreeMap<String, (String, Vec<i64>)> = BTreeMap::new();
    for (repository_key, name_with_owner, number) in &wanted {
        let key = (repository_key.clone(), *number);
        match cached.get(&key) {
            Some(entry) if is_fresh(entry, now) => items.extend(entry.item.clone()),
            _ => stale
                .entry(repository_key.clone())
                .or_insert_with(|| (name_with_owner.clone(), Vec::new()))
                .1
                .push(*number),
        }
    }

    let mut errors = Vec::new();
    for (repository_key, (name_with_owner, numbers)) in stale {
        for batch in numbers.chunks(GRAPHQL_BATCH_SIZE) {
            let outcomes = match fetch_batch(&run, &name_with_owner, batch).await {
                Ok(outcomes) => outcomes,
                Err(error) => {
                    errors.push(format!("{name_with_owner}: {error}"));
                    items.extend(
                        batch
                            .iter()
                            .filter_map(|number| cached_item(&(repository_key.clone(), *number))),
                    );
                    continue;
                }
            };
            store(pool, &repository_key, &name_with_owner, &outcomes, now).await?;
            let mut failed = false;
            for (number, outcome) in outcomes {
                match outcome {
                    Outcome::Found(item) => items.push(item),
                    Outcome::Missing => {}
                    Outcome::Failed => {
                        failed = true;
                        items.extend(cached_item(&(repository_key.clone(), number)));
                    }
                }
            }
            if failed {
                errors.push(format!(
                    "{name_with_owner}: GitHub did not return some references"
                ));
            }
        }
    }

    items.sort_by(|left, right| {
        (left.name_with_owner.to_ascii_lowercase(), left.number)
            .cmp(&(right.name_with_owner.to_ascii_lowercase(), right.number))
    });
    Ok(GithubReferenceLookup {
        items,
        error: (!errors.is_empty()).then(|| errors.join("; ")),
    })
}

fn valid_repository_part(part: &str) -> bool {
    part.starts_with(|ch: char| ch.is_ascii_alphanumeric())
        && part
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-'))
}

/// Valid, de-duplicated `(lower-cased owner/repo, owner/repo, number)` triples.
/// Numbers are interpolated into the GraphQL query, so they must be positive
/// 32-bit integers (GraphQL `Int`).
fn normalized_references(references: Vec<GithubReferenceKey>) -> Vec<(String, String, i64)> {
    let mut seen = HashSet::new();
    references
        .into_iter()
        .filter_map(|reference| {
            let name_with_owner = reference.name_with_owner.trim();
            let (owner, repository) = name_with_owner.split_once('/')?;
            let valid = valid_repository_part(owner)
                && valid_repository_part(repository)
                && (1..=i64::from(i32::MAX)).contains(&reference.number);
            valid.then(|| {
                (
                    name_with_owner.to_ascii_lowercase(),
                    name_with_owner.to_owned(),
                    reference.number,
                )
            })
        })
        .filter(|(key, _, number)| seen.insert((key.clone(), *number)))
        .take(MAX_LOOKUP_REFERENCES)
        .collect()
}

fn is_fresh(entry: &CachedState, now: DateTime<Utc>) -> bool {
    let ttl = match &entry.item {
        Some(item) if item.state == "merged" => return true,
        Some(item) if item.state == "open" => open_ttl(),
        _ => closed_ttl(),
    };
    now.signed_duration_since(entry.fetched_at) < ttl
}

async fn load_cached(
    pool: &SqlitePool,
    wanted: &[(String, String, i64)],
) -> CommandResult<HashMap<ReferenceKey, CachedState>> {
    let keys = serde_json::to_string(
        &wanted
            .iter()
            .map(|(key, _, number)| (key, number))
            .collect::<Vec<_>>(),
    )
    .map_err(to_string)?;
    let rows = sqlx::query(
        r#"
        select
            repository_key, number, kind, name_with_owner, title, url, state, state_reason,
            is_draft, checks_status, github_updated_at, fetched_at
        from github_reference_state_cache
        where (repository_key, number) in (
            select json_extract(value, '$[0]'), json_extract(value, '$[1]')
            from json_each($1)
        )
        "#,
    )
    .bind(keys)
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    let mut cached = HashMap::new();
    for row in rows {
        let Ok(fetched_at) = DateTime::parse_from_rfc3339(row.get::<&str, _>("fetched_at")) else {
            continue;
        };
        let kind: String = row.get("kind");
        let item = match kind.as_str() {
            "pull" | "issue" => Some(GithubReferenceItem {
                name_with_owner: row.get("name_with_owner"),
                number: row.get("number"),
                kind: if kind == "pull" { "pull" } else { "issue" },
                title: row.get("title"),
                url: row.get("url"),
                state: row.get("state"),
                state_reason: row.get("state_reason"),
                is_draft: row.get("is_draft"),
                checks_status: row.get("checks_status"),
                updated_at: row.get("github_updated_at"),
            }),
            _ => None,
        };
        cached.insert(
            (row.get("repository_key"), row.get("number")),
            CachedState {
                item,
                fetched_at: fetched_at.with_timezone(&Utc),
            },
        );
    }
    Ok(cached)
}

async fn store(
    pool: &SqlitePool,
    repository_key: &str,
    name_with_owner: &str,
    outcomes: &[(i64, Outcome)],
    now: DateTime<Utc>,
) -> CommandResult<()> {
    let fetched_at = now.to_rfc3339_opts(SecondsFormat::Millis, true);
    let mut tx = pool.begin().await.map_err(to_string)?;
    for (number, outcome) in outcomes {
        let missing;
        let item = match outcome {
            Outcome::Found(item) => item,
            Outcome::Missing => {
                missing = GithubReferenceItem {
                    name_with_owner: name_with_owner.to_owned(),
                    number: *number,
                    kind: MISSING_KIND,
                    title: String::new(),
                    url: String::new(),
                    state: String::new(),
                    state_reason: String::new(),
                    is_draft: false,
                    checks_status: "none".to_owned(),
                    updated_at: String::new(),
                };
                &missing
            }
            Outcome::Failed => continue,
        };
        sqlx::query(
            r#"
            insert into github_reference_state_cache (
                repository_key, number, kind, name_with_owner, title, url, state, state_reason,
                is_draft, checks_status, github_updated_at, fetched_at
            )
            values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            on conflict (repository_key, number) do update set
                kind = excluded.kind,
                name_with_owner = excluded.name_with_owner,
                title = excluded.title,
                url = excluded.url,
                state = excluded.state,
                state_reason = excluded.state_reason,
                is_draft = excluded.is_draft,
                checks_status = excluded.checks_status,
                github_updated_at = excluded.github_updated_at,
                fetched_at = excluded.fetched_at
            "#,
        )
        .bind(repository_key)
        .bind(number)
        .bind(item.kind)
        .bind(&item.name_with_owner)
        .bind(&item.title)
        .bind(&item.url)
        .bind(&item.state)
        .bind(&item.state_reason)
        .bind(item.is_draft)
        .bind(&item.checks_status)
        .bind(&item.updated_at)
        .bind(&fetched_at)
        .execute(&mut *tx)
        .await
        .map_err(to_string)?;
    }
    tx.commit().await.map_err(to_string)
}

fn batch_query(numbers: &[i64]) -> String {
    let fields = numbers
        .iter()
        .map(|number| {
            format!(
                r#"
        n{number}: issueOrPullRequest(number: {number}) {{
            __typename
            ... on PullRequest {{
                number title url state isDraft updatedAt
                commits(last: 1) {{ nodes {{ commit {{ statusCheckRollup {{ state }} }} }} }}
            }}
            ... on Issue {{ number title url state stateReason updatedAt }}
        }}"#
            )
        })
        .collect::<String>();
    format!(
        "query($owner: String!, $name: String!) {{ repository(owner: $owner, name: $name) {{ {fields} }} }}"
    )
}

async fn fetch_batch<F, Fut>(
    run: &F,
    name_with_owner: &str,
    numbers: &[i64],
) -> CommandResult<Vec<(i64, Outcome)>>
where
    F: Fn(Vec<String>) -> Fut,
    Fut: Future<Output = CommandResult<Vec<u8>>>,
{
    let (owner, name) = name_with_owner
        .split_once('/')
        .ok_or_else(|| "invalid GitHub repository".to_owned())?;
    let output = run(vec![
        "api".to_owned(),
        "graphql".to_owned(),
        "-f".to_owned(),
        format!("owner={owner}"),
        "-f".to_owned(),
        format!("name={name}"),
        "-f".to_owned(),
        format!("query={}", batch_query(numbers)),
    ])
    .await?;
    parse_batch(&output, name_with_owner, numbers)
}

#[derive(Deserialize)]
struct RollupState {
    state: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Commit {
    status_check_rollup: Option<RollupState>,
}

#[derive(Deserialize)]
struct CommitNode {
    commit: Commit,
}

#[derive(Deserialize)]
struct Commits {
    nodes: Vec<CommitNode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReferenceNode {
    #[serde(rename = "__typename")]
    typename: String,
    number: i64,
    title: String,
    url: String,
    state: String,
    #[serde(default)]
    is_draft: bool,
    #[serde(default)]
    state_reason: Option<String>,
    updated_at: String,
    #[serde(default)]
    commits: Option<Commits>,
}

fn checks_status(rollup: &str) -> &'static str {
    match rollup.to_ascii_uppercase().as_str() {
        "SUCCESS" => "success",
        "FAILURE" | "ERROR" => "failure",
        "PENDING" | "EXPECTED" => "pending",
        _ => "none",
    }
}

fn reference_item(node: ReferenceNode, name_with_owner: &str) -> Option<GithubReferenceItem> {
    let kind = match node.typename.as_str() {
        "PullRequest" => "pull",
        "Issue" => "issue",
        _ => return None,
    };
    let state = node.state.to_ascii_lowercase();
    // Checks of merged or closed PRs no longer matter (and often stay pending).
    let checks = if kind == "pull" && state == "open" {
        node.commits
            .and_then(|commits| commits.nodes.into_iter().last())
            .and_then(|node| node.commit.status_check_rollup)
            .map_or("none", |rollup| checks_status(&rollup.state))
    } else {
        "none"
    };
    let state_reason = if kind == "issue" && state == "closed" {
        node.state_reason.unwrap_or_default().to_ascii_lowercase()
    } else {
        String::new()
    };
    Some(GithubReferenceItem {
        name_with_owner: name_with_owner.to_owned(),
        number: node.number,
        kind,
        title: node.title,
        url: node.url,
        state,
        state_reason,
        is_draft: kind == "pull" && node.is_draft,
        checks_status: checks.to_owned(),
        updated_at: node.updated_at,
    })
}

fn parse_batch(
    bytes: &[u8],
    name_with_owner: &str,
    numbers: &[i64],
) -> CommandResult<Vec<(i64, Outcome)>> {
    let response: Value = parse_json(bytes, "GitHub reference batch")?;
    let errors = response
        .get("errors")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let not_found = |path: &[&str]| {
        errors.iter().any(|error| {
            error.get("type").and_then(Value::as_str) == Some("NOT_FOUND")
                && error
                    .get("path")
                    .and_then(Value::as_array)
                    .is_some_and(|actual| {
                        actual.len() == path.len()
                            && actual
                                .iter()
                                .zip(path)
                                .all(|(actual, expected)| actual.as_str() == Some(expected))
                    })
        })
    };
    let first_error = || {
        errors
            .first()
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("GitHub returned no data")
            .chars()
            .take(300)
            .collect::<String>()
    };

    let repository = response
        .pointer("/data/repository")
        .filter(|value| !value.is_null());
    let Some(repository) = repository else {
        if not_found(&["repository"]) {
            return Ok(numbers
                .iter()
                .map(|number| (*number, Outcome::Missing))
                .collect());
        }
        return Err(first_error());
    };

    Ok(numbers
        .iter()
        .map(|number| {
            let alias = format!("n{number}");
            let outcome = match repository.get(&alias).filter(|value| !value.is_null()) {
                None if not_found(&["repository", &alias]) => Outcome::Missing,
                None => Outcome::Failed,
                Some(value) => serde_json::from_value::<ReferenceNode>(value.clone())
                    .ok()
                    .filter(|node| node.number == *number)
                    .and_then(|node| reference_item(node, name_with_owner))
                    .map_or(Outcome::Failed, Outcome::Found),
            };
            (*number, outcome)
        })
        .collect())
}

#[cfg(test)]
mod tests;
