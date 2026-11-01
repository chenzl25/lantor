use std::{
    future::{ready, Ready},
    sync::{Arc, Mutex},
};

use super::*;
use crate::test_support::{drop_test_schema, test_pool};

const MERGED_AND_MISSING: &str = r#"{
    "data": {"repository": {
        "n27251": {"__typename": "PullRequest", "number": 27251, "title": "fix(stream): wrap",
            "url": "https://github.com/org/repo/pull/27251", "state": "MERGED", "isDraft": false,
            "updatedAt": "2026-10-13T09:46:10Z",
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "PENDING"}}}]}},
        "n27429": {"__typename": "PullRequest", "number": 27429, "title": "fix(sum): keep NULL",
            "url": "https://github.com/org/repo/pull/27429", "state": "OPEN", "isDraft": true,
            "updatedAt": "2026-10-14T00:00:00Z",
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "FAILURE"}}}]}},
        "n12": {"__typename": "Issue", "number": 12, "title": "Crash on start",
            "url": "https://github.com/org/repo/issues/12", "state": "CLOSED",
            "stateReason": "NOT_PLANNED", "updatedAt": "2026-10-01T00:00:00Z"},
        "n99999": null
    }},
    "errors": [{"type": "NOT_FOUND", "path": ["repository", "n99999"], "message": "Could not resolve"}]
}"#;

type Calls = Arc<Mutex<Vec<Vec<String>>>>;
type Response = Ready<CommandResult<Vec<u8>>>;

/// A fake `gh` that records its arguments and replays `responses` in order.
fn runner(
    responses: Vec<CommandResult<&'static str>>,
) -> (Calls, impl Fn(Vec<String>) -> Response) {
    let calls: Calls = Arc::default();
    let responses = Arc::new(Mutex::new(responses.into_iter()));
    let recorded = calls.clone();
    let run = move |args: Vec<String>| {
        recorded.lock().unwrap().push(args);
        let response = responses
            .lock()
            .unwrap()
            .next()
            .expect("unexpected GitHub request");
        ready(response.map(|body| body.as_bytes().to_vec()))
    };
    (calls, run)
}

fn key(name_with_owner: &str, number: i64) -> GithubReferenceKey {
    GithubReferenceKey {
        name_with_owner: name_with_owner.to_owned(),
        number,
    }
}

fn at(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .unwrap()
        .with_timezone(&Utc)
}

fn requested_numbers(args: &[String]) -> Vec<i64> {
    let query = args
        .iter()
        .find_map(|arg| arg.strip_prefix("query="))
        .unwrap();
    query
        .split("issueOrPullRequest(number: ")
        .skip(1)
        .map(|rest| rest.split(')').next().unwrap().parse().unwrap())
        .collect()
}

#[tokio::test]
async fn lookup_fetches_once_and_caches_by_state() {
    let (pool, schema) = test_pool().await.expect("create test database");
    let references = vec![
        key("Org/Repo", 27251),
        key("org/repo", 27429),
        key("org/repo", 12),
        key("org/repo", 99999),
        key("org/repo", 27251),
    ];
    let (calls, run) = runner(vec![Ok(MERGED_AND_MISSING)]);
    let lookup = lookup_with(&pool, references.clone(), at("2026-10-14T10:00:00Z"), &run)
        .await
        .unwrap();
    assert_eq!(lookup.error, None);
    {
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].contains(&"owner=Org".to_owned()));
        assert!(calls[0].contains(&"name=Repo".to_owned()));
        assert_eq!(requested_numbers(&calls[0]), vec![27251, 27429, 12, 99999]);
    }
    let summary: Vec<_> = lookup
        .items
        .iter()
        .map(|item| {
            (
                item.number,
                item.kind,
                item.state.as_str(),
                item.state_reason.as_str(),
                item.is_draft,
                item.checks_status.as_str(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (12, "issue", "closed", "not_planned", false, "none"),
            // Merged PRs drop their stale CI state.
            (27251, "pull", "merged", "", false, "none"),
            (27429, "pull", "open", "", true, "failure"),
        ]
    );

    // Five minutes later everything is fresh, including the missing number.
    let (calls, run) = runner(Vec::new());
    let lookup = lookup_with(&pool, references.clone(), at("2026-10-14T10:05:00Z"), &run)
        .await
        .unwrap();
    assert_eq!(lookup.items.len(), 3);
    assert!(calls.lock().unwrap().is_empty());

    // After ten minutes only the open PR is refreshed; a day later the closed
    // issue and the missing number are too, but the merged PR never is.
    let (calls, run) = runner(vec![Ok(r#"{"data": {"repository": {
        "n27429": {"__typename": "PullRequest", "number": 27429, "title": "fix(sum): keep NULL",
            "url": "https://github.com/org/repo/pull/27429", "state": "MERGED", "isDraft": false,
            "updatedAt": "2026-10-14T10:20:00Z", "commits": {"nodes": []}}
    }}}"#)]);
    let lookup = lookup_with(&pool, references.clone(), at("2026-10-14T10:20:00Z"), &run)
        .await
        .unwrap();
    assert_eq!(requested_numbers(&calls.lock().unwrap()[0]), vec![27429]);
    let merged = lookup
        .items
        .iter()
        .find(|item| item.number == 27429)
        .unwrap();
    assert_eq!(
        (merged.state.as_str(), merged.checks_status.as_str()),
        ("merged", "none")
    );

    let (calls, run) = runner(vec![Err("offline".to_owned())]);
    let lookup = lookup_with(&pool, references, at("2026-10-16T00:00:00Z"), &run)
        .await
        .unwrap();
    assert_eq!(
        requested_numbers(&calls.lock().unwrap()[0]),
        vec![12, 99999]
    );
    // GitHub failed, so the stale closed issue is served from the cache.
    assert_eq!(
        lookup
            .items
            .iter()
            .map(|item| item.number)
            .collect::<Vec<_>>(),
        vec![12, 27251, 27429]
    );
    assert_eq!(lookup.error.as_deref(), Some("org/repo: offline"));

    drop_test_schema(pool, schema).await;
}

#[tokio::test]
async fn lookup_handles_missing_repositories_and_partial_failures() {
    let (pool, schema) = test_pool().await.expect("create test database");
    let (calls, run) = runner(vec![
        Ok(
            r#"{"data": {"repository": null}, "errors": [{"type": "NOT_FOUND", "path": ["repository"], "message": "Could not resolve to a Repository"}]}"#,
        ),
        Ok(
            r#"{"data": {"repository": {"n1": null, "n2": {"__typename": "Discussion", "number": 2}}},
            "errors": [{"type": "FORBIDDEN", "path": ["repository", "n1"], "message": "Resource not accessible"}]}"#,
        ),
    ]);
    let lookup = lookup_with(
        &pool,
        vec![
            key("org/gone", 7),
            key("org/private", 1),
            key("org/private", 2),
        ],
        at("2026-10-14T10:00:00Z"),
        &run,
    )
    .await
    .unwrap();
    assert_eq!(calls.lock().unwrap().len(), 2);
    assert!(lookup.items.is_empty());
    assert_eq!(
        lookup.error.as_deref(),
        Some("org/private: GitHub did not return some references")
    );

    // The missing repository is cached as missing; failed numbers are retried.
    let (calls, run) = runner(vec![Ok(r#"{"data": {"repository": {}}}"#)]);
    lookup_with(
        &pool,
        vec![key("org/gone", 7), key("org/private", 1)],
        at("2026-10-14T10:01:00Z"),
        &run,
    )
    .await
    .unwrap();
    assert_eq!(requested_numbers(&calls.lock().unwrap()[0]), vec![1]);

    drop_test_schema(pool, schema).await;
}

#[test]
fn references_are_validated_deduplicated_and_capped() {
    let mut references = vec![
        key("org/repo", 0),
        key("org/repo", i64::from(i32::MAX) + 1),
        key("org/repo/extra", 1),
        key("-org/repo", 1),
        key("org/re po", 1),
        key("org", 1),
        key(" Org/Repo.rs ", 5),
        key("org/repo.rs", 5),
    ];
    references.extend((1..=MAX_LOOKUP_REFERENCES as i64 + 5).map(|number| key("a/b", number)));
    let normalized = normalized_references(references);
    assert_eq!(
        normalized[0],
        ("org/repo.rs".to_owned(), "Org/Repo.rs".to_owned(), 5)
    );
    assert_eq!(normalized.len(), MAX_LOOKUP_REFERENCES);
}
