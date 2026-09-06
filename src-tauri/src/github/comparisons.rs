//! Successful comparisons are immutable for an exact repository/base/head tuple.
//! Optional counts are loaded separately so they cannot hold up the PR list.

use std::{collections::HashMap, future::Future};

use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool};
use tokio::task::JoinSet;
use uuid::Uuid;

use super::{
    load_cached_review_requests, load_github_binding, load_resource_links, parse_json,
    run_github_cli, GithubPullRequestSnapshot, GithubRepositoryBinding, GithubResourceLink,
    PULL_REQUEST_RESOURCE_KIND,
};
use crate::app::{to_string, CommandResult};

const COMPARISON_CONCURRENCY: usize = 4;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GithubCompareCli {
    status: String,
    ahead_by: i64,
}

#[derive(Debug, Serialize)]
pub(crate) struct GithubReviewComparisons {
    pub(crate) repository_id: String,
    pub(crate) comparisons: Vec<GithubReviewComparison>,
}

#[derive(Debug, Serialize)]
pub(crate) struct GithubReviewComparison {
    pub(crate) pull_number: i64,
    pub(crate) review_anchor_sha: String,
    pub(crate) head_sha: String,
    pub(crate) commits_ahead: i64,
}

#[derive(Clone)]
struct ComparisonTarget {
    pull_number: i64,
    base_sha: String,
    head_sha: String,
}

fn comparison_targets(
    requests: &[GithubPullRequestSnapshot],
    links: &HashMap<i64, GithubResourceLink>,
) -> Vec<ComparisonTarget> {
    requests
        .iter()
        .filter_map(|snapshot| {
            let pull = &snapshot.pull_request;
            let link = links.get(&pull.number)?;
            if link.head_sha.is_empty()
                || pull.head_ref_oid.is_empty()
                || link.head_sha == pull.head_ref_oid
            {
                return None;
            }
            Some(ComparisonTarget {
                pull_number: pull.number,
                base_sha: link.head_sha.clone(),
                head_sha: pull.head_ref_oid.clone(),
            })
        })
        .collect()
}

pub(super) async fn cached_counts_for_requests(
    pool: &SqlitePool,
    binding: &GithubRepositoryBinding,
    requests: &[GithubPullRequestSnapshot],
    links: &HashMap<i64, GithubResourceLink>,
) -> CommandResult<HashMap<i64, i64>> {
    let targets = comparison_targets(requests, links);
    let mut counts = HashMap::new();
    for batch in targets.chunks(100) {
        let mut query = QueryBuilder::<Sqlite>::new(
            "select base_sha, head_sha, ahead_by from github_commit_comparisons where repository_id = ",
        );
        query.push_bind(&binding.repository_id).push(" and (");
        for (index, target) in batch.iter().enumerate() {
            if index > 0 {
                query.push(" or ");
            }
            query
                .push("(base_sha = ")
                .push_bind(&target.base_sha)
                .push(" and head_sha = ")
                .push_bind(&target.head_sha)
                .push(")");
        }
        query.push(")");
        let rows = query.build().fetch_all(pool).await.map_err(to_string)?;
        let by_sha = rows
            .into_iter()
            .map(|row| {
                (
                    (
                        row.get::<String, _>("base_sha"),
                        row.get::<String, _>("head_sha"),
                    ),
                    row.get::<i64, _>("ahead_by"),
                )
            })
            .collect::<HashMap<_, _>>();
        for target in batch {
            if let Some(count) = by_sha.get(&(target.base_sha.clone(), target.head_sha.clone())) {
                counts.insert(target.pull_number, *count);
            }
        }
    }
    Ok(counts)
}

async fn cached_or_fetch<F, Fut>(
    pool: &SqlitePool,
    repository_id: &str,
    base_sha: &str,
    head_sha: &str,
    fetch: F,
) -> CommandResult<i64>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = CommandResult<i64>>,
{
    if base_sha.is_empty() || head_sha.is_empty() {
        return Err("GitHub comparison requires both commit SHAs".to_owned());
    }
    if base_sha == head_sha {
        return Ok(0);
    }
    let cached: Option<i64> = sqlx::query_scalar(
        "select ahead_by from github_commit_comparisons where repository_id = $1 and base_sha = $2 and head_sha = $3",
    ).bind(repository_id).bind(base_sha).bind(head_sha)
        .fetch_optional(pool).await.map_err(to_string)?;
    if let Some(count) = cached {
        return Ok(count);
    }
    let count = fetch().await?;
    sqlx::query(
        "insert into github_commit_comparisons (repository_id, base_sha, head_sha, ahead_by) values ($1, $2, $3, $4) on conflict do nothing",
    ).bind(repository_id).bind(base_sha).bind(head_sha).bind(count)
        .execute(pool).await.map_err(to_string)?;
    Ok(count)
}

pub(crate) async fn load_github_commits_ahead(
    pool: &SqlitePool,
    binding: &GithubRepositoryBinding,
    previous_head_sha: &str,
    head_sha: &str,
) -> CommandResult<i64> {
    cached_or_fetch(
        pool,
        &binding.repository_id,
        previous_head_sha,
        head_sha,
        || async {
            let output = run_github_cli(vec![
                "api".to_owned(),
                format!(
                    "repos/{}/compare/{}...{}",
                    binding.name_with_owner, previous_head_sha, head_sha
                ),
                "--jq".to_owned(),
                "{status: .status, aheadBy: .ahead_by}".to_owned(),
            ])
            .await?;
            let comparison: GithubCompareCli = parse_json(&output, "GitHub commit comparison")?;
            Ok(if comparison.status.eq_ignore_ascii_case("identical") {
                0
            } else {
                comparison.ahead_by.max(0)
            })
        },
    )
    .await
}

async fn resolve_comparisons<F, Fut>(
    targets: Vec<ComparisonTarget>,
    fetch: F,
) -> CommandResult<Vec<GithubReviewComparison>>
where
    F: Fn(ComparisonTarget) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = CommandResult<i64>> + Send + 'static,
{
    let mut targets = targets.into_iter();
    let mut pending = JoinSet::new();
    let mut comparisons = Vec::new();
    loop {
        while pending.len() < COMPARISON_CONCURRENCY {
            let Some(target) = targets.next() else {
                break;
            };
            let fetch = fetch.clone();
            pending.spawn(async move {
                let count = fetch(target.clone()).await?;
                Ok::<_, String>(GithubReviewComparison {
                    pull_number: target.pull_number,
                    review_anchor_sha: target.base_sha,
                    head_sha: target.head_sha,
                    commits_ahead: count,
                })
            });
        }
        let Some(result) = pending.join_next().await else {
            break;
        };
        // An unavailable comparison must not discard other PRs' counts.
        if let Ok(comparison) = result.map_err(to_string)? {
            comparisons.push(comparison);
        }
    }
    comparisons.sort_by_key(|comparison| comparison.pull_number);
    Ok(comparisons)
}

pub(crate) async fn load_github_review_comparisons(
    pool: &SqlitePool,
    channel_id: Uuid,
) -> CommandResult<GithubReviewComparisons> {
    let binding = load_github_binding(pool, channel_id)
        .await?
        .ok_or_else(|| "channel has no GitHub repository binding".to_owned())?;
    let requests = load_cached_review_requests(pool, &binding).await?;
    let links = load_resource_links(
        pool,
        channel_id,
        &binding.repository_id,
        PULL_REQUEST_RESOURCE_KIND,
    )
    .await?;
    let targets = comparison_targets(&requests, &links);
    let repository_id = binding.repository_id.clone();
    let pool = pool.clone();
    let comparisons = resolve_comparisons(targets, move |target| {
        let pool = pool.clone();
        let binding = binding.clone();
        async move {
            load_github_commits_ahead(&pool, &binding, &target.base_sha, &target.head_sha).await
        }
    })
    .await?;
    Ok(GithubReviewComparisons {
        repository_id,
        comparisons,
    })
}

#[cfg(test)]
mod tests;
