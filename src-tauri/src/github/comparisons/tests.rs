use super::*;
use crate::test_support::{drop_test_schema, test_pool};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[tokio::test]
async fn reuses_only_successes_for_the_exact_repository_and_sha_pair() {
    let (pool, schema) = test_pool().await.expect("create test database");
    let calls = AtomicUsize::new(0);
    for (repo, base, head, expected) in [
        ("repo", "base", "head", 1),
        ("repo", "base", "head", 1),
        ("repo", "other-base", "head", 2),
        ("repo", "base", "other-head", 3),
        ("other-repo", "base", "head", 4),
    ] {
        let count = cached_or_fetch(&pool, repo, base, head, || async {
            Ok(calls.fetch_add(1, Ordering::SeqCst) as i64 + 1)
        })
        .await
        .unwrap();
        assert_eq!(count, expected);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    assert!(cached_or_fetch(&pool, "repo", "base", "retry", || async {
        Err("unavailable".to_owned())
    })
    .await
    .is_err());
    assert_eq!(
        cached_or_fetch(&pool, "repo", "base", "retry", || async { Ok(5) })
            .await
            .unwrap(),
        5
    );
    assert_eq!(
        cached_or_fetch(&pool, "repo", "head", "head", || async {
            panic!("identical heads need no network")
        })
        .await
        .unwrap(),
        0
    );
    assert!(
        cached_or_fetch(&pool, "repo", "", "head", || async { Ok(0) })
            .await
            .is_err()
    );
    drop_test_schema(pool, schema).await;
}

#[tokio::test]
async fn comparisons_run_concurrently_with_a_limit_and_keep_partial_successes() {
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(tokio::sync::Barrier::new(COMPARISON_CONCURRENCY));
    let targets = (0..8)
        .map(|number| ComparisonTarget {
            pull_number: number,
            base_sha: "base".to_owned(),
            head_sha: format!("head-{number}"),
        })
        .collect();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        resolve_comparisons(targets, {
            let active = active.clone();
            let peak = peak.clone();
            move |target| {
                let active = active.clone();
                let peak = peak.clone();
                let barrier = barrier.clone();
                async move {
                    let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(count, Ordering::SeqCst);
                    barrier.wait().await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    if target.pull_number == 2 {
                        Err("unavailable".to_owned())
                    } else {
                        Ok(target.pull_number)
                    }
                }
            }
        }),
    )
    .await
    .expect("bounded concurrency must make progress")
    .unwrap();
    assert_eq!(peak.load(Ordering::SeqCst), COMPARISON_CONCURRENCY);
    assert_eq!(active.load(Ordering::SeqCst), 0);
    assert_eq!(result.len(), 7);
    assert!(!result.iter().any(|item| item.pull_number == 2));
    assert!(result
        .iter()
        .all(|item| item.commits_ahead == item.pull_number));
}

#[tokio::test]
async fn cached_list_counts_follow_the_current_review_anchor_and_head() {
    use crate::github::{GithubCheckSummary, GithubPullRequestCli};
    let (pool, schema) = test_pool().await.expect("create test database");
    let binding = GithubRepositoryBinding {
        channel_id: Uuid::nil(),
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
    };
    let mut requests = vec![GithubPullRequestSnapshot {
        pull_request: GithubPullRequestCli {
            number: 42,
            title: "PR".to_owned(),
            url: String::new(),
            author: None,
            is_draft: false,
            state: "OPEN".to_owned(),
            updated_at: String::new(),
            head_ref_oid: "head".to_owned(),
            status_check_rollup: Vec::new(),
        },
        checks: GithubCheckSummary::default(),
        is_review_requested: true,
        is_authored: false,
    }];
    let mut links = HashMap::from([(
        42,
        GithubResourceLink {
            thread_root_id: Uuid::nil(),
            task_id: None,
            task_number: None,
            task_status: Some("done".to_owned()),
            assignee_id: None,
            assignee_name: None,
            head_sha: "base".to_owned(),
        },
    )]);
    assert!(
        cached_counts_for_requests(&pool, &binding, &requests, &links)
            .await
            .unwrap()
            .is_empty()
    );
    cached_or_fetch(&pool, "repo", "base", "head", || async { Ok(3) })
        .await
        .unwrap();
    assert_eq!(
        cached_counts_for_requests(&pool, &binding, &requests, &links)
            .await
            .unwrap()
            .get(&42),
        Some(&3)
    );
    requests[0].pull_request.head_ref_oid = "new-head".to_owned();
    assert!(
        cached_counts_for_requests(&pool, &binding, &requests, &links)
            .await
            .unwrap()
            .is_empty()
    );
    requests[0].pull_request.head_ref_oid = "head".to_owned();
    links.get_mut(&42).unwrap().head_sha = "new-base".to_owned();
    assert!(
        cached_counts_for_requests(&pool, &binding, &requests, &links)
            .await
            .unwrap()
            .is_empty()
    );
    links.get_mut(&42).unwrap().head_sha = "head".to_owned();
    assert!(comparison_targets(&requests, &links).is_empty());
    drop_test_schema(pool, schema).await;
}
