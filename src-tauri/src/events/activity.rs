use serde_json::{json, Value};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::activity_store::load_agent_activity;
use crate::ui_notifications::{notify_ui_activity_upsert, notify_ui_refresh};
use crate::{to_string, CommandResult};

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
    } else if kind == "run_retry" || lowered.contains("warning") {
        "warning"
    } else if lowered.contains("cancel") || lowered.contains("stop") || lowered.contains("stopping")
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
    ) {
        "active"
    } else if lowered.contains("running")
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

pub(crate) async fn record_agent_activity(
    pool: &SqlitePool,
    agent_id: Option<Uuid>,
    run_id: Option<Uuid>,
    kind: &str,
    title: impl AsRef<str>,
    detail: impl AsRef<str>,
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
    let title = title.as_ref();
    let detail = detail.as_ref();
    let phase = activity_phase(kind);
    let status = activity_status(kind, title);
    let summary = title;
    let metadata = parse_activity_metadata(detail);

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
    .fetch_one(pool)
    .await
    .map_err(to_string)?;
    if let Ok(activity) = load_agent_activity(pool, activity_id).await {
        let _ = notify_ui_activity_upsert(pool, &activity, "activity").await;
    } else {
        let _ = notify_ui_refresh(pool, "activity").await;
    }

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
    use super::{activity_status, parse_activity_metadata};

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
