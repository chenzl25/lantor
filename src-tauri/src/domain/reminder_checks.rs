//! Loop checks: a repeating reminder with a check command runs that command on
//! every tick without waking a model. Empty output is a quiet tick; output is
//! either posted into the reminder's thread as its agent (`post`) or handed to
//! the agent as an inbox item (`wake`).

use std::collections::HashSet;
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde_json::json;
use sqlx::{Row, SqlitePool};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use uuid::Uuid;

use crate::agent_environment::apply_agent_environment_variables;
use crate::agent_routing::ensure_agent_channel_member;
use crate::app::{to_string, CommandResult};
use crate::events::activity::record_agent_activity;
use crate::message_store::insert_agent_message_with_options;
use crate::runtime::process::{
    configure_agent_context_tool_env, configure_agent_identity_env, terminate_process_group,
};
use crate::ui_notifications::{enqueue_ui_event_in_tx, insert_system_message, UiEvent};
use crate::{create_agent_inbox_item, ensure_agent_inbox_wake_work_item, AgentInboxItemInput};

/// Captured stdout per check; longer output is cut and marked as truncated.
const CHECK_OUTPUT_LIMIT: usize = 64 * 1024;
const CHECK_STDERR_LIMIT: usize = 8 * 1024;
/// Largest body posted into a thread in `post` mode.
const POSTED_OUTPUT_CHAR_LIMIT: usize = 20_000;
/// Largest output handed to the agent in `wake` mode.
const WAKE_OUTPUT_CHAR_LIMIT: usize = 16_000;
const CHECK_DETAIL_CHAR_LIMIT: usize = 600;
const MAX_CHECK_TIMEOUT_SECONDS: i64 = 300;
/// Covers killing the process group and recording the result after a timeout.
const CHECK_LEASE_GRACE_SECONDS: u64 = 30;
/// Consecutive failures before the thread is told and retries back off.
pub(crate) const CHECK_FAILURE_NOTICE_THRESHOLD: i64 = 3;
const MAX_FAILURE_BACKOFF_SECONDS: i64 = 60 * 60;

#[derive(Debug, Clone)]
pub(crate) struct ReminderCheckJob {
    pub(crate) reminder_id: Uuid,
    pub(crate) creator_agent_id: Option<Uuid>,
    pub(crate) channel_id: Option<Uuid>,
    pub(crate) thread_root_id: Option<Uuid>,
    pub(crate) title: String,
    pub(crate) check_command: String,
    pub(crate) check_action: String,
    pub(crate) interval_seconds: i64,
    pub(crate) timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReminderCheckOutcome {
    Quiet,
    Posted {
        message_id: Uuid,
    },
    Woke {
        inbox_item_id: Uuid,
    },
    Failed {
        error: String,
        failures: i64,
    },
    /// The loop was cancelled or completed while its command ran.
    Inactive,
}

fn running_checks() -> &'static Mutex<HashSet<Uuid>> {
    static RUNNING: OnceLock<Mutex<HashSet<Uuid>>> = OnceLock::new();
    RUNNING.get_or_init(|| Mutex::new(HashSet::new()))
}

struct RunningCheck(Uuid);

impl RunningCheck {
    /// One check per loop at a time: a tick that arrives while the previous
    /// command still runs is skipped rather than queued.
    fn acquire(reminder_id: Uuid) -> Option<Self> {
        let mut running = running_checks()
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        running.insert(reminder_id).then_some(Self(reminder_id))
    }
}

impl Drop for RunningCheck {
    fn drop(&mut self) {
        running_checks()
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .remove(&self.0);
    }
}

pub(crate) fn spawn_reminder_check(pool: SqlitePool, job: ReminderCheckJob) {
    let Some(guard) = RunningCheck::acquire(job.reminder_id) else {
        return;
    };
    tokio::spawn(async move {
        let _guard = guard;
        // The desktop app and the supervisor both run the reminder worker, so
        // the in-process guard alone cannot keep a slow check from overlapping
        // the next tick claimed by the other process.
        match acquire_check_lease(&pool, job.reminder_id, job.timeout).await {
            Ok(true) => {}
            Ok(false) => return,
            Err(err) => {
                eprintln!("Lantor loop check {} lease failed: {err}", job.reminder_id);
                return;
            }
        }
        if let Err(err) = run_reminder_check(&pool, &job).await {
            eprintln!("Lantor loop check {} failed: {err}", job.reminder_id);
        }
        if let Err(err) = release_check_lease(&pool, job.reminder_id).await {
            eprintln!(
                "Lantor loop check {} lease release failed: {err}",
                job.reminder_id
            );
        }
    });
}

/// Claims the loop's check across processes. The lease outlives the command
/// timeout so a crashed process cannot block the loop for longer than that.
pub(crate) async fn acquire_check_lease(
    pool: &SqlitePool,
    reminder_id: Uuid,
    timeout: Duration,
) -> CommandResult<bool> {
    let lease_seconds = timeout.as_secs() + CHECK_LEASE_GRACE_SECONDS;
    let claimed = sqlx::query(
        r#"
        update reminders
        set check_lease_until = strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now', '+' || $2 || ' seconds')
        where id = $1
          and (check_lease_until is null
               or julianday(check_lease_until) <= julianday('now'))
        "#,
    )
    .bind(reminder_id)
    .bind(lease_seconds as i64)
    .execute(pool)
    .await
    .map_err(to_string)?
    .rows_affected();
    Ok(claimed == 1)
}

pub(crate) async fn release_check_lease(pool: &SqlitePool, reminder_id: Uuid) -> CommandResult<()> {
    sqlx::query("update reminders set check_lease_until = null where id = $1")
        .bind(reminder_id)
        .execute(pool)
        .await
        .map_err(to_string)?;
    Ok(())
}

struct CheckAgent {
    handle: String,
    working_directory: String,
    environment_variables: String,
}

enum CommandResultKind {
    Output { text: String, truncated: bool },
    Empty,
    Failed(String),
}

pub(crate) async fn run_reminder_check(
    pool: &SqlitePool,
    job: &ReminderCheckJob,
) -> CommandResult<ReminderCheckOutcome> {
    let result = match load_check_agent(pool, job.creator_agent_id).await? {
        Some(agent) => run_check_command(job, &agent).await,
        None => CommandResultKind::Failed("the loop's agent no longer exists".to_owned()),
    };
    match result {
        CommandResultKind::Empty => {
            if !loop_is_active(pool, job.reminder_id).await? {
                return Ok(ReminderCheckOutcome::Inactive);
            }
            record_check_success(pool, job, "quiet", "").await?;
            Ok(ReminderCheckOutcome::Quiet)
        }
        CommandResultKind::Output { text, truncated } => {
            if !loop_is_active(pool, job.reminder_id).await? {
                return Ok(ReminderCheckOutcome::Inactive);
            }
            match deliver_check_output(pool, job, &text, truncated).await {
                Ok(outcome) => Ok(outcome),
                Err(err) => record_check_failure(pool, job, &err).await,
            }
        }
        CommandResultKind::Failed(error) => {
            if !loop_is_active(pool, job.reminder_id).await? {
                return Ok(ReminderCheckOutcome::Inactive);
            }
            record_check_failure(pool, job, &error).await
        }
    }
}

async fn load_check_agent(
    pool: &SqlitePool,
    agent_id: Option<Uuid>,
) -> CommandResult<Option<CheckAgent>> {
    let Some(agent_id) = agent_id else {
        return Ok(None);
    };
    let row = sqlx::query(
        "select handle, working_directory, environment_variables from agents where id = $1",
    )
    .bind(agent_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;
    Ok(row.map(|row| CheckAgent {
        handle: row.get("handle"),
        working_directory: row.get("working_directory"),
        environment_variables: row.get("environment_variables"),
    }))
}

/// Agent runs use zsh; hosts without it (Linux CI) fall back to sh.
fn check_shell() -> &'static str {
    if std::path::Path::new("/bin/zsh").exists() {
        "/bin/zsh"
    } else {
        "/bin/sh"
    }
}

/// A check may run for one interval, between 30 seconds and 5 minutes.
pub(crate) fn check_timeout(interval_seconds: i64) -> Duration {
    Duration::from_secs(interval_seconds.clamp(30, MAX_CHECK_TIMEOUT_SECONDS) as u64)
}

async fn run_check_command(job: &ReminderCheckJob, agent: &CheckAgent) -> CommandResultKind {
    let Some(agent_id) = job.creator_agent_id else {
        return CommandResultKind::Failed("the loop has no agent".to_owned());
    };
    // A login shell matches agent runs: Finder-launched apps do not inherit
    // Homebrew's PATH.
    let mut command = Command::new(check_shell());
    command.arg("-lc").arg(&job.check_command);
    if let Err(err) = apply_agent_environment_variables(&mut command, &agent.environment_variables)
    {
        return CommandResultKind::Failed(err);
    }
    configure_agent_identity_env(&mut command, agent_id, &agent.handle);
    configure_agent_context_tool_env(&mut command);
    command.env("LANTOR_REMINDER_ID", job.reminder_id.to_string());
    #[cfg(unix)]
    command.process_group(0);
    if !agent.working_directory.trim().is_empty() {
        command.current_dir(agent.working_directory.trim());
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => return CommandResultKind::Failed(format!("could not start command: {err}")),
    };
    let pid = child.id();
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return CommandResultKind::Failed("command pipes were not captured".to_owned());
    };
    // Each reader owns its pipe and closes it at its limit, so a chatty
    // command gets EPIPE instead of blocking until the timeout.
    let read_stdout = async move {
        let mut buffer = Vec::new();
        let read = stdout
            .take(CHECK_OUTPUT_LIMIT as u64 + 1)
            .read_to_end(&mut buffer)
            .await;
        (buffer, read.is_ok())
    };
    let read_stderr = async move {
        let mut buffer = Vec::new();
        let _ = stderr
            .take(CHECK_STDERR_LIMIT as u64)
            .read_to_end(&mut buffer)
            .await;
        buffer
    };
    let timeout = job.timeout;
    let finished = tokio::time::timeout(timeout, async {
        let ((stdout, stdout_ok), stderr, status) =
            tokio::join!(read_stdout, read_stderr, child.wait());
        (stdout, stdout_ok, stderr, status)
    })
    .await;
    let (mut stdout, stdout_ok, stderr, status) = match finished {
        Ok(result) => result,
        Err(_) => {
            if let Some(pid) = pid.and_then(|pid| i32::try_from(pid).ok()) {
                let _ = terminate_process_group(pid).await;
            }
            return CommandResultKind::Failed(format!(
                "timed out after {}s",
                timeout.as_secs_f32().ceil()
            ));
        }
    };
    let status = match status {
        Ok(status) => status,
        Err(err) => return CommandResultKind::Failed(format!("could not wait for command: {err}")),
    };
    let truncated = stdout.len() > CHECK_OUTPUT_LIMIT;
    // Closing the pipe early makes the shell report SIGPIPE; that is our
    // truncation, not a failed check.
    if !status.success() && !truncated {
        let stderr = compact_detail(&String::from_utf8_lossy(&stderr));
        let code = status
            .code()
            .map(|code| format!("exit status {code}"))
            .unwrap_or_else(|| "terminated by a signal".to_owned());
        return CommandResultKind::Failed(if stderr.is_empty() {
            code
        } else {
            format!("{code}: {stderr}")
        });
    }
    if !stdout_ok && stdout.is_empty() {
        return CommandResultKind::Failed("could not read command output".to_owned());
    }
    stdout.truncate(CHECK_OUTPUT_LIMIT);
    let text = String::from_utf8_lossy(&stdout).trim().to_owned();
    if text.is_empty() {
        CommandResultKind::Empty
    } else {
        CommandResultKind::Output { text, truncated }
    }
}

async fn deliver_check_output(
    pool: &SqlitePool,
    job: &ReminderCheckJob,
    output: &str,
    truncated: bool,
) -> CommandResult<ReminderCheckOutcome> {
    let (Some(agent_id), Some(channel_id)) = (job.creator_agent_id, job.channel_id) else {
        return Err("the loop has no agent or channel".to_owned());
    };
    match job.check_action.as_str() {
        "post" => {
            ensure_agent_channel_member(pool, agent_id, channel_id, "loop post").await?;
            let (body, cut) = limit_chars(output, POSTED_OUTPUT_CHAR_LIMIT);
            let body = if truncated || cut {
                format!("{body}\n\n…(output truncated)")
            } else {
                body
            };
            // Output often quotes external text (chat, CI logs). Never let it
            // dispatch @mentions as if the agent had written them.
            let message_id = insert_agent_message_with_options(
                pool,
                agent_id,
                channel_id,
                job.thread_root_id,
                &body,
                false,
                false,
            )
            .await?;
            record_check_success(pool, job, "posted", &message_id.to_string()).await?;
            record_agent_activity(
                pool,
                Some(agent_id),
                None,
                "reminder",
                "Loop posted check output",
                json!({ "reminder_id": job.reminder_id, "message_id": message_id }).to_string(),
            )
            .await?;
            Ok(ReminderCheckOutcome::Posted { message_id })
        }
        "wake" => {
            let (output, cut) = limit_chars(output, WAKE_OUTPUT_CHAR_LIMIT);
            let title = format!("Loop check output: {}", job.title);
            let inbox_item_id = create_agent_inbox_item(
                pool,
                AgentInboxItemInput {
                    agent_id,
                    channel_id: Some(channel_id),
                    thread_root_id: job.thread_root_id,
                    source_message_id: None,
                    task_id: None,
                    kind: "loop_check",
                    priority: 80,
                    title: &title,
                    body_preview: &output,
                    payload: json!({
                        "reminder_id": job.reminder_id,
                        "check_command": job.check_command,
                        "check_output": output,
                        "truncated": truncated || cut,
                    }),
                },
            )
            .await?;
            ensure_agent_inbox_wake_work_item(pool, agent_id).await?;
            record_check_success(pool, job, "woke", &inbox_item_id.to_string()).await?;
            Ok(ReminderCheckOutcome::Woke { inbox_item_id })
        }
        other => Err(format!("unsupported check_action: {other}")),
    }
}

async fn loop_is_active(pool: &SqlitePool, reminder_id: Uuid) -> CommandResult<bool> {
    sqlx::query_scalar(
        "select exists(select 1 from reminders where id = $1 and status = 'scheduled')",
    )
    .bind(reminder_id)
    .fetch_one(pool)
    .await
    .map_err(to_string)
}

async fn record_check_success(
    pool: &SqlitePool,
    job: &ReminderCheckJob,
    status: &str,
    detail: &str,
) -> CommandResult<()> {
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(to_string)?;
    let previous_failures: Option<i64> =
        sqlx::query_scalar("select check_failures from reminders where id = $1")
            .bind(job.reminder_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(to_string)?;
    sqlx::query(
        r#"
        update reminders
        set last_checked_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
            last_check_status = $2,
            last_check_detail = $3,
            check_failures = 0,
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1
        "#,
    )
    .bind(job.reminder_id)
    .bind(status)
    .bind(detail)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;
    enqueue_ui_event_in_tx(
        &mut transaction,
        &UiEvent::Refresh {
            reason: "reminder_checked",
        },
    )
    .await?;
    transaction.commit().await.map_err(to_string)?;
    if previous_failures.unwrap_or_default() >= CHECK_FAILURE_NOTICE_THRESHOLD {
        if let Some(channel_id) = job.channel_id {
            let _ = insert_system_message(
                pool,
                channel_id,
                job.thread_root_id,
                format!("Loop \"{}\" is working again.", job.title),
            )
            .await;
        }
    }
    Ok(())
}

async fn record_check_failure(
    pool: &SqlitePool,
    job: &ReminderCheckJob,
    error: &str,
) -> CommandResult<ReminderCheckOutcome> {
    let error = compact_detail(error);
    let mut transaction = pool.begin().await.map_err(to_string)?;
    let failures: Option<i64> = sqlx::query_scalar(
        r#"
        update reminders
        set last_checked_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
            last_check_status = 'failed',
            last_check_detail = $2,
            check_failures = check_failures + 1,
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1
        returning check_failures
        "#,
    )
    .bind(job.reminder_id)
    .bind(&error)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(to_string)?;
    let failures = failures.unwrap_or(1);
    if let Some(backoff) = failure_backoff_seconds(job.interval_seconds, failures) {
        // Push the next tick out; the fire step re-aligns to the cadence after.
        sqlx::query(
            r#"
            update reminders
            set due_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now','+' || $2 || ' seconds')
            where id = $1
              and status = 'scheduled'
              and julianday(due_at) < julianday('now','+' || $2 || ' seconds')
            "#,
        )
        .bind(job.reminder_id)
        .bind(backoff)
        .execute(&mut *transaction)
        .await
        .map_err(to_string)?;
    }
    enqueue_ui_event_in_tx(
        &mut transaction,
        &UiEvent::Refresh {
            reason: "reminder_checked",
        },
    )
    .await?;
    transaction.commit().await.map_err(to_string)?;
    if failures == CHECK_FAILURE_NOTICE_THRESHOLD {
        if let Some(channel_id) = job.channel_id {
            let _ = insert_system_message(
                pool,
                channel_id,
                job.thread_root_id,
                format!(
                    "Loop \"{}\" failed {failures} checks in a row: {error}\nIt keeps retrying less often. Stop it from the agent's Reminders tab.",
                    job.title
                ),
            )
            .await;
        }
    }
    Ok(ReminderCheckOutcome::Failed { error, failures })
}

/// Retries back off only after the failure notice: twice the interval, then
/// doubling, capped at an hour (or the interval when that is longer).
pub(crate) fn failure_backoff_seconds(interval_seconds: i64, failures: i64) -> Option<i64> {
    if failures < CHECK_FAILURE_NOTICE_THRESHOLD {
        return None;
    }
    let exponent = (failures - CHECK_FAILURE_NOTICE_THRESHOLD + 1).min(16) as u32;
    let cap = MAX_FAILURE_BACKOFF_SECONDS.max(interval_seconds);
    Some(
        interval_seconds
            .max(1)
            .saturating_mul(2_i64.saturating_pow(exponent))
            .min(cap),
    )
}

fn limit_chars(text: &str, limit: usize) -> (String, bool) {
    match text.char_indices().nth(limit) {
        Some((cut, _)) => (text[..cut].trim_end().to_owned(), true),
        None => (text.to_owned(), false),
    }
}

fn compact_detail(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let (limited, cut) = limit_chars(&flat, CHECK_DETAIL_CHAR_LIMIT);
    if cut {
        format!("{limited}…")
    } else {
        limited
    }
}

#[cfg(test)]
#[path = "../tests/reminder_checks.rs"]
mod db_tests;

#[cfg(test)]
mod tests {
    use super::{check_timeout, failure_backoff_seconds, limit_chars};

    #[test]
    fn check_timeout_follows_the_interval_within_bounds() {
        assert_eq!(check_timeout(60).as_secs(), 60);
        assert_eq!(check_timeout(10).as_secs(), 30);
        assert_eq!(check_timeout(3_600).as_secs(), 300);
    }

    #[test]
    fn backoff_starts_at_the_failure_notice_and_is_capped() {
        assert_eq!(failure_backoff_seconds(300, 1), None);
        assert_eq!(failure_backoff_seconds(300, 2), None);
        assert_eq!(failure_backoff_seconds(300, 3), Some(600));
        assert_eq!(failure_backoff_seconds(300, 4), Some(1_200));
        assert_eq!(failure_backoff_seconds(300, 6), Some(3_600));
        assert_eq!(failure_backoff_seconds(300, 40), Some(3_600));
        assert_eq!(failure_backoff_seconds(7_200, 3), Some(7_200));
    }

    #[test]
    fn limit_chars_respects_character_boundaries() {
        assert_eq!(limit_chars("转发消息", 2), ("转发".to_owned(), true));
        assert_eq!(limit_chars("ok", 5), ("ok".to_owned(), false));
    }
}
