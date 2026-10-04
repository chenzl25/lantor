use serde_json::{json, Value};
use sqlx::{Sqlite, SqlitePool, Transaction};
use uuid::Uuid;

use crate::activity_store::load_agent_activity_in_tx;
use crate::app::{to_string, CommandResult};
use crate::ui_notifications::{enqueue_ui_event_in_tx, UiEvent};

fn activity_phase(kind: &str) -> &'static str {
    match kind {
        "thinking" => "thinking",
        "command" => "command",
        "file_edit" => "file_edit",
        "tools" => "tools",
        "error" | "event_error" | "run_error" => "error",
        "run" | "run_retry" | "usage" => "runtime",
        "dispatch" | "mention" | "dm" | "task" | "schedule" | "channel" | "membership" => "work",
        "profile" | "memory" => "profile",
        _ => "acting",
    }
}

pub(crate) fn normalize_agent_activity_kind(kind: Option<&str>) -> &'static str {
    match kind.map(str::trim).filter(|kind| !kind.is_empty()) {
        Some("thinking") => "thinking",
        Some("command") | Some("running_command") => "command",
        Some("file_edit") | Some("editing_file") => "file_edit",
        Some("tools") | Some("tool") => "tools",
        Some("error") => "error",
        Some("run_retry") => "run_retry",
        Some("task") => "task",
        Some("message") => "message",
        Some("dispatch") => "dispatch",
        Some("reminder") => "schedule",
        Some("schedule") => "schedule",
        Some("usage") => "usage",
        Some("memory") => "memory",
        Some("channel") => "channel",
        Some("membership") => "membership",
        _ => "acting",
    }
}

pub(crate) fn activity_status(kind: &str, title: &str) -> &'static str {
    let lowered = title.to_lowercase();
    let is_terminal_error_kind = matches!(kind, "error" | "event_error" | "run_error");
    let can_infer_error_from_title =
        matches!(kind, "command" | "run" | "dispatch" | "task" | "schedule");
    if is_terminal_error_kind
        || (can_infer_error_from_title
            && (lowered.contains("failed")
                || lowered.contains("error")
                || lowered.contains("rejected")))
    {
        "error"
    } else if kind == "run_retry"
        || lowered.contains("warning")
        || lowered.contains("cancel")
        || lowered.contains("stop")
        || lowered.contains("stopping")
    {
        "warning"
    } else if lowered.contains("completed")
        || lowered.contains("complete")
        || lowered.contains("done")
        || lowered.contains("exited")
        || lowered.contains("finished")
        || lowered.contains("ready")
        || lowered.contains("accepted")
    {
        "success"
    } else if matches!(
        kind,
        "thinking" | "command" | "file_edit" | "tools" | "acting"
    ) || lowered.contains("running")
        || lowered.contains("started")
        || lowered.contains("queued")
        || lowered.contains("dispatched")
        || lowered.contains("responding")
        || lowered.contains("thinking")
        || lowered.contains("editing")
        || lowered.contains("using")
    {
        "active"
    } else {
        "info"
    }
}

pub(crate) fn work_status_title(status: &str) -> &'static str {
    match status {
        "running" => "Request started",
        "done" => "Request completed",
        "silent" => "No visible reply needed",
        "held" => "Reply held for newer context",
        "cancelled" => "Request cancelled",
        "failed" => "Request failed",
        "queued" => "Request queued",
        _ => "Request updated",
    }
}

pub(crate) fn parse_activity_metadata(detail: &str) -> Value {
    let detail = detail.trim();
    if detail.is_empty() {
        return json!({});
    }
    if let Ok(value) = serde_json::from_str::<Value>(detail) {
        if value.is_object() {
            return value;
        }
    }

    let mut metadata = serde_json::Map::new();
    for segment in detail.split([',', '\n']) {
        let Some((key, value)) = segment.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if key.is_empty() || value.is_empty() {
            continue;
        }
        metadata.insert(key.to_owned(), json!(value));
        if key.ends_with("duration") || key == "duration" {
            if let Some(ms) = value
                .split_whitespace()
                .next()
                .and_then(|value| value.parse::<u64>().ok())
            {
                metadata.insert("duration_ms".to_owned(), json!(ms));
            }
        }
    }

    if metadata.is_empty() {
        if Uuid::parse_str(detail).is_ok() {
            metadata.insert("reference_id".to_owned(), json!(detail));
        } else {
            metadata.insert("detail".to_owned(), json!(detail));
        }
    }

    Value::Object(metadata)
}

/// A tool call a run started. Each one adds to the run's totals, and every
/// activity of the run carries those totals as `run_command_count` and
/// `run_file_edit_count` metadata, so a client holding only the latest few
/// activities still shows exact counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunToolCall {
    Command,
    FileEdit,
}

impl RunToolCall {
    /// Runtime adapters record the start of a shell command or file edit with
    /// these kinds. Agent-reported progress notes use other paths and do not count.
    pub(crate) fn from_activity_kind(kind: &str) -> Option<Self> {
        match kind {
            "command" => Some(Self::Command),
            "file_edit" => Some(Self::FileEdit),
            _ => None,
        }
    }

    fn kind(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::FileEdit => "file_edit",
        }
    }
}

pub(crate) async fn record_agent_activity(
    pool: &SqlitePool,
    agent_id: Option<Uuid>,
    run_id: Option<Uuid>,
    kind: &str,
    title: impl AsRef<str>,
    detail: impl AsRef<str>,
) -> CommandResult<()> {
    insert_agent_activity(
        pool,
        agent_id,
        run_id,
        kind,
        title.as_ref(),
        detail.as_ref(),
        None,
    )
    .await
}

/// Records a tool call the run started and counts it toward the run's totals.
/// Never throttled: parallel calls with identical details are still separate
/// calls, and each one must count.
pub(crate) async fn record_run_tool_call(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
    call: RunToolCall,
    title: impl AsRef<str>,
    detail: impl AsRef<str>,
) -> CommandResult<()> {
    insert_agent_activity(
        pool,
        Some(agent_id),
        Some(run_id),
        call.kind(),
        title.as_ref(),
        detail.as_ref(),
        Some(call),
    )
    .await
}

async fn insert_agent_activity(
    pool: &SqlitePool,
    agent_id: Option<Uuid>,
    run_id: Option<Uuid>,
    kind: &str,
    title: &str,
    detail: &str,
    tool_call: Option<RunToolCall>,
) -> CommandResult<()> {
    let agent_handle = match agent_id {
        Some(agent_id) => sqlx::query_scalar("select handle from agents where id = $1")
            .bind(agent_id)
            .fetch_optional(pool)
            .await
            .map_err(to_string)?
            .unwrap_or_else(|| "unknown".to_owned()),
        None => String::new(),
    };
    let phase = activity_phase(kind);
    let status = activity_status(kind, title);
    let summary = title;
    let metadata = parse_activity_metadata(detail);

    let mut transaction = pool.begin().await.map_err(to_string)?;
    let activity_id: Uuid = sqlx::query_scalar(
        r#"
        insert into agent_activities (
            agent_id,
            agent_handle,
            run_id,
            kind,
            phase,
            status,
            title,
            summary,
            detail,
            metadata
        )
        values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
        returning id
        "#,
    )
    .bind(agent_id)
    .bind(agent_handle)
    .bind(run_id)
    .bind(kind)
    .bind(phase)
    .bind(status)
    .bind(title)
    .bind(summary)
    .bind(detail)
    .bind(metadata.to_string())
    .fetch_one(&mut *transaction)
    .await
    .map_err(to_string)?;
    if let Some(run_id) = run_id {
        // The insert above took the write lock, so the totals read here
        // include every tool call other writers have committed.
        stamp_run_tool_call_totals(&mut transaction, activity_id, run_id, tool_call).await?;
    }
    let activity = load_agent_activity_in_tx(&mut transaction, activity_id).await?;
    enqueue_ui_event_in_tx(
        &mut transaction,
        &UiEvent::ActivityUpsert {
            reason: "activity",
            activity: &activity,
        },
    )
    .await?;
    transaction.commit().await.map_err(to_string)?;
    Ok(())
}

async fn stamp_run_tool_call_totals(
    transaction: &mut Transaction<'_, Sqlite>,
    activity_id: Uuid,
    run_id: Uuid,
    tool_call: Option<RunToolCall>,
) -> CommandResult<()> {
    if let Some(call) = tool_call {
        sqlx::query(
            r#"
            update agent_runs
            set command_count = command_count + $2,
                file_edit_count = file_edit_count + $3
            where id = $1
            "#,
        )
        .bind(run_id)
        .bind(i64::from(call == RunToolCall::Command))
        .bind(i64::from(call == RunToolCall::FileEdit))
        .execute(&mut **transaction)
        .await
        .map_err(to_string)?;
    }
    sqlx::query(
        r#"
        update agent_activities
        set metadata = json_set(
            metadata,
            '$.run_command_count', runs.command_count,
            '$.run_file_edit_count', runs.file_edit_count
        )
        from agent_runs runs
        where agent_activities.id = $1
          and runs.id = $2
        "#,
    )
    .bind(activity_id)
    .bind(run_id)
    .execute(&mut **transaction)
    .await
    .map_err(to_string)?;
    Ok(())
}

pub(crate) async fn record_agent_activity_throttled(
    pool: &SqlitePool,
    agent_id: Option<Uuid>,
    run_id: Option<Uuid>,
    kind: &str,
    title: impl AsRef<str>,
    detail: impl AsRef<str>,
) -> CommandResult<()> {
    let title = title.as_ref();
    let detail = detail.as_ref();
    let recently_recorded: bool = sqlx::query_scalar(
        r#"
        select exists (
            select 1
            from agent_activities
            where agent_id is not distinct from $1
              and run_id is not distinct from $2
              and kind = $3
              and title = $4
              and detail = $5
              and julianday(created_at) > julianday(strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-1 second'))
        )
        "#,
    )
    .bind(agent_id)
    .bind(run_id)
    .bind(kind)
    .bind(title)
    .bind(detail)
    .fetch_one(pool)
    .await
    .map_err(to_string)?;

    if recently_recorded {
        return Ok(());
    }

    record_agent_activity(pool, agent_id, run_id, kind, title, detail).await
}

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use sqlx::SqlitePool;
    use uuid::Uuid;

    use super::{
        activity_status, parse_activity_metadata, record_agent_activity, record_run_tool_call,
        RunToolCall,
    };
    use crate::test_support::{drop_test_schema, insert_test_agent, test_pool};

    async fn run_activity_metadata(pool: &SqlitePool, run_id: Uuid) -> Vec<(String, Value)> {
        sqlx::query_as::<_, (String, String)>(
            "select title, metadata from agent_activities where run_id = $1 order by rowid",
        )
        .bind(run_id)
        .fetch_all(pool)
        .await
        .expect("activities")
        .into_iter()
        .map(|(title, metadata)| (title, serde_json::from_str(&metadata).expect("json")))
        .collect()
    }

    #[tokio::test]
    async fn run_activities_carry_the_runs_tool_call_totals() -> Result<(), String> {
        let Some((pool, database_path)) = test_pool().await else {
            return Ok(());
        };
        let agent_id = insert_test_agent(&pool, "counter").await?;
        let run_id: Uuid = sqlx::query_scalar(
            "insert into agent_runs (agent_id, command, status) values ($1, 'claude', 'running') returning id",
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;

        record_agent_activity(
            &pool,
            Some(agent_id),
            Some(run_id),
            "thinking",
            "Thinking",
            "",
        )
        .await?;
        // Parallel calls start with identical details; each one still counts.
        for _ in 0..2 {
            record_run_tool_call(
                &pool,
                agent_id,
                run_id,
                RunToolCall::Command,
                "Running command",
                r#"{"tool":"Bash"}"#,
            )
            .await?;
        }
        record_run_tool_call(
            &pool,
            agent_id,
            run_id,
            RunToolCall::FileEdit,
            "Editing file",
            r#"{"tool":"Edit"}"#,
        )
        .await?;
        // An agent's own progress note may use the command kind; it is not a call.
        record_agent_activity(
            &pool,
            Some(agent_id),
            Some(run_id),
            "command",
            "Running the test suite",
            "cargo test",
        )
        .await?;
        record_agent_activity(
            &pool,
            Some(agent_id),
            None,
            "profile",
            "Profile updated",
            "",
        )
        .await?;

        let totals: Vec<(String, Value, Value)> = run_activity_metadata(&pool, run_id)
            .await
            .into_iter()
            .map(|(title, metadata)| {
                (
                    title,
                    metadata["run_command_count"].clone(),
                    metadata["run_file_edit_count"].clone(),
                )
            })
            .collect();
        assert_eq!(
            totals,
            vec![
                ("Thinking".to_owned(), 0.into(), 0.into()),
                ("Running command".to_owned(), 1.into(), 0.into()),
                ("Running command".to_owned(), 2.into(), 0.into()),
                ("Editing file".to_owned(), 2.into(), 1.into()),
                ("Running the test suite".to_owned(), 2.into(), 1.into()),
            ]
        );
        let (_, tool_metadata) = &run_activity_metadata(&pool, run_id).await[1];
        assert_eq!(tool_metadata["tool"], "Bash");
        let runless: String = sqlx::query_scalar(
            "select metadata from agent_activities where run_id is null and agent_id = $1",
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert!(!runless.contains("run_command_count"));
        let run_totals: (i64, i64) =
            sqlx::query_as("select command_count, file_edit_count from agent_runs where id = $1")
                .bind(run_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(run_totals, (2, 1));

        drop_test_schema(pool, database_path).await;
        Ok(())
    }

    #[test]
    fn structures_activity_metadata_from_detail() {
        let metadata = parse_activity_metadata("pid=123, thread_id=abc, duration=42 ms");
        assert_eq!(metadata["pid"], "123");
        assert_eq!(metadata["thread_id"], "abc");
        assert_eq!(metadata["duration_ms"], 42);
    }

    #[test]
    fn marks_runtime_warning_activity_as_warning_status() {
        assert_eq!(activity_status("run", "Runtime warning"), "warning");
        assert_eq!(
            activity_status("run_retry", "Claude provider retrying"),
            "warning"
        );
        assert_eq!(activity_status("error", "Error output"), "error");
        assert_eq!(
            activity_status("thinking", "Investigating Activity ERROR"),
            "active"
        );
        assert_eq!(
            activity_status("tools", "Checking current changes"),
            "active"
        );
        assert_eq!(
            activity_status("file_edit", "Adjusting progress display"),
            "active"
        );
        assert_eq!(activity_status("command", "Command finished"), "success");
    }
}
