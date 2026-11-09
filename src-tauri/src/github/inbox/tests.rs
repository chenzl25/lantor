use std::{
    future::{ready, Ready},
    sync::{Arc, Mutex as StdMutex},
};

use serde_json::{json, Value};

use super::*;
use crate::test_support::{drop_test_schema, insert_test_channel, test_pool, ui_event_cursor};

/// Synthetic GitHub responses: the unread threads, the newest threads and the
/// account login. Records every request.
#[derive(Clone)]
struct FakeGithub {
    unread: Arc<StdMutex<Vec<Value>>>,
    recent: Arc<StdMutex<Vec<Value>>>,
    login: Arc<StdMutex<String>>,
    fail: Arc<StdMutex<Option<String>>>,
    calls: Arc<StdMutex<Vec<Vec<String>>>>,
}

impl FakeGithub {
    fn new() -> Self {
        Self {
            unread: Arc::default(),
            recent: Arc::default(),
            login: Arc::new(StdMutex::new("octo".to_owned())),
            fail: Arc::default(),
            calls: Arc::default(),
        }
    }

    fn set(&self, unread: Vec<Value>, recent: Vec<Value>) {
        *self.unread.lock().unwrap() = unread;
        *self.recent.lock().unwrap() = recent;
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().unwrap().clone()
    }

    fn runner(&self) -> impl Fn(Vec<String>) -> Ready<CommandResult<Vec<u8>>> + '_ {
        move |args: Vec<String>| {
            self.calls.lock().unwrap().push(args.clone());
            if let Some(error) = self.fail.lock().unwrap().clone() {
                return ready(Err(error));
            }
            let path = args.last().cloned().unwrap_or_default();
            let body = if path == "user" {
                json!({ "login": *self.login.lock().unwrap() })
            } else if path.starts_with("notifications?participating=true&all=true") {
                Value::Array(self.recent.lock().unwrap().clone())
            } else if path.starts_with("notifications?participating=true&per_page=") {
                if path.ends_with("&page=1") {
                    Value::Array(self.unread.lock().unwrap().clone())
                } else {
                    json!([])
                }
            } else if path.starts_with("notifications/threads/") {
                return ready(Ok(Vec::new()));
            } else {
                panic!("unexpected GitHub request: {args:?}");
            };
            ready(Ok(serde_json::to_vec(&body).unwrap()))
        }
    }
}

fn notification(id: &str, reason: &str, unread: bool, updated_at: &str) -> Value {
    json!({
        "id": id,
        "unread": unread,
        "reason": reason,
        "updated_at": updated_at,
        "last_read_at": null,
        "subject": {
            "title": format!("Change {id}"),
            "url": format!("https://api.github.com/repos/org/repo/pulls/{id}"),
            "latest_comment_url": null,
            "type": "PullRequest"
        },
        "repository": {
            "full_name": "org/repo",
            "html_url": "https://github.com/org/repo"
        }
    })
}

fn at(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .unwrap()
        .with_timezone(&Utc)
}

fn unread_ids(inbox: &GithubInbox) -> Vec<&str> {
    inbox
        .items
        .iter()
        .filter(|item| item.unread)
        .map(|item| item.thread_id.as_str())
        .collect()
}

fn ids(inbox: &GithubInbox) -> Vec<&str> {
    inbox
        .items
        .iter()
        .map(|item| item.thread_id.as_str())
        .collect()
}

const NOW: &str = "2026-11-09T03:00:00Z";

#[test]
fn subject_pages_point_at_the_latest_comment() {
    let mut pull = serde_json::from_value::<NotificationCli>(notification(
        "27861",
        "review_requested",
        true,
        NOW,
    ))
    .unwrap();
    assert_eq!(
        subject_page(&pull),
        (
            "https://github.com/org/repo/pull/27861".to_owned(),
            Some(27861)
        )
    );

    pull.subject.latest_comment_url =
        Some("https://api.github.com/repos/org/repo/issues/comments/555".to_owned());
    assert_eq!(
        subject_page(&pull).0,
        "https://github.com/org/repo/pull/27861#issuecomment-555"
    );
    pull.subject.latest_comment_url =
        Some("https://api.github.com/repos/org/repo/pulls/comments/777".to_owned());
    assert_eq!(
        subject_page(&pull).0,
        "https://github.com/org/repo/pull/27861#discussion_r777"
    );
    // GitHub names the subject itself when its latest event is not a comment.
    pull.subject.latest_comment_url =
        Some("https://api.github.com/repos/org/repo/pulls/27861".to_owned());
    assert_eq!(
        subject_page(&pull).0,
        "https://github.com/org/repo/pull/27861"
    );

    for (kind, url, page, number) in [
        (
            "Issue",
            Some("https://api.github.com/repos/org/repo/issues/12"),
            "https://github.com/org/repo/issues/12",
            Some(12),
        ),
        (
            "Release",
            Some("https://api.github.com/repos/org/repo/releases/99"),
            "https://github.com/org/repo/releases",
            None,
        ),
        (
            "Commit",
            Some("https://api.github.com/repos/org/repo/commits/abc123"),
            "https://github.com/org/repo/commit/abc123",
            None,
        ),
        (
            "RepositoryAdvisory",
            None,
            "https://github.com/org/repo/security",
            None,
        ),
        (
            "CheckSuite",
            None,
            "https://github.com/org/repo/actions",
            None,
        ),
    ] {
        let mut value = notification("1", "subscribed", true, NOW);
        value["subject"]["type"] = json!(kind);
        value["subject"]["url"] = json!(url);
        let parsed = serde_json::from_value::<NotificationCli>(value).unwrap();
        assert_eq!(subject_page(&parsed), (page.to_owned(), number), "{kind}");
    }
}

#[tokio::test]
async fn sync_keeps_unread_and_newest_threads_and_turns_the_inbox_on() {
    let (pool, schema) = test_pool().await.expect("create test database");
    assert!(!load_github_inbox(&pool).await.unwrap().enabled);
    assert!(!poll_due(&pool, at(NOW)).await.unwrap());

    let github = FakeGithub::new();
    github.set(
        vec![
            notification("3", "mention", true, "2026-11-09T02:00:00Z"),
            notification("1", "review_requested", true, "2026-11-01T00:00:00Z"),
        ],
        vec![
            notification("4", "author", false, "2026-11-09T02:30:00Z"),
            notification("3", "mention", true, "2026-11-09T02:00:00Z"),
        ],
    );
    sync_with(&pool, at(NOW), github.runner()).await.unwrap();

    let inbox = load_github_inbox(&pool).await.unwrap();
    assert!(inbox.enabled);
    assert_eq!(inbox.account_login, "octo");
    assert_eq!(
        inbox.synced_at.as_deref(),
        Some("2026-11-09T03:00:00+00:00")
    );
    assert_eq!(ids(&inbox), ["4", "3", "1"]);
    assert_eq!(unread_ids(&inbox), ["3", "1"]);
    assert_eq!(inbox.items[1].url, "https://github.com/org/repo/pull/3");
    assert_eq!(inbox.items[1].number, Some(3));

    // Read on GitHub, and an old read thread has dropped out of the newest ones.
    github.set(
        vec![notification(
            "1",
            "review_requested",
            true,
            "2026-11-01T00:00:00Z",
        )],
        vec![notification("3", "mention", false, "2026-11-09T02:00:00Z")],
    );
    sync_with(&pool, at("2026-11-09T03:01:00Z"), github.runner())
        .await
        .unwrap();
    let inbox = load_github_inbox(&pool).await.unwrap();
    assert_eq!(ids(&inbox), ["3", "1"]);
    assert_eq!(unread_ids(&inbox), ["1"]);
    // The account is checked once an hour, not on every sync.
    let account_checks = github
        .calls()
        .iter()
        .filter(|args| args.last().map(String::as_str) == Some("user"))
        .count();
    assert_eq!(account_checks, 1);
    drop_test_schema(pool, schema).await;
}

#[tokio::test]
async fn an_unchanged_sync_publishes_no_refresh() {
    let (pool, schema) = test_pool().await.expect("create test database");
    let github = FakeGithub::new();
    github.set(
        vec![notification("1", "mention", true, "2026-11-09T02:00:00Z")],
        vec![],
    );
    sync_with(&pool, at(NOW), github.runner()).await.unwrap();
    let cursor = ui_event_cursor(&pool).await.unwrap();
    sync_with(&pool, at("2026-11-09T03:01:00Z"), github.runner())
        .await
        .unwrap();
    assert_eq!(ui_event_cursor(&pool).await.unwrap(), cursor);

    github.set(
        vec![notification("1", "mention", true, "2026-11-09T02:59:00Z")],
        vec![],
    );
    sync_with(&pool, at("2026-11-09T03:02:00Z"), github.runner())
        .await
        .unwrap();
    assert!(ui_event_cursor(&pool).await.unwrap() > cursor);
    drop_test_schema(pool, schema).await;
}

#[tokio::test]
async fn marks_survive_an_older_sync_until_new_activity() {
    let (pool, schema) = test_pool().await.expect("create test database");
    let github = FakeGithub::new();
    let stale = vec![
        notification("1", "review_requested", true, "2026-11-09T01:00:00Z"),
        notification("2", "mention", true, "2026-11-09T02:00:00Z"),
    ];
    github.set(stale.clone(), vec![]);
    sync_with(&pool, at(NOW), github.runner()).await.unwrap();

    mark_with(&pool, "1", Mark::Read, github.runner())
        .await
        .unwrap();
    mark_with(&pool, "2", Mark::Done, github.runner())
        .await
        .unwrap();
    let writes: Vec<_> = github
        .calls()
        .into_iter()
        .filter(|args| args.iter().any(|arg| arg == "--method"))
        .collect();
    assert_eq!(
        writes,
        [
            ["api", "--method", "PATCH", "notifications/threads/1"],
            ["api", "--method", "DELETE", "notifications/threads/2"],
        ]
    );
    let inbox = load_github_inbox(&pool).await.unwrap();
    assert_eq!(ids(&inbox), ["1"]);
    assert!(unread_ids(&inbox).is_empty());

    // A sync read before GitHub took the writes still says both are unread.
    sync_with(&pool, at("2026-11-09T03:01:00Z"), github.runner())
        .await
        .unwrap();
    let inbox = load_github_inbox(&pool).await.unwrap();
    assert_eq!(ids(&inbox), ["1"]);
    assert!(unread_ids(&inbox).is_empty());

    // New activity brings both back, as GitHub does.
    github.set(
        vec![
            notification("1", "review_requested", true, "2026-11-09T03:05:00Z"),
            notification("2", "mention", true, "2026-11-09T03:06:00Z"),
        ],
        vec![],
    );
    sync_with(&pool, at("2026-11-09T03:07:00Z"), github.runner())
        .await
        .unwrap();
    let inbox = load_github_inbox(&pool).await.unwrap();
    assert_eq!(unread_ids(&inbox), ["2", "1"]);
    drop_test_schema(pool, schema).await;
}

#[tokio::test]
async fn marks_are_checked_before_writing_to_github() {
    let (pool, schema) = test_pool().await.expect("create test database");
    let github = FakeGithub::new();
    github.set(vec![notification("1", "mention", true, NOW)], vec![]);
    sync_with(&pool, at(NOW), github.runner()).await.unwrap();
    let before = github.calls().len();

    for thread_id in ["../user", "1/subscription", ""] {
        assert!(mark_with(&pool, thread_id, Mark::Done, github.runner())
            .await
            .is_err());
    }
    assert!(mark_with(&pool, "9", Mark::Read, github.runner())
        .await
        .is_err());
    *github.fail.lock().unwrap() = Some("HTTP 403".to_owned());
    assert!(mark_with(&pool, "1", Mark::Read, github.runner())
        .await
        .is_err());
    // Only the failed write reached GitHub, and the thread is still unread.
    assert_eq!(github.calls().len(), before + 1);
    assert_eq!(unread_ids(&load_github_inbox(&pool).await.unwrap()), ["1"]);
    drop_test_schema(pool, schema).await;
}

#[tokio::test]
async fn failures_are_shown_and_retried_later() {
    let (pool, schema) = test_pool().await.expect("create test database");
    let github = FakeGithub::new();
    github.set(vec![notification("1", "mention", true, NOW)], vec![]);
    sync_with(&pool, at(NOW), github.runner()).await.unwrap();
    assert!(!poll_due(&pool, at("2026-11-09T03:00:59Z")).await.unwrap());
    assert!(poll_due(&pool, at("2026-11-09T03:01:00Z")).await.unwrap());

    *github.fail.lock().unwrap() = Some("GitHub CLI request failed: HTTP 401".to_owned());
    let cursor = ui_event_cursor(&pool).await.unwrap();
    assert!(
        sync_with(&pool, at("2026-11-09T03:01:00Z"), github.runner())
            .await
            .is_err()
    );
    assert!(ui_event_cursor(&pool).await.unwrap() > cursor);
    let inbox = load_github_inbox(&pool).await.unwrap();
    assert_eq!(
        inbox.error.as_deref(),
        Some("GitHub CLI request failed: HTTP 401")
    );
    // The cached threads stay while GitHub cannot be read.
    assert_eq!(ids(&inbox), ["1"]);
    assert!(!poll_due(&pool, at("2026-11-09T03:05:59Z")).await.unwrap());
    assert!(poll_due(&pool, at("2026-11-09T03:06:00Z")).await.unwrap());

    *github.fail.lock().unwrap() = None;
    sync_with(&pool, at("2026-11-09T03:06:00Z"), github.runner())
        .await
        .unwrap();
    assert!(load_github_inbox(&pool).await.unwrap().error.is_none());
    drop_test_schema(pool, schema).await;
}

#[tokio::test]
async fn another_account_replaces_the_threads() {
    let (pool, schema) = test_pool().await.expect("create test database");
    let github = FakeGithub::new();
    github.set(vec![notification("1", "mention", true, NOW)], vec![]);
    sync_with(&pool, at(NOW), github.runner()).await.unwrap();
    mark_with(&pool, "1", Mark::Done, github.runner())
        .await
        .unwrap();

    *github.login.lock().unwrap() = "other".to_owned();
    github.set(vec![notification("1", "mention", true, NOW)], vec![]);
    // An hour later the account is checked again.
    sync_with(&pool, at("2026-11-09T04:00:00Z"), github.runner())
        .await
        .unwrap();
    let inbox = load_github_inbox(&pool).await.unwrap();
    assert_eq!(inbox.account_login, "other");
    // The other account's thread 1 is not hidden by the first one's mark.
    assert_eq!(unread_ids(&inbox), ["1"]);
    drop_test_schema(pool, schema).await;
}

#[tokio::test]
async fn threads_link_to_their_lantor_task_thread() {
    let (pool, schema) = test_pool().await.expect("create test database");
    let channel_id = insert_test_channel(&pool, "repo").await.unwrap();
    let root_id: Uuid = sqlx::query_scalar(
        "insert into messages (channel_id, sender_name, body) values ($1, 'Me', 'Review') returning id",
    )
    .bind(channel_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        insert into channel_github_repositories (
            channel_id, repository_id, name_with_owner, url, account_login, review_login
        )
        values ($1, 'R_1', 'Org/Repo', 'https://github.com/Org/Repo', 'octo', 'octo')
        "#,
    )
    .bind(channel_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"
        insert into github_resource_threads (
            channel_id, repository_id, resource_kind, resource_number, resource_url,
            thread_root_id
        )
        values ($1, 'R_1', 'pull_request', 7, 'https://github.com/Org/Repo/pull/7', $2)
        "#,
    )
    .bind(channel_id)
    .bind(root_id)
    .execute(&pool)
    .await
    .unwrap();

    let github = FakeGithub::new();
    let mut issue = notification("8", "mention", true, NOW);
    issue["subject"]["type"] = json!("Issue");
    issue["subject"]["url"] = json!("https://api.github.com/repos/org/repo/issues/7");
    github.set(
        vec![notification("7", "review_requested", true, NOW), issue],
        vec![],
    );
    sync_with(&pool, at(NOW), github.runner()).await.unwrap();
    let inbox = load_github_inbox(&pool).await.unwrap();
    let linked: Vec<_> = inbox
        .items
        .iter()
        .map(|item| (item.thread_id.as_str(), item.thread_root_id))
        .collect();
    // Issue 7 is not the reviewed pull request 7.
    assert_eq!(linked, [("8", None), ("7", Some(root_id))]);
    drop_test_schema(pool, schema).await;
}
