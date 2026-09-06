use super::*;
use serde_json::json;

fn fixture() -> Value {
    json!({"data": {"repository": {"pr42": {
        "number": 42, "title": "PR", "url": "https://github.com/a/b/pull/42",
        "author": null, "isDraft": false, "state": "OPEN", "updatedAt": "2026-09-06T00:00:00Z",
        "headRefOid": "head", "commits": {"nodes": [{"commit": {
            "oid": "head", "statusCheckRollup": {"contexts": {
                "nodes": [
                    {"__typename": "CheckRun", "name": "Build", "status": "COMPLETED", "conclusion": "SUCCESS"},
                    {"__typename": "StatusContext", "context": "CI", "state": "FAILURE"}
                ],
                "pageInfo": {"hasNextPage": true, "endCursor": "page1"}
            }}
        }}]}
    }}}})
}

#[test]
fn parses_checks_and_keeps_pagination_cursor() {
    let pulls = parse_batch(&serde_json::to_vec(&fixture()).unwrap(), &[42]).unwrap();
    assert_eq!(pulls[0].metadata.number, 42);
    assert!(pulls[0].metadata.author.is_none());
    assert_eq!(pulls[0].next_cursor.as_deref(), Some("page1"));
    let summary = super::super::summarize_checks(&pulls[0].metadata.status_check_rollup);
    assert_eq!(summary.total, 2);
    assert_eq!(summary.failed, 1);
    assert_eq!(summary.failing_checks, ["CI"]);
}

#[test]
fn incomplete_batches_are_not_interpreted_as_closed_prs() {
    for response in [
        json!({"data": {"repository": {"pr42": null}}}),
        json!({"data": {"repository": {}}}),
        json!({"data": {"repository": null}}),
        json!({"errors": [{"message": "denied"}], "data": fixture()["data"]}),
    ] {
        assert!(parse_batch(&serde_json::to_vec(&response).unwrap(), &[42]).is_err());
    }
    let mut response = fixture();
    response["data"]["repository"]["pr42"]["number"] = json!(43);
    assert!(parse_batch(&serde_json::to_vec(&response).unwrap(), &[42]).is_err());
}

#[test]
fn head_mismatch_and_missing_cursor_are_errors() {
    let mut response = fixture();
    response["data"]["repository"]["pr42"]["headRefOid"] = json!("new-head");
    assert!(parse_batch(&serde_json::to_vec(&response).unwrap(), &[42]).is_err());
    assert!(next_cursor(&PageInfo {
        has_next_page: true,
        end_cursor: None
    })
    .is_err());
    assert!(next_cursor(&PageInfo {
        has_next_page: true,
        end_cursor: Some(String::new())
    })
    .is_err());
    assert_eq!(
        next_cursor(&PageInfo {
            has_next_page: false,
            end_cursor: Some("last".to_owned())
        })
        .unwrap(),
        None
    );
}

#[test]
fn closed_and_merged_prs_without_checks_are_preserved() {
    for state in ["CLOSED", "MERGED"] {
        let mut response = fixture();
        let pr = &mut response["data"]["repository"]["pr42"];
        pr["state"] = json!(state);
        pr["commits"]["nodes"][0]["commit"]["statusCheckRollup"] = Value::Null;
        let pulls = parse_batch(&serde_json::to_vec(&response).unwrap(), &[42]).unwrap();
        assert_eq!(pulls[0].metadata.state, state);
        assert!(pulls[0].metadata.status_check_rollup.is_empty());
        assert!(pulls[0].next_cursor.is_none());
    }
}

fn binding() -> GithubRepositoryBinding {
    GithubRepositoryBinding {
        channel_id: uuid::Uuid::nil(),
        repository_id: "repo".to_owned(),
        name_with_owner: "a/b".to_owned(),
        url: String::new(),
        local_path: String::new(),
        account_login: "owner".to_owned(),
        review_login: "owner".to_owned(),
        review_queue_synced_at: None,
        issue_queue_synced_at: None,
        created_at: String::new(),
        updated_at: String::new(),
    }
}

#[tokio::test]
async fn loads_later_check_pages_from_the_original_head() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = AtomicUsize::new(0);
    let pulls = load_linked_pull_requests_with(&binding(), &[42], |args| {
        let call = calls.fetch_add(1, Ordering::SeqCst);
        async move {
            let response = match call {
                0 => fixture(),
                1 => {
                    assert!(args.contains(&"head=head".to_owned()));
                    assert!(args.contains(&"cursor=page1".to_owned()));
                    json!({"data": {"repository": {"object": {"statusCheckRollup": {"contexts": {
                        "nodes": [{"__typename": "StatusContext", "context": "Slow CI", "state": "PENDING"}],
                        "pageInfo": {"hasNextPage": false, "endCursor": "page2"}
                    }}}}}})
                },
                _ => panic!("unexpected extra request"),
            };
            Ok(serde_json::to_vec(&response).unwrap())
        }
    }).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let summary = super::super::summarize_checks(&pulls[0].status_check_rollup);
    assert_eq!(summary.total, 3);
    assert_eq!(summary.pending, 1);
    assert_eq!(summary.failed, 1);
}

#[tokio::test]
async fn large_link_sets_are_batched_without_losing_prs() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = AtomicUsize::new(0);
    let pulls = load_linked_pull_requests_with(&binding(), &(1..=25).collect::<Vec<_>>(), |args| {
        let call = calls.fetch_add(1, Ordering::SeqCst);
        async move {
            let numbers = match call {
                0 => 1..=20,
                1 => 21..=25,
                _ => panic!("too many requests"),
            };
            let query = args.iter().find(|arg| arg.starts_with("query=")).unwrap();
            assert_eq!(
                query.matches(": pullRequest(").count(),
                numbers.clone().count()
            );
            let mut entries = serde_json::Map::new();
            for number in numbers {
                let mut pr = fixture()["data"]["repository"]["pr42"].clone();
                pr["number"] = json!(number);
                pr["commits"]["nodes"][0]["commit"]["statusCheckRollup"] = Value::Null;
                entries.insert(format!("pr{number}"), pr);
            }
            Ok(serde_json::to_vec(&json!({"data": {"repository": entries}})).unwrap())
        }
    })
    .await
    .unwrap();
    assert_eq!(
        pulls.iter().map(|pull| pull.number).collect::<Vec<_>>(),
        (1..=25).collect::<Vec<_>>()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
