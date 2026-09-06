//! Batched metadata for linked PRs, including all check-context pages.

use std::{collections::HashSet, future::Future};

use serde::Deserialize;
use serde_json::Value;

use super::{
    parse_json, run_github_cli, GithubPullRequestCli, GithubRepositoryBinding, GithubStatusCheckCli,
};
use crate::app::CommandResult;

const PULL_REQUEST_BATCH_SIZE: usize = 20;
const CHECK_FIELDS: &str = r#"
    nodes {
        __typename
        ... on CheckRun { name status conclusion }
        ... on StatusContext { context state }
    }
    pageInfo { hasNextPage endCursor }
"#;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CheckPage {
    nodes: Vec<GithubStatusCheckCli>,
    page_info: PageInfo,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CheckRollup {
    contexts: CheckPage,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Commit {
    oid: String,
    status_check_rollup: Option<CheckRollup>,
}

#[derive(Deserialize)]
struct PullRequestCommit {
    commit: Commit,
}

#[derive(Deserialize)]
struct Commits {
    nodes: Vec<PullRequestCommit>,
}

#[derive(Deserialize)]
struct PullRequest {
    #[serde(flatten)]
    metadata: GithubPullRequestCli,
    commits: Commits,
}

struct PendingPullRequest {
    metadata: GithubPullRequestCli,
    next_cursor: Option<String>,
}

fn batch_query(numbers: &[i64]) -> String {
    let fields = numbers
        .iter()
        .map(|number| {
            format!(
                r#"
        pr{number}: pullRequest(number: {number}) {{
            number title url author {{ login }} isDraft state updatedAt headRefOid
            commits(last: 1) {{ nodes {{ commit {{ oid statusCheckRollup {{
                contexts(first: 100) {{ {CHECK_FIELDS} }}
            }} }} }} }}
        }}
    "#
            )
        })
        .collect::<String>();
    format!("query($owner: String!, $name: String!) {{ repository(owner: $owner, name: $name) {{ {fields} }} }}")
}

fn next_cursor(page: &PageInfo) -> CommandResult<Option<String>> {
    if !page.has_next_page {
        return Ok(None);
    }
    page.end_cursor
        .clone()
        .filter(|cursor| !cursor.is_empty())
        .map(Some)
        .ok_or_else(|| "GitHub returned a check page without a continuation cursor".to_owned())
}

fn parse_batch(bytes: &[u8], numbers: &[i64]) -> CommandResult<Vec<PendingPullRequest>> {
    let response: Value = parse_json(bytes, "GitHub pull request batch")?;
    if response
        .get("errors")
        .and_then(Value::as_array)
        .is_some_and(|errors| !errors.is_empty())
    {
        return Err("GitHub returned errors in the pull request batch".to_owned());
    }
    let repository = response
        .pointer("/data/repository")
        .and_then(Value::as_object)
        .ok_or_else(|| "GitHub did not return the requested repository".to_owned())?;
    numbers.iter().map(|number| {
        let value = repository.get(&format!("pr{number}")).filter(|value| !value.is_null())
            .ok_or_else(|| format!("GitHub did not return linked pull request #{number}"))?;
        let mut pull: PullRequest = serde_json::from_value(value.clone())
            .map_err(|err| format!("failed to parse GitHub pull request #{number}: {err}"))?;
        if pull.metadata.number != *number {
            return Err("GitHub returned an unexpected pull request in the batch".to_owned());
        }
        let mut cursor = None;
        if let Some(last) = pull.commits.nodes.pop() {
            if last.commit.oid != pull.metadata.head_ref_oid {
                return Err(format!("GitHub pull request #{number} head changed while loading checks; refresh and retry"));
            }
            if let Some(rollup) = last.commit.status_check_rollup {
                cursor = next_cursor(&rollup.contexts.page_info)?;
                pull.metadata.status_check_rollup = rollup.contexts.nodes;
            }
        }
        Ok(PendingPullRequest { metadata: pull.metadata, next_cursor: cursor })
    }).collect()
}

pub(super) async fn load_linked_pull_requests(
    binding: &GithubRepositoryBinding,
    numbers: &[i64],
) -> CommandResult<Vec<GithubPullRequestCli>> {
    load_linked_pull_requests_with(binding, numbers, run_github_cli).await
}

async fn load_linked_pull_requests_with<F, Fut>(
    binding: &GithubRepositoryBinding,
    numbers: &[i64],
    run: F,
) -> CommandResult<Vec<GithubPullRequestCli>>
where
    F: Fn(Vec<String>) -> Fut,
    Fut: Future<Output = CommandResult<Vec<u8>>>,
{
    if numbers.is_empty() {
        return Ok(Vec::new());
    }
    if numbers.iter().any(|number| *number <= 0) {
        return Err("pull request number must be positive".to_owned());
    }
    let (owner, name) = binding
        .name_with_owner
        .split_once('/')
        .ok_or_else(|| "invalid GitHub repository binding".to_owned())?;
    let mut pulls = Vec::with_capacity(numbers.len());
    for batch in numbers.chunks(PULL_REQUEST_BATCH_SIZE) {
        let output = run(vec![
            "api".to_owned(),
            "graphql".to_owned(),
            "-f".to_owned(),
            format!("owner={owner}"),
            "-f".to_owned(),
            format!("name={name}"),
            "-f".to_owned(),
            format!("query={}", batch_query(batch)),
        ])
        .await?;
        // Reject incomplete batches before any PR is interpreted as closed.
        for mut pull in parse_batch(&output, batch)? {
            let mut seen_cursors = HashSet::new();
            while let Some(cursor) = pull.next_cursor.take() {
                if !seen_cursors.insert(cursor.clone()) {
                    return Err("GitHub check pagination did not advance".to_owned());
                }
                // Pin follow-up pages to the same commit even if the PR is pushed meanwhile.
                let query = format!(
                    r#"
                    query($owner: String!, $name: String!, $head: GitObjectID!, $cursor: String!) {{
                        repository(owner: $owner, name: $name) {{
                            object(oid: $head) {{ ... on Commit {{ statusCheckRollup {{
                                contexts(first: 100, after: $cursor) {{ {CHECK_FIELDS} }}
                            }} }} }}
                        }}
                    }}
                "#
                );
                let output = run(vec![
                    "api".to_owned(),
                    "graphql".to_owned(),
                    "-f".to_owned(),
                    format!("owner={owner}"),
                    "-f".to_owned(),
                    format!("name={name}"),
                    "-f".to_owned(),
                    format!("head={}", pull.metadata.head_ref_oid),
                    "-f".to_owned(),
                    format!("cursor={cursor}"),
                    "-f".to_owned(),
                    format!("query={query}"),
                ])
                .await?;
                let response: Value = parse_json(&output, "GitHub check page")?;
                if response
                    .get("errors")
                    .and_then(Value::as_array)
                    .is_some_and(|errors| !errors.is_empty())
                {
                    return Err("GitHub returned errors loading check contexts".to_owned());
                }
                let page: CheckPage = serde_json::from_value(
                    response
                        .pointer("/data/repository/object/statusCheckRollup/contexts")
                        .cloned()
                        .ok_or_else(|| {
                            "GitHub did not return the requested check page".to_owned()
                        })?,
                )
                .map_err(|err| format!("failed to parse GitHub check page: {err}"))?;
                pull.next_cursor = next_cursor(&page.page_info)?;
                pull.metadata.status_check_rollup.extend(page.nodes);
            }
            pulls.push(pull.metadata);
        }
    }
    Ok(pulls)
}

#[cfg(test)]
mod tests;
