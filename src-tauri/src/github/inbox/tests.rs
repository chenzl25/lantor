use std::{
    future::{ready, Ready},
    sync::{Arc, Mutex as StdMutex},
};

use serde_json::{json, Value};

use super::*;
use crate::test_support::{drop_test_schema, insert_test_channel, test_pool, ui_event_cursor};

/// Synthetic GitHub responses: the participating threads, newest first, read
/// 50 a page whatever `per_page` asks (as GitHub does), and the account
/// login. Records every request.
#[derive(Clone)]
struct FakeGithub {
    threads: Arc<StdMutex<Vec<Value>>>,
    login: Arc<StdMutex<String>>,
    fail: Arc<StdMutex<Option<String>>>,
    /// A request path that fails while the others succeed.
    fail_path: Arc<StdMutex<Option<String>>>,
    calls: Arc<StdMutex<Vec<Vec<String>>>>,
}

impl FakeGithub {
    fn new() -> Self {
        Self {
            threads: Arc::default(),
            login: Arc::new(StdMutex::new("octo".to_owned())),
            fail: Arc::default(),
            fail_path: Arc::default(),
            calls: Arc::default(),
        }
    }

    fn set(&self, threads: Vec<Value>) {
        *self.threads.lock().unwrap() = threads;
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
            if self.fail_path.lock().unwrap().as_deref() == Some(path.as_str()) {
                return ready(Err(format!("{path} failed")));
            }
            let body = if path == "user" {
                json!({ "login": *self.login.lock().unwrap() })
            } else if let Some(query) =
                path.strip_prefix("notifications?participating=true&all=true&")
            {
                let param = |name: &str| {
                    query
                        .split('&')
                        .find_map(|pair| pair.strip_prefix(&format!("{name}=")))
                        .and_then(|value| value.parse::<usize>().ok())
                };
                let per_page = param("per_page").unwrap_or(50).min(50);
                let page = param("page").unwrap_or(1);
                let threads = self.threads.lock().unwrap();
                let start = (page - 1) * per_page;
                Value::Array(threads.iter().skip(start).take(per_page).cloned().collect())
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
async fn sync_lists_participating_threads_and_turns_the_inbox_on() {
    let (pool, schema) = test_pool().await.expect("create test database");
    assert!(!load_github_inbox(&pool).await.unwrap().enabled);
    assert!(!poll_due(&pool, at(NOW)).await.unwrap());

    let github = FakeGithub::new();
    github.set(vec![
        notification("4", "author", false, "2026-11-09T02:30:00Z"),
        notification("3", "mention", true, "2026-11-09T02:00:00Z"),
        notification("1", "review_requested", true, "2026-11-01T00:00:00Z"),
    ]);
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

    // 3 was read on GitHub and stays listed; 4 was marked done there.
    github.set(vec![
        notification("3", "mention", false, "2026-11-09T02:00:00Z"),
        notification("1", "review_requested", true, "2026-11-01T00:00:00Z"),
    ]);
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

/// `count` threads numbered from `first`, newest first, one a minute back from
/// `newest`; the first `unread` of them unread.
fn threads(first: usize, count: usize, unread: usize, newest: &str) -> Vec<Value> {
    (0..count)
        .map(|index| {
            let updated_at = (at(newest) - chrono::Duration::minutes(index as i64))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            notification(
                &(first + index).to_string(),
                "mention",
                index < unread,
                &updated_at,
            )
        })
        .collect()
}

#[tokio::test]
async fn read_threads_stay_listed_past_the_first_page_until_done() {
    let (pool, schema) = test_pool().await.expect("create test database");
    let github = FakeGithub::new();
    // More unread threads than one page holds, then older read ones.
    github.set(threads(1, 120, 70, "2026-11-09T02:00:00Z"));
    sync_with(&pool, at(NOW), github.runner()).await.unwrap();
    let inbox = load_github_inbox(&pool).await.unwrap();
    assert_eq!(inbox.items.len(), 120);
    assert_eq!(unread_ids(&inbox).len(), 70);

    // The oldest thread, read in Lantor, is still listed by the next sync.
    mark_with(&pool, "120", Mark::Read, github.runner())
        .await
        .unwrap();
    sync_with(&pool, at("2026-11-09T03:01:00Z"), github.runner())
        .await
        .unwrap();
    let inbox = load_github_inbox(&pool).await.unwrap();
    assert_eq!(inbox.items.len(), 120);
    assert!(inbox
        .items
        .iter()
        .any(|item| item.thread_id == "120" && !item.unread));
    drop_test_schema(pool, schema).await;
}

#[tokio::test]
async fn threads_past_the_page_limit_are_kept_until_github_lists_them_again() {
    let (pool, schema) = test_pool().await.expect("create test database");
    let github = FakeGithub::new();
    let old = threads(5001, 10, 0, "2026-10-01T00:00:00Z");
    let mut first = vec![notification(
        "9000",
        "mention",
        false,
        "2026-11-09T01:00:00Z",
    )];
    first.extend(old.clone());
    github.set(first);
    sync_with(&pool, at(NOW), github.runner()).await.unwrap();

    // A thousand newer threads fill every page the sync reads: the old ones
    // are past its end and stay; 9000, within it, was done on GitHub.
    let mut second = threads(1, PAGE_SIZE * PAGE_LIMIT, 0, "2026-11-09T02:00:00Z");
    second.extend(old);
    github.set(second);
    sync_with(&pool, at("2026-11-09T03:01:00Z"), github.runner())
        .await
        .unwrap();
    let inbox = load_github_inbox(&pool).await.unwrap();
    assert_eq!(inbox.items.len(), PAGE_SIZE * PAGE_LIMIT + 10);
    assert!(inbox.items.iter().any(|item| item.thread_id == "5010"));
    assert!(!inbox.items.iter().any(|item| item.thread_id == "9000"));
    drop_test_schema(pool, schema).await;
}

#[tokio::test]
async fn an_unchanged_sync_publishes_no_refresh() {
    let (pool, schema) = test_pool().await.expect("create test database");
    let github = FakeGithub::new();
    github.set(vec![notification(
        "1",
        "mention",
        true,
        "2026-11-09T02:00:00Z",
    )]);
    sync_with(&pool, at(NOW), github.runner()).await.unwrap();
    let cursor = ui_event_cursor(&pool).await.unwrap();
    sync_with(&pool, at("2026-11-09T03:01:00Z"), github.runner())
        .await
        .unwrap();
    assert_eq!(ui_event_cursor(&pool).await.unwrap(), cursor);

    github.set(vec![notification(
        "1",
        "mention",
        true,
        "2026-11-09T02:59:00Z",
    )]);
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
    github.set(stale.clone());
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
    github.set(vec![
        notification("1", "review_requested", true, "2026-11-09T03:05:00Z"),
        notification("2", "mention", true, "2026-11-09T03:06:00Z"),
    ]);
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
    github.set(vec![notification("1", "mention", true, NOW)]);
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
    github.set(vec![notification("1", "mention", true, NOW)]);
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
    github.set(vec![notification("1", "mention", true, NOW)]);
    sync_with(&pool, at(NOW), github.runner()).await.unwrap();
    mark_with(&pool, "1", Mark::Done, github.runner())
        .await
        .unwrap();

    *github.login.lock().unwrap() = "other".to_owned();
    github.set(vec![notification("1", "mention", true, NOW)]);
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
    github.set(vec![
        notification("7", "review_requested", true, NOW),
        issue,
    ]);
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

/// A notification about `kind` (`PullRequest`, `Issue`, `Release`) number `id`
/// in `repository`.
fn subject(id: &str, repository: &str, kind: &str, unread: bool, updated_at: &str) -> Value {
    let mut value = notification(id, "comment", unread, updated_at);
    let path = match kind {
        "PullRequest" => format!("pulls/{id}"),
        "Issue" => format!("issues/{id}"),
        _ => format!("releases/{id}"),
    };
    value["subject"]["type"] = json!(kind);
    value["subject"]["url"] = json!(format!("https://api.github.com/repos/{repository}/{path}"));
    value["repository"] = json!({
        "full_name": repository,
        "html_url": format!("https://github.com/{repository}"),
    });
    value
}

async fn bind(pool: &SqlitePool, name: &str, repository: &str, review_login: &str) -> Uuid {
    let channel_id = insert_test_channel(pool, name).await.unwrap();
    sqlx::query(
        r#"
        insert into channel_github_repositories (
            channel_id, repository_id, name_with_owner, url, account_login, review_login
        )
        values ($1, $2, $3, $4, 'octo', $5)
        "#,
    )
    .bind(channel_id)
    .bind(format!("R_{name}"))
    .bind(repository)
    .bind(format!("https://github.com/{repository}"))
    .bind(review_login)
    .execute(pool)
    .await
    .unwrap();
    channel_id
}

fn activity(entries: &[(&str, bool, bool)]) -> InboxActivity {
    entries
        .iter()
        .map(|(repository, pull_requests, issues)| {
            (
                (*repository).to_owned(),
                RepositoryActivity {
                    pull_requests: *pull_requests,
                    issues: *issues,
                },
            )
        })
        .collect()
}

#[tokio::test]
async fn a_sync_names_repositories_with_new_pull_request_or_issue_activity() {
    let (pool, schema) = test_pool().await.expect("create test database");
    let github = FakeGithub::new();
    github.set(vec![
        subject("1", "Org/Repo", "PullRequest", true, NOW),
        subject("2", "Org/Repo", "Issue", true, NOW),
        subject("3", "org/docs", "Release", true, NOW),
        subject("4", "org/other", "Issue", false, NOW),
    ]);
    let first = sync_with(&pool, at(NOW), github.runner()).await.unwrap();
    // Every thread is new on the first sync; releases do not change a queue.
    assert_eq!(
        first,
        activity(&[("org/repo", true, true), ("org/other", false, true)])
    );

    let unchanged = sync_with(&pool, at(NOW), github.runner()).await.unwrap();
    assert_eq!(unchanged, InboxActivity::new());

    let later = "2026-11-09T03:05:00Z";
    github.set(vec![
        subject("1", "Org/Repo", "PullRequest", true, later),
        subject("2", "Org/Repo", "Issue", true, NOW),
        subject("3", "org/docs", "Release", true, later),
        subject("4", "org/other", "Issue", false, NOW),
    ]);
    let next = sync_with(&pool, at(later), github.runner()).await.unwrap();
    assert_eq!(next, activity(&[("org/repo", true, false)]));
    drop_test_schema(pool, schema).await;
}

#[tokio::test]
async fn bound_channel_queues_refresh_at_most_every_five_minutes() {
    let (pool, schema) = test_pool().await.expect("create test database");
    let repo = bind(&pool, "repo", "Org/Repo", "octo").await;
    let other = bind(&pool, "other", "org/other", "octo").await;
    sqlx::query(
        r#"
        update channel_github_repositories
        set review_queue_synced_at = '2026-11-09T02:58:00Z', issue_queue_synced_at = null
        where channel_id = $1
        "#,
    )
    .bind(repo)
    .execute(&pool)
    .await
    .unwrap();
    let active = activity(&[("org/repo", true, true), ("org/unbound", true, true)]);

    let due = channel_queues_due(&pool, &active, at(NOW)).await.unwrap();
    // Pull requests synced two minutes ago wait; issues were never synced.
    assert_eq!(due, [(repo, ChannelQueue::Issues)]);

    let due = channel_queues_due(&pool, &active, at("2026-11-09T03:03:00Z"))
        .await
        .unwrap();
    assert_eq!(
        due,
        [
            (repo, ChannelQueue::PullRequests),
            (repo, ChannelQueue::Issues)
        ]
    );

    let due = channel_queues_due(&pool, &activity(&[("org/other", true, false)]), at(NOW))
        .await
        .unwrap();
    assert_eq!(due, [(other, ChannelQueue::PullRequests)]);
    drop_test_schema(pool, schema).await;
}

#[tokio::test]
async fn items_name_the_channel_bound_to_their_repository() {
    let (pool, schema) = test_pool().await.expect("create test database");
    let _teammate = bind(&pool, "teammate", "org/repo", "someone").await;
    let mine = bind(&pool, "mine", "Org/Repo", "Octo").await;
    let github = FakeGithub::new();
    github.set(vec![
        subject("1", "org/repo", "PullRequest", true, NOW),
        subject("2", "org/unbound", "Issue", true, NOW),
    ]);
    sync_with(&pool, at(NOW), github.runner()).await.unwrap();
    let inbox = load_github_inbox(&pool).await.unwrap();
    let channels: Vec<_> = inbox
        .items
        .iter()
        .map(|item| (item.thread_id.as_str(), item.channel_id))
        .collect();
    // The channel that reviews as the inbox's account wins over an older one.
    assert_eq!(channels, [("2", None), ("1", Some(mine))]);
    drop_test_schema(pool, schema).await;
}

async fn channel_github_unread(pool: &SqlitePool, channel_id: Uuid) -> (i32, bool) {
    let channel = crate::channels::load_channels(pool)
        .await
        .unwrap()
        .into_iter()
        .find(|channel| channel.id == channel_id)
        .unwrap();
    (
        channel.github_unread_count,
        channel.github_unread_from_inbox,
    )
}

#[tokio::test]
async fn a_bound_channel_counts_unread_notifications_once_the_inbox_is_on() {
    let (pool, schema) = test_pool().await.expect("create test database");
    let mine = bind(&pool, "mine", "Org/Repo", "octo").await;
    let teammate = bind(&pool, "teammate", "org/repo", "someone").await;
    assert_eq!(channel_github_unread(&pool, mine).await, (0, false));

    let github = FakeGithub::new();
    github.set(vec![
        subject("1", "org/repo", "PullRequest", true, NOW),
        subject("2", "org/repo", "Issue", true, NOW),
        subject("3", "org/repo", "Release", true, NOW),
        subject("4", "org/other", "PullRequest", true, NOW),
        subject("5", "org/repo", "PullRequest", false, NOW),
    ]);
    sync_with(&pool, at(NOW), github.runner()).await.unwrap();
    // Unread pull requests and issues of the repository; not the release.
    assert_eq!(channel_github_unread(&pool, mine).await, (2, true));
    // Another login's queue keeps its own review-request count.
    assert_eq!(channel_github_unread(&pool, teammate).await, (0, false));

    mark_with(&pool, "1", Mark::Read, github.runner())
        .await
        .unwrap();
    assert_eq!(channel_github_unread(&pool, mine).await, (1, true));
    mark_with(&pool, "2", Mark::Done, github.runner())
        .await
        .unwrap();
    assert_eq!(channel_github_unread(&pool, mine).await, (0, true));
    drop_test_schema(pool, schema).await;
}

#[tokio::test]
async fn marking_a_repository_read_writes_each_unread_thread_to_github() {
    let (pool, schema) = test_pool().await.expect("create test database");
    let github = FakeGithub::new();
    github.set(vec![
        subject("1", "org/repo", "PullRequest", true, NOW),
        subject("2", "org/repo", "Issue", true, "2026-11-09T02:00:00Z"),
        subject("3", "org/repo", "Release", true, NOW),
        subject("4", "org/other", "PullRequest", true, NOW),
        subject("5", "org/repo", "PullRequest", false, NOW),
    ]);
    sync_with(&pool, at(NOW), github.runner()).await.unwrap();
    let cursor = ui_event_cursor(&pool).await.unwrap();
    let before = github.calls().len();

    mark_repository_read_with(&pool, "Org/Repo", github.runner())
        .await
        .unwrap();
    let writes: Vec<_> = github.calls()[before..]
        .iter()
        .map(|args| args.join(" "))
        .collect();
    assert_eq!(
        writes,
        [
            "api --method PATCH notifications/threads/1",
            "api --method PATCH notifications/threads/2",
        ]
    );
    let inbox = load_github_inbox(&pool).await.unwrap();
    assert_eq!(unread_ids(&inbox), ["4", "3"]);
    assert!(ui_event_cursor(&pool).await.unwrap() > cursor);

    // An older sync that still reports them unread does not undo the marks.
    sync_with(&pool, at(NOW), github.runner()).await.unwrap();
    let inbox = load_github_inbox(&pool).await.unwrap();
    assert_eq!(unread_ids(&inbox), ["4", "3"]);
    drop_test_schema(pool, schema).await;
}

#[tokio::test]
async fn a_failed_write_keeps_the_threads_github_took() {
    let (pool, schema) = test_pool().await.expect("create test database");
    let github = FakeGithub::new();
    github.set(vec![
        subject("1", "org/repo", "PullRequest", true, NOW),
        subject("2", "org/repo", "Issue", true, "2026-11-09T02:00:00Z"),
        subject("3", "org/repo", "Issue", true, "2026-11-09T01:00:00Z"),
    ]);
    sync_with(&pool, at(NOW), github.runner()).await.unwrap();
    *github.fail_path.lock().unwrap() = Some("notifications/threads/2".to_owned());

    let error = mark_repository_read_with(&pool, "org/repo", github.runner())
        .await
        .unwrap_err();
    assert_eq!(error, "notifications/threads/2 failed");
    let inbox = load_github_inbox(&pool).await.unwrap();
    // Thread 1 was written; 2 failed, so 3 was not tried.
    assert_eq!(unread_ids(&inbox), ["2", "3"]);
    drop_test_schema(pool, schema).await;
}
