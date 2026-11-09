//! The owner's GitHub inbox: their participating notifications across every
//! repository, read from the GitHub notifications API through `gh`.
//!
//! A sync keeps every unread thread plus the newest read ones. Marking a thread
//! read or done in Lantor writes it back to GitHub first, then locally; the
//! GitHub `updated_at` at that moment is kept, so a sync that started earlier
//! cannot undo it, while new activity on the thread brings it back.

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    sync::LazyLock,
    time::Duration,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use tokio::{sync::Mutex, time::sleep};
use uuid::Uuid;

use super::{parse_json, run_github_cli};
use crate::{
    app::{to_string, CommandResult},
    ui_notifications::{enqueue_ui_event_in_tx, UiEvent},
};

/// GitHub asks clients to poll notifications at most once a minute.
const POLL_INTERVAL: Duration = Duration::from_secs(60);
const ERROR_RETRY_INTERVAL: Duration = Duration::from_secs(5 * 60);
const POLL_TICK: Duration = Duration::from_secs(15);
const ACCOUNT_RECHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// Read threads kept besides the unread ones, newest first.
const RECENT_THREAD_LIMIT: usize = 50;
const UNREAD_PAGE_SIZE: usize = 100;
const UNREAD_PAGE_LIMIT: usize = 5;
const UPDATED_REASON: &str = "github_inbox_updated";

/// One sync at a time, so the poller and a manual refresh do not both call GitHub.
static SYNC_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct GithubInboxItem {
    pub(crate) thread_id: String,
    pub(crate) repository: String,
    /// GitHub's subject type: `PullRequest`, `Issue`, `Release`, `Discussion`, ...
    pub(crate) subject_type: String,
    pub(crate) title: String,
    pub(crate) number: Option<i64>,
    /// Web page of the subject, at its latest comment when GitHub names one.
    pub(crate) url: String,
    /// Why the owner gets it: `review_requested`, `mention`, `author`, ...
    pub(crate) reason: String,
    pub(crate) unread: bool,
    pub(crate) updated_at: String,
    /// The Lantor thread of a review or issue task for this PR or issue.
    pub(crate) thread_root_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct GithubInbox {
    /// Off until the owner first opens the inbox; only then does Lantor poll GitHub.
    pub(crate) enabled: bool,
    pub(crate) account_login: String,
    pub(crate) synced_at: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) items: Vec<GithubInboxItem>,
}

#[derive(Debug, Deserialize)]
struct NotificationCli {
    id: String,
    unread: bool,
    reason: String,
    updated_at: String,
    subject: SubjectCli,
    repository: RepositoryCli,
}

#[derive(Debug, Deserialize)]
struct SubjectCli {
    title: String,
    url: Option<String>,
    latest_comment_url: Option<String>,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Debug, Deserialize)]
struct RepositoryCli {
    full_name: String,
    html_url: String,
}

#[derive(Debug, Clone, PartialEq)]
struct InboxThread {
    thread_id: String,
    repository: String,
    subject_type: String,
    title: String,
    number: Option<i64>,
    url: String,
    reason: String,
    unread: bool,
    updated_at: String,
}

#[derive(Debug, Deserialize)]
struct AccountCli {
    login: String,
}

impl From<NotificationCli> for InboxThread {
    fn from(notification: NotificationCli) -> Self {
        let (url, number) = subject_page(&notification);
        Self {
            thread_id: notification.id,
            repository: notification.repository.full_name,
            subject_type: notification.subject.kind,
            title: notification.subject.title,
            number,
            url,
            reason: notification.reason,
            unread: notification.unread,
            updated_at: notification.updated_at,
        }
    }
}

/// The web page of a notification's subject, and its PR, issue or discussion
/// number. The API gives `api.github.com` URLs, and none for some subjects.
fn subject_page(notification: &NotificationCli) -> (String, Option<i64>) {
    let repository_url = notification.repository.html_url.trim_end_matches('/');
    let api_prefix = format!(
        "https://api.github.com/repos/{}/",
        notification.repository.full_name
    );
    let path = notification
        .subject
        .url
        .as_deref()
        .and_then(|url| url.strip_prefix(&api_prefix));
    let mut segments = path.map(|path| path.split('/')).into_iter().flatten();
    let (kind, id) = (segments.next(), segments.next());
    let number = id.and_then(|id| id.parse::<i64>().ok());
    let page = match (kind, id, number) {
        (Some("pulls"), _, Some(number)) => format!("{repository_url}/pull/{number}"),
        (Some("issues"), _, Some(number)) => format!("{repository_url}/issues/{number}"),
        (Some("discussions"), _, Some(number)) => {
            format!("{repository_url}/discussions/{number}")
        }
        (Some("commits"), Some(sha), _) => format!("{repository_url}/commit/{sha}"),
        (Some("releases"), ..) => format!("{repository_url}/releases"),
        _ => match notification.subject.kind.as_str() {
            "RepositoryAdvisory" | "RepositoryVulnerabilityAlert" => {
                format!("{repository_url}/security")
            }
            "CheckSuite" | "WorkflowRun" => format!("{repository_url}/actions"),
            "Discussion" => format!("{repository_url}/discussions"),
            _ => repository_url.to_owned(),
        },
    };
    let number = match kind {
        Some("pulls" | "issues" | "discussions") => number,
        _ => None,
    };
    let anchor = notification
        .subject
        .latest_comment_url
        .as_deref()
        .and_then(|url| url.strip_prefix(&api_prefix))
        .and_then(|path| {
            let (kind, id) = path.rsplit_once('/')?;
            id.parse::<u64>().ok()?;
            match kind {
                "issues/comments" => Some(format!("#issuecomment-{id}")),
                "pulls/comments" => Some(format!("#discussion_r{id}")),
                _ => None,
            }
        });
    match (anchor, number) {
        (Some(anchor), Some(_)) => (format!("{page}{anchor}"), number),
        _ => (page, number),
    }
}

fn parse_notifications(bytes: &[u8]) -> CommandResult<Vec<InboxThread>> {
    let notifications: Vec<NotificationCli> = parse_json(bytes, "GitHub notifications")?;
    Ok(notifications.into_iter().map(InboxThread::from).collect())
}

fn is_thread_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 32 && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn duration_since(now: DateTime<Utc>, value: Option<&str>) -> Option<Duration> {
    let then = DateTime::parse_from_rfc3339(value?)
        .ok()?
        .with_timezone(&Utc);
    (now - then).to_std().ok().or(Some(Duration::ZERO))
}

struct InboxState {
    enabled: bool,
    account_login: String,
    account_checked_at: Option<String>,
    synced_at: Option<String>,
    attempted_at: Option<String>,
    error: Option<String>,
}

async fn load_state(pool: &SqlitePool) -> CommandResult<InboxState> {
    let row = sqlx::query(
        r#"
        select enabled, account_login, account_checked_at, synced_at, attempted_at, error
        from github_inbox_state
        where id = 1
        "#,
    )
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;
    Ok(match row {
        Some(row) => InboxState {
            enabled: row.get("enabled"),
            account_login: row.get("account_login"),
            account_checked_at: row.get("account_checked_at"),
            synced_at: row.get("synced_at"),
            attempted_at: row.get("attempted_at"),
            error: row.get("error"),
        },
        None => InboxState {
            enabled: false,
            account_login: String::new(),
            account_checked_at: None,
            synced_at: None,
            attempted_at: None,
            error: None,
        },
    })
}

const VISIBLE_THREADS: &str = r#"
    select n.thread_id, n.repository, n.subject_type, n.title, n.number, n.url, n.reason,
        n.unread, n.github_updated_at,
        (
            select t.thread_root_id
            from github_resource_threads t
            join channel_github_repositories b
                on b.channel_id = t.channel_id and b.repository_id = t.repository_id
            where lower(b.name_with_owner) = lower(n.repository)
                and t.resource_number = n.number
                and t.resource_kind = case n.subject_type
                    when 'PullRequest' then 'pull_request'
                    when 'Issue' then 'issue'
                end
            order by t.updated_at desc
            limit 1
        ) as thread_root_id
    from github_inbox_threads n
    where n.done_updated_at is null or n.github_updated_at > n.done_updated_at
    order by n.github_updated_at desc, n.thread_id desc
"#;

async fn load_items<'e, E>(executor: E) -> CommandResult<Vec<GithubInboxItem>>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let rows = sqlx::query(VISIBLE_THREADS)
        .fetch_all(executor)
        .await
        .map_err(to_string)?;
    Ok(rows
        .into_iter()
        .map(|row| GithubInboxItem {
            thread_id: row.get("thread_id"),
            repository: row.get("repository"),
            subject_type: row.get("subject_type"),
            title: row.get("title"),
            number: row.get("number"),
            url: row.get("url"),
            reason: row.get("reason"),
            unread: row.get("unread"),
            updated_at: row.get("github_updated_at"),
            thread_root_id: row.get("thread_root_id"),
        })
        .collect())
}

pub(crate) async fn load_github_inbox(pool: &SqlitePool) -> CommandResult<GithubInbox> {
    let state = load_state(pool).await?;
    Ok(GithubInbox {
        enabled: state.enabled,
        account_login: state.account_login,
        synced_at: state.synced_at,
        error: state.error,
        items: load_items(pool).await?,
    })
}

/// Reads the owner's notifications from GitHub and stores them. Also turns the
/// inbox on, so the poller keeps it current from then on.
pub(crate) async fn refresh_github_inbox(pool: &SqlitePool) -> CommandResult<GithubInbox> {
    sync_with(pool, Utc::now(), run_github_cli).await?;
    load_github_inbox(pool).await
}

async fn sync_with<F, Fut>(pool: &SqlitePool, now: DateTime<Utc>, run: F) -> CommandResult<()>
where
    F: Fn(Vec<String>) -> Fut,
    Fut: Future<Output = CommandResult<Vec<u8>>>,
{
    let _guard = SYNC_LOCK.lock().await;
    let attempted_at = now.to_rfc3339();
    sqlx::query(
        r#"
        insert into github_inbox_state (id, enabled, attempted_at) values (1, 1, $1)
        on conflict(id) do update set enabled = 1, attempted_at = excluded.attempted_at
        "#,
    )
    .bind(&attempted_at)
    .execute(pool)
    .await
    .map_err(to_string)?;
    match fetch_snapshot(pool, now, &run).await {
        Ok(snapshot) => apply_snapshot(pool, snapshot, &attempted_at).await,
        Err(error) => {
            record_error(pool, &error).await?;
            Err(error)
        }
    }
}

struct Snapshot {
    /// Set when the account was checked during this sync.
    account_login: Option<String>,
    threads: Vec<InboxThread>,
}

async fn fetch_snapshot<F, Fut>(
    pool: &SqlitePool,
    now: DateTime<Utc>,
    run: &F,
) -> CommandResult<Snapshot>
where
    F: Fn(Vec<String>) -> Fut,
    Fut: Future<Output = CommandResult<Vec<u8>>>,
{
    let state = load_state(pool).await?;
    let account_due = state.account_login.is_empty()
        || duration_since(now, state.account_checked_at.as_deref())
            .is_none_or(|elapsed| elapsed >= ACCOUNT_RECHECK_INTERVAL);
    let account_login = if account_due {
        let output = run(vec!["api".to_owned(), "user".to_owned()]).await?;
        let account: AccountCli = parse_json(&output, "GitHub account")?;
        Some(account.login)
    } else {
        None
    };

    // Every unread thread, so the counts match GitHub's, then the newest ones
    // whether read or not.
    let mut threads = Vec::new();
    for page in 1..=UNREAD_PAGE_LIMIT {
        let output = run(vec![
            "api".to_owned(),
            format!("notifications?participating=true&per_page={UNREAD_PAGE_SIZE}&page={page}"),
        ])
        .await?;
        let unread = parse_notifications(&output)?;
        let last_page = unread.len() < UNREAD_PAGE_SIZE;
        threads.extend(unread);
        if last_page {
            break;
        }
    }
    let output = run(vec![
        "api".to_owned(),
        format!("notifications?participating=true&all=true&per_page={RECENT_THREAD_LIMIT}"),
    ])
    .await?;
    let mut seen: HashSet<String> = threads
        .iter()
        .map(|thread| thread.thread_id.clone())
        .collect();
    for thread in parse_notifications(&output)? {
        if seen.insert(thread.thread_id.clone()) {
            threads.push(thread);
        }
    }
    Ok(Snapshot {
        account_login,
        threads,
    })
}

struct LocalMarks {
    read_updated_at: Option<String>,
    done_updated_at: Option<String>,
}

async fn apply_snapshot(
    pool: &SqlitePool,
    snapshot: Snapshot,
    synced_at: &str,
) -> CommandResult<()> {
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(to_string)?;
    let state_row = sqlx::query("select account_login, error from github_inbox_state where id = 1")
        .fetch_one(&mut *transaction)
        .await
        .map_err(to_string)?;
    let previous_login: String = state_row.get("account_login");
    let previous_error: Option<String> = state_row.get("error");
    let account_login = snapshot
        .account_login
        .clone()
        .unwrap_or_else(|| previous_login.clone());
    let before = load_items(&mut *transaction).await?;
    if !previous_login.is_empty() && previous_login != account_login {
        sqlx::query("delete from github_inbox_threads")
            .execute(&mut *transaction)
            .await
            .map_err(to_string)?;
    }

    let marks: HashMap<String, LocalMarks> =
        sqlx::query("select thread_id, read_updated_at, done_updated_at from github_inbox_threads")
            .fetch_all(&mut *transaction)
            .await
            .map_err(to_string)?
            .into_iter()
            .map(|row| {
                (
                    row.get("thread_id"),
                    LocalMarks {
                        read_updated_at: row.get("read_updated_at"),
                        done_updated_at: row.get("done_updated_at"),
                    },
                )
            })
            .collect();

    let mut kept = HashSet::new();
    for thread in &snapshot.threads {
        kept.insert(thread.thread_id.as_str());
        let local = marks.get(&thread.thread_id);
        // Read in Lantor since its last activity, though this sync may have
        // started before GitHub took the write.
        let read_locally = local
            .and_then(|marks| marks.read_updated_at.as_deref())
            .is_some_and(|read_at| thread.updated_at.as_str() <= read_at);
        let done_locally = local
            .and_then(|marks| marks.done_updated_at.as_deref())
            .is_some_and(|done_at| thread.updated_at.as_str() <= done_at);
        sqlx::query(
            r#"
            insert into github_inbox_threads (
                thread_id, repository, subject_type, title, number, url, reason, unread,
                github_updated_at
            )
            values ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            on conflict(thread_id) do update set
                repository = excluded.repository,
                subject_type = excluded.subject_type,
                title = excluded.title,
                number = excluded.number,
                url = excluded.url,
                reason = excluded.reason,
                unread = excluded.unread,
                github_updated_at = excluded.github_updated_at
            "#,
        )
        .bind(&thread.thread_id)
        .bind(&thread.repository)
        .bind(&thread.subject_type)
        .bind(&thread.title)
        .bind(thread.number)
        .bind(&thread.url)
        .bind(&thread.reason)
        .bind(thread.unread && !read_locally && !done_locally)
        .bind(&thread.updated_at)
        .execute(&mut *transaction)
        .await
        .map_err(to_string)?;
    }
    for thread_id in marks.keys().filter(|id| !kept.contains(id.as_str())) {
        sqlx::query("delete from github_inbox_threads where thread_id = $1")
            .bind(thread_id)
            .execute(&mut *transaction)
            .await
            .map_err(to_string)?;
    }

    sqlx::query(
        r#"
        update github_inbox_state
        set account_login = $1,
            account_checked_at = coalesce($2, account_checked_at),
            synced_at = $3,
            error = null
        where id = 1
        "#,
    )
    .bind(&account_login)
    .bind(snapshot.account_login.as_ref().map(|_| synced_at))
    .bind(synced_at)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;

    let after = load_items(&mut *transaction).await?;
    if before != after || previous_error.is_some() || previous_login != account_login {
        enqueue_ui_event_in_tx(
            &mut transaction,
            &UiEvent::Refresh {
                reason: UPDATED_REASON,
            },
        )
        .await?;
    }
    transaction.commit().await.map_err(to_string)
}

async fn record_error(pool: &SqlitePool, error: &str) -> CommandResult<()> {
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(to_string)?;
    let changed =
        sqlx::query("update github_inbox_state set error = $1 where id = 1 and error is not $1")
            .bind(error)
            .execute(&mut *transaction)
            .await
            .map_err(to_string)?
            .rows_affected()
            > 0;
    if changed {
        enqueue_ui_event_in_tx(
            &mut transaction,
            &UiEvent::Refresh {
                reason: UPDATED_REASON,
            },
        )
        .await?;
    }
    transaction.commit().await.map_err(to_string)
}

/// Marks a thread read on GitHub, then in Lantor.
pub(crate) async fn mark_github_inbox_read(
    pool: &SqlitePool,
    thread_id: &str,
) -> CommandResult<GithubInbox> {
    mark_with(pool, thread_id, Mark::Read, run_github_cli).await?;
    load_github_inbox(pool).await
}

/// Marks a thread done on GitHub, then hides it in Lantor until new activity.
pub(crate) async fn mark_github_inbox_done(
    pool: &SqlitePool,
    thread_id: &str,
) -> CommandResult<GithubInbox> {
    mark_with(pool, thread_id, Mark::Done, run_github_cli).await?;
    load_github_inbox(pool).await
}

#[derive(Clone, Copy)]
enum Mark {
    Read,
    Done,
}

async fn mark_with<F, Fut>(
    pool: &SqlitePool,
    thread_id: &str,
    mark: Mark,
    run: F,
) -> CommandResult<()>
where
    F: Fn(Vec<String>) -> Fut,
    Fut: Future<Output = CommandResult<Vec<u8>>>,
{
    if !is_thread_id(thread_id) {
        return Err("invalid GitHub notification thread id".to_owned());
    }
    let exists: Option<i64> =
        sqlx::query_scalar("select 1 from github_inbox_threads where thread_id = $1")
            .bind(thread_id)
            .fetch_optional(pool)
            .await
            .map_err(to_string)?;
    if exists.is_none() {
        return Err("GitHub notification not found".to_owned());
    }
    let method = match mark {
        Mark::Read => "PATCH",
        Mark::Done => "DELETE",
    };
    run(vec![
        "api".to_owned(),
        "--method".to_owned(),
        method.to_owned(),
        format!("notifications/threads/{thread_id}"),
    ])
    .await?;

    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(to_string)?;
    let statement = match mark {
        Mark::Read => {
            "update github_inbox_threads set unread = 0, read_updated_at = github_updated_at \
             where thread_id = $1"
        }
        Mark::Done => {
            "update github_inbox_threads set unread = 0, read_updated_at = github_updated_at, \
             done_updated_at = github_updated_at where thread_id = $1"
        }
    };
    sqlx::query(statement)
        .bind(thread_id)
        .execute(&mut *transaction)
        .await
        .map_err(to_string)?;
    enqueue_ui_event_in_tx(
        &mut transaction,
        &UiEvent::Refresh {
            reason: UPDATED_REASON,
        },
    )
    .await?;
    transaction.commit().await.map_err(to_string)
}

/// Whether the poller should sync now: the inbox is on, and the last attempt
/// is a minute old, or five minutes after a failure.
async fn poll_due(pool: &SqlitePool, now: DateTime<Utc>) -> CommandResult<bool> {
    let state = load_state(pool).await?;
    if !state.enabled {
        return Ok(false);
    }
    let wait = if state.error.is_some() {
        ERROR_RETRY_INTERVAL
    } else {
        POLL_INTERVAL
    };
    Ok(duration_since(now, state.attempted_at.as_deref()).is_none_or(|elapsed| elapsed >= wait))
}

/// Keeps the inbox current once the owner has opened it. Runs in the process
/// that serves the API, not in the supervisor.
pub(crate) fn spawn_github_inbox_poller(pool: SqlitePool) {
    tauri::async_runtime::spawn(async move {
        loop {
            match poll_due(&pool, Utc::now()).await {
                Ok(true) => {
                    // Failures are stored on the inbox and shown there.
                    let _ = sync_with(&pool, Utc::now(), run_github_cli).await;
                }
                Ok(false) => {}
                Err(err) => eprintln!("Lantor GitHub inbox poller failed: {err}"),
            }
            sleep(POLL_TICK).await;
        }
    });
}

#[cfg(test)]
mod tests;
