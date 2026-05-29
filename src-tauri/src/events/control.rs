use std::collections::HashSet;

use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::agent_inbox_wake::agent_accepts_new_work;
use crate::agent_memory::{append_agent_memory, append_run_log, compact_agent_memory};
use crate::agent_routing::{
    create_agent_handoff, ensure_agent_channel_member, queue_mentions_as_work_items,
    resolve_agent_by_handle, resolve_agent_handle, resolve_task_for_handoff, MentionDispatchOrigin,
};
use crate::artifact_store::{create_agent_artifact, normalize_artifact_kind};
use crate::attachments::{
    default_attachment_message_body, load_agent_attachment_uploads, AgentAttachmentFile,
};
use crate::channels::{add_agent_to_channel, create_channel_in_pool, normalize_channel_name};
use crate::domain::parse_due_at;
use crate::domain::reminders::{cancel_reminder_in_pool, create_reminder_in_pool};
use crate::events::activity::{normalize_agent_activity_kind, record_agent_activity};
use crate::message_store::{
    insert_agent_attachment_message, insert_agent_handoff_message, insert_agent_message,
    insert_agent_message_with_options,
};
use crate::runtime::streaming::mark_run_work_item_silent;
use crate::task_messages::create_agent_task_thread;
use crate::ui_notifications::{insert_system_message, notify_ui_refresh};
use crate::usage::record_run_usage;
use crate::{
    app::{to_string, CommandResult},
    dispatch_task_assignment_to_agent, resolve_event_channel, resolve_run_reminder_anchor,
    try_claim_unassigned_task,
};

const AGENT_EVENT_PREFIX: &str = "LANTOR_EVENT";
const SILENT_REPLY_PREFIX: &str = "LANTOR_SILENT_REPLY";

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum AgentEvent {
    Message {
        channel: Option<String>,
        channel_id: Option<Uuid>,
        thread_root_id: Option<Uuid>,
        body: String,
        as_task: Option<bool>,
    },
    ChannelMessageCreate {
        channel: Option<String>,
        channel_id: Option<Uuid>,
        thread_root_id: Option<Uuid>,
        body: String,
    },
    Activity {
        kind: Option<String>,
        title: String,
        detail: Option<String>,
    },
    TaskCreate {
        channel: Option<String>,
        channel_id: Option<Uuid>,
        title: String,
        body: Option<String>,
        thread_body: Option<String>,
        assign_self: Option<bool>,
        status: Option<String>,
    },
    TaskStatus {
        task_number: i64,
        status: String,
    },
    TaskClaim {
        task_number: i64,
        assignee_handle: Option<String>,
    },
    TaskHandoff {
        #[serde(alias = "target_handle")]
        target_agent: String,
        #[serde(default)]
        task_number: Option<i64>,
        reason: String,
        #[serde(default)]
        body: Option<String>,
    },
    Silent {
        reason: Option<String>,
    },
    ReminderCreate {
        channel: Option<String>,
        channel_id: Option<Uuid>,
        thread_root_id: Option<Uuid>,
        message_id: Option<Uuid>,
        title: String,
        note: Option<String>,
        #[serde(alias = "when", alias = "dueAt", default)]
        due_at: Option<String>,
        #[serde(alias = "cadence", default)]
        recurrence: Option<String>,
    },
    ReminderCancel {
        reminder_id: Uuid,
    },
    Usage {
        #[serde(default)]
        input_tokens: Option<i64>,
        #[serde(default)]
        output_tokens: Option<i64>,
        #[serde(default)]
        total_tokens: Option<i64>,
        #[serde(default)]
        cost_micros: Option<i64>,
        #[serde(default)]
        cost_usd: Option<f64>,
    },
    MemoryAppend {
        body: String,
    },
    MemoryCompact {
        body: String,
    },
    ChannelCreate {
        name: String,
        description: Option<String>,
        agent_handles: Option<Vec<String>>,
    },
    ChannelInvite {
        channel: Option<String>,
        channel_id: Option<Uuid>,
        agent_handles: Vec<String>,
    },
    ProfileUpdate {
        display_name: Option<String>,
        role: Option<String>,
        avatar: Option<String>,
        description: Option<String>,
    },
    ArtifactCreate {
        channel: Option<String>,
        channel_id: Option<Uuid>,
        thread_root_id: Option<Uuid>,
        kind: String,
        title: String,
        summary: Option<String>,
        content: String,
        metadata: Option<Value>,
    },
    AttachmentCreate {
        channel: Option<String>,
        channel_id: Option<Uuid>,
        thread_root_id: Option<Uuid>,
        body: Option<String>,
        files: Vec<AgentAttachmentFile>,
    },
    HandoffCreate {
        #[serde(alias = "target_handle")]
        target_agent: String,
        channel: Option<String>,
        channel_id: Option<Uuid>,
        thread_root_id: Uuid,
        reason: Option<String>,
        body: String,
    },
}

pub(crate) async fn ingest_agent_event_line(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
    line: &str,
) -> CommandResult<Option<String>> {
    let Some(json) = extract_agent_event_json(line) else {
        return Ok(None);
    };
    if !claim_agent_event(pool, run_id, json).await? {
        return Ok(None);
    }
    let event: AgentEvent = serde_json::from_str(json).map_err(to_string)?;
    handle_agent_event(pool, agent_id, run_id, event)
        .await
        .map(Some)
}

pub(crate) async fn claim_agent_event(
    pool: &SqlitePool,
    run_id: Uuid,
    json: &str,
) -> CommandResult<bool> {
    let event_hash = format!("{:x}", Sha256::digest(json.as_bytes()));
    let inserted: Option<bool> = sqlx::query_scalar(
        r#"
        insert into agent_event_receipts (run_id, event_json, event_hash)
        values ($1, $2, $3)
        on conflict (run_id, event_hash) do nothing
        returning true
        "#,
    )
    .bind(run_id)
    .bind(json)
    .bind(event_hash)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;

    Ok(inserted.unwrap_or(false))
}

pub(crate) async fn replay_agent_events_from_run_log_if_needed(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
) -> CommandResult<usize> {
    let accepted_events: i64 = sqlx::query_scalar(
        r#"
        select count(*)
        from agent_activities
        where run_id = $1
          and kind = 'event'
          and title = 'Stdout event accepted'
        "#,
    )
    .bind(run_id)
    .fetch_one(pool)
    .await
    .map_err(to_string)?;
    if accepted_events > 0 {
        return Ok(0);
    }

    let Some(log): Option<String> = sqlx::query_scalar("select log from agent_runs where id = $1")
        .bind(run_id)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?
    else {
        return Ok(0);
    };

    let mut replayed = 0;
    let mut seen = HashSet::new();
    for line in log.lines() {
        let Some(json) = extract_agent_event_json(line) else {
            continue;
        };
        if !seen.insert(json.to_owned()) {
            continue;
        }
        let result = match serde_json::from_str::<AgentEvent>(json).map_err(to_string) {
            Ok(event) => {
                if claim_agent_event(pool, run_id, json).await? {
                    handle_agent_event(pool, agent_id, run_id, event).await
                } else {
                    continue;
                }
            }
            Err(err) => Err(err),
        };
        match result {
            Ok(note) => {
                replayed += 1;
                append_run_log(pool, run_id, format!("[event-replay] {note}\n")).await?;
                record_agent_activity(
                    pool,
                    Some(agent_id),
                    Some(run_id),
                    "event",
                    "Run log event replayed",
                    note,
                )
                .await?;
            }
            Err(err) => {
                append_run_log(pool, run_id, format!("[event-replay] rejected: {err}\n")).await?;
                record_agent_activity(
                    pool,
                    Some(agent_id),
                    Some(run_id),
                    "event_error",
                    "Run log event rejected",
                    err,
                )
                .await?;
            }
        }
    }

    Ok(replayed)
}

pub(crate) fn extract_agent_event_json(line: &str) -> Option<&str> {
    extract_agent_event_json_with_remainder(line).map(|(json, _)| json)
}

fn extract_agent_event_json_with_remainder(line: &str) -> Option<(&str, &str)> {
    let mut trimmed = line.trim();
    for prefix in ["[stdout] ", "[stderr] "] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            trimmed = rest.trim_start();
            break;
        }
    }
    let payload = strip_agent_event_prefix(trimmed)?;
    if payload.is_empty() {
        return None;
    }
    match complete_json_object_end(payload) {
        Some(end) => Some((&payload[..end], &payload[end..])),
        None => Some((payload.trim(), "")),
    }
}

fn strip_agent_event_prefix(text: &str) -> Option<&str> {
    let rest = text.strip_prefix(AGENT_EVENT_PREFIX)?;
    let payload = match rest.chars().next() {
        None => "",
        Some(ch) if ch.is_whitespace() => rest.trim_start(),
        _ => return None,
    };
    if payload.is_empty() || payload.starts_with('{') {
        Some(payload)
    } else {
        None
    }
}

fn find_agent_event_prefix(text: &str) -> Option<(usize, &str)> {
    let mut offset = 0usize;
    while let Some(found) = text[offset..].find(AGENT_EVENT_PREFIX) {
        let marker_index = offset + found;
        let candidate = &text[marker_index..];
        if let Some(payload) = strip_agent_event_prefix(candidate) {
            return Some((marker_index, payload));
        }
        offset = marker_index + AGENT_EVENT_PREFIX.len();
    }
    None
}

fn split_agent_event_jsons_from_text(
    text: &str,
    strip_incomplete_tail: bool,
) -> (String, Vec<String>) {
    let mut visible = String::new();
    let mut events = Vec::new();
    let mut rest = text;

    while let Some((marker_index, payload)) = find_agent_event_prefix(rest) {
        visible.push_str(&rest[..marker_index]);
        let Some(end) = complete_json_object_end(payload) else {
            if !strip_incomplete_tail {
                visible.push_str(&rest[marker_index..]);
            }
            return (visible.trim().to_owned(), events);
        };
        events.push(payload[..end].to_owned());
        rest = &payload[end..];
    }

    visible.push_str(rest);
    (visible.trim().to_owned(), events)
}

fn complete_json_object_end(value: &str) -> Option<usize> {
    if !value.starts_with('{') {
        return None;
    }

    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, ch) in value.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index + ch.len_utf8());
                }
            }
            _ => {}
        }
    }

    None
}

pub(crate) fn silent_reply_reason(body: &str) -> Option<String> {
    let first_line = body.trim().lines().next()?.trim().trim_matches('`').trim();
    let rest = first_line.strip_prefix(SILENT_REPLY_PREFIX)?;
    if !rest.is_empty()
        && !rest.starts_with(':')
        && !rest
            .chars()
            .next()
            .map(char::is_whitespace)
            .unwrap_or(false)
    {
        return None;
    }
    let reason = rest.trim_start_matches(':').trim();
    Some(reason.to_owned())
}

pub(crate) fn split_streaming_agent_event_lines(body: &str) -> (String, Vec<String>) {
    split_agent_event_jsons_from_text(body, false)
}

pub(crate) fn split_complete_streaming_agent_event_lines(body: &str) -> (String, Vec<String>) {
    split_streaming_agent_event_lines(body)
}

pub(crate) fn split_terminal_streaming_agent_event_lines(body: &str) -> (String, Vec<String>) {
    split_agent_event_jsons_from_text(body, true)
}

fn control_event_creates_visible_chat_message(json: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(json) else {
        return false;
    };
    match value.get("type").and_then(Value::as_str) {
        Some(
            "message"
            | "channel_message_create"
            | "task_create"
            | "task_handoff"
            | "attachment_create"
            | "handoff_create",
        ) => true,
        Some("artifact_create") => {
            let kind_supported = value
                .get("kind")
                .and_then(Value::as_str)
                .is_some_and(|kind| normalize_artifact_kind(kind).is_ok());
            let has_title = value
                .get("title")
                .and_then(Value::as_str)
                .is_some_and(|title| !title.trim().is_empty());
            let has_content = value
                .get("content")
                .and_then(Value::as_str)
                .is_some_and(|content| !content.trim().is_empty());
            kind_supported && has_title && has_content
        }
        _ => false,
    }
}

pub(crate) fn control_event_hides_empty_streaming_reply(json: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(json) else {
        return false;
    };
    value.get("type").and_then(Value::as_str) == Some("silent")
        || control_event_creates_visible_chat_message(json)
}

pub(crate) async fn handle_streaming_agent_event_json(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
    json: &str,
) -> CommandResult<()> {
    match serde_json::from_str::<AgentEvent>(json).map_err(to_string) {
        Ok(event) => {
            if !claim_agent_event(pool, run_id, json).await? {
                return Ok(());
            }
            match handle_agent_event(pool, agent_id, run_id, event).await {
                Ok(note) => {
                    append_run_log(pool, run_id, format!("[stream-event] {note}\n")).await?;
                    record_agent_activity(
                        pool,
                        Some(agent_id),
                        Some(run_id),
                        "event",
                        "Stream event accepted",
                        note,
                    )
                    .await?;
                }
                Err(err) => {
                    append_run_log(pool, run_id, format!("[stream-event] rejected: {err}\n"))
                        .await?;
                    record_agent_activity(
                        pool,
                        Some(agent_id),
                        Some(run_id),
                        "event_error",
                        "Stream event rejected",
                        err,
                    )
                    .await?;
                }
            }
        }
        Err(err) => {
            append_run_log(pool, run_id, format!("[stream-event] rejected: {err}\n")).await?;
            record_agent_activity(
                pool,
                Some(agent_id),
                Some(run_id),
                "event_error",
                "Stream event rejected",
                err,
            )
            .await?;
        }
    }
    Ok(())
}

pub(crate) async fn handle_agent_event(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
    event: AgentEvent,
) -> CommandResult<String> {
    match event {
        AgentEvent::Message {
            channel,
            channel_id,
            thread_root_id,
            body,
            as_task,
        } => {
            let channel_id = resolve_event_channel(pool, channel_id, channel.as_deref()).await?;
            let msg_id = insert_agent_message(
                pool,
                agent_id,
                channel_id,
                thread_root_id,
                body.trim(),
                as_task.unwrap_or(false),
            )
            .await?;
            record_agent_activity(
                pool,
                Some(agent_id),
                None,
                if as_task.unwrap_or(false) {
                    "task"
                } else {
                    "message"
                },
                if as_task.unwrap_or(false) {
                    "Task created from stdout event"
                } else {
                    "Message posted from stdout event"
                },
                format!("message_id={msg_id}"),
            )
            .await?;
            Ok(format!("message accepted {msg_id}"))
        }
        AgentEvent::ChannelMessageCreate {
            channel,
            channel_id,
            thread_root_id,
            body,
        } => {
            let channel_id = resolve_event_channel(pool, channel_id, channel.as_deref()).await?;
            ensure_agent_channel_member(pool, agent_id, channel_id, "channel_message_create")
                .await?;
            let body = body.trim();
            if body.is_empty() {
                return Err("channel_message_create body is required".to_owned());
            }
            let msg_id = insert_agent_message_with_options(
                pool,
                agent_id,
                channel_id,
                thread_root_id,
                body,
                false,
                false,
            )
            .await?;
            queue_mentions_as_work_items(
                pool,
                channel_id,
                thread_root_id,
                msg_id,
                None,
                body,
                MentionDispatchOrigin::Agent {
                    sender_agent_id: agent_id,
                    allow_channel_member_invite: true,
                },
            )
            .await?;
            record_agent_activity(
                pool,
                Some(agent_id),
                None,
                "message",
                "Channel message posted from control event",
                json!({
                    "message_id": msg_id,
                    "channel_id": channel_id,
                    "thread_root_id": thread_root_id
                })
                .to_string(),
            )
            .await?;
            Ok(format!("channel message accepted {msg_id}"))
        }
        AgentEvent::Activity {
            kind,
            title,
            detail,
        } => {
            let title = title.trim();
            if title.is_empty() {
                return Err("activity title is required".to_owned());
            }
            let kind = normalize_agent_activity_kind(kind.as_deref());
            record_agent_activity(
                pool,
                Some(agent_id),
                Some(run_id),
                kind,
                title,
                detail.unwrap_or_default(),
            )
            .await?;
            Ok("activity accepted".to_owned())
        }
        AgentEvent::TaskCreate {
            channel,
            channel_id,
            title,
            body,
            thread_body,
            assign_self,
            status,
        } => {
            let channel_id = resolve_event_channel(pool, channel_id, channel.as_deref()).await?;
            let (task_number, root_message_id, thread_reply_id) = create_agent_task_thread(
                pool,
                agent_id,
                channel_id,
                &title,
                body.as_deref(),
                thread_body.as_deref(),
                assign_self.unwrap_or(true),
                status.as_deref(),
            )
            .await?;
            record_agent_activity(
                pool,
                Some(agent_id),
                Some(run_id),
                "task",
                format!("Task #{task_number} created"),
                json!({
                    "task_number": task_number,
                    "message_id": root_message_id,
                    "thread_reply_id": thread_reply_id,
                })
                .to_string(),
            )
            .await?;
            Ok(format!("task #{task_number} created {root_message_id}"))
        }
        AgentEvent::TaskStatus {
            task_number,
            status,
        } => {
            let status = status.trim();
            if !matches!(status, "todo" | "in_progress" | "in_review" | "done") {
                return Err(format!("unsupported task status: {status}"));
            }
            let affected = sqlx::query(
                r#"
                update tasks
                set status = $2, version = version + 1, updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
                where number = $1
                "#,
            )
            .bind(task_number)
            .bind(status)
            .execute(pool)
            .await
            .map_err(to_string)?
            .rows_affected();
            if affected == 0 {
                return Err(format!("task #{task_number} does not exist"));
            }
            record_agent_activity(
                pool,
                Some(agent_id),
                None,
                "task",
                format!("Task #{task_number} status changed"),
                format!("status={status}"),
            )
            .await?;
            Ok(format!("task #{task_number} status set to {status}"))
        }
        AgentEvent::TaskClaim {
            task_number,
            assignee_handle,
        } => {
            let assignee = match assignee_handle.as_deref().map(str::trim) {
                Some("") | Some("null") | Some("unassigned") => None,
                Some(handle) => Some(resolve_agent_by_handle(pool, handle).await?),
                None => Some(agent_id),
            };
            let task_id: Option<Uuid> =
                sqlx::query_scalar("select id from tasks where number = $1")
                    .bind(task_number)
                    .fetch_optional(pool)
                    .await
                    .map_err(to_string)?;
            let Some(task_id) = task_id else {
                return Err(format!("task #{task_number} does not exist"));
            };
            if assignee.is_none() {
                let affected = sqlx::query(
                    r#"
                    update tasks
                    set assignee_agent_id = null,
                        version = version + 1,
                        updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
                    where id = $1
                      and assignee_agent_id = $2
                      and status <> 'done'
                    "#,
                )
                .bind(task_id)
                .bind(agent_id)
                .execute(pool)
                .await
                .map_err(to_string)?
                .rows_affected();
                if affected == 0 {
                    return Ok(format!("task #{task_number} unclaim ignored"));
                }
                record_agent_activity(
                    pool,
                    Some(agent_id),
                    None,
                    "task",
                    format!("Task #{task_number} unclaimed"),
                    "agent_claim",
                )
                .await?;
            } else if assignee != Some(agent_id) {
                return Err("task_claim can only claim for the current agent".to_owned());
            } else if try_claim_unassigned_task(pool, task_id, agent_id, None, "agent_claim")
                .await?
                .is_none()
            {
                return Ok(format!("task #{task_number} claim ignored"));
            }
            Ok(format!("task #{task_number} assignee updated"))
        }
        AgentEvent::TaskHandoff {
            target_agent,
            task_number,
            reason,
            body,
        } => {
            let (task_id, resolved_task_number, title, channel_id, thread_root_id) =
                resolve_task_for_handoff(pool, agent_id, run_id, task_number).await?;
            let target_agent_id = resolve_agent_by_handle(pool, &target_agent).await?;
            if target_agent_id == agent_id {
                return Err("task_handoff target_agent must be a different agent".to_owned());
            }
            let target_handle = resolve_agent_handle(pool, target_agent_id).await?;
            if !agent_accepts_new_work(pool, target_agent_id).await? {
                return Err(format!(
                    "task_handoff target_agent @{target_handle} is in error state"
                ));
            }
            let source_handle = resolve_agent_handle(pool, agent_id).await?;
            let reason = reason.trim();
            if reason.is_empty() {
                return Err("task_handoff reason is required".to_owned());
            }
            let handoff_body = body
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    format!("@{target_handle} taking over task #{resolved_task_number}: {reason}")
                });

            let affected = sqlx::query(
                r#"
                update tasks
                set assignee_agent_id = $2,
                    status = 'in_progress',
                    version = version + 1,
                    updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
                where id = $1
                  and assignee_agent_id = $3
                  and status <> 'done'
                "#,
            )
            .bind(task_id)
            .bind(target_agent_id)
            .bind(agent_id)
            .execute(pool)
            .await
            .map_err(to_string)?
            .rows_affected();
            if affected == 0 {
                return Err(format!(
                    "task #{resolved_task_number} can only be handed off by its current assignee"
                ));
            }

            let handoff_message_id = insert_agent_handoff_message(
                pool,
                agent_id,
                channel_id,
                thread_root_id,
                &handoff_body,
            )
            .await?;
            dispatch_task_assignment_to_agent(pool, task_id, target_agent_id, reason).await?;
            let _ = notify_ui_refresh(pool, "task_handoff").await;
            record_agent_activity(
                pool,
                Some(agent_id),
                Some(run_id),
                "task",
                format!("Task #{resolved_task_number} handed off"),
                json!({
                    "task_id": task_id,
                    "task_number": resolved_task_number,
                    "title": title,
                    "from": format!("@{source_handle}"),
                    "target_agent": format!("@{target_handle}"),
                    "reason": reason,
                    "message_id": handoff_message_id,
                })
                .to_string(),
            )
            .await?;
            Ok(format!(
                "task #{resolved_task_number} handed off to @{target_handle}"
            ))
        }
        AgentEvent::Silent { reason } => {
            let reason =
                reason.unwrap_or_else(|| "Agent judged no visible reply was needed.".to_owned());
            mark_run_work_item_silent(pool, agent_id, run_id, &reason).await?;
            Ok("silent reply accepted".to_owned())
        }
        AgentEvent::ReminderCreate {
            channel,
            channel_id,
            thread_root_id,
            message_id,
            title,
            note,
            due_at,
            recurrence,
        } => {
            let due_at = due_at.ok_or_else(|| {
                "reminder_create requires a when or due_at ISO8601 timestamp".to_owned()
            })?;
            let due_at = parse_due_at(&due_at)?;
            let (default_channel_id, default_thread_root_id, default_message_id) =
                resolve_run_reminder_anchor(pool, agent_id, run_id).await?;
            let resolved_channel_id = if channel_id.is_some() || channel.is_some() {
                Some(resolve_event_channel(pool, channel_id, channel.as_deref()).await?)
            } else {
                default_channel_id
            };
            let reminder_id = create_reminder_in_pool(
                pool,
                Some(agent_id),
                resolved_channel_id,
                thread_root_id.or(default_thread_root_id),
                message_id.or(default_message_id),
                &title,
                note.as_deref().unwrap_or(""),
                due_at,
                recurrence.as_deref().unwrap_or("none"),
            )
            .await?;
            record_agent_activity(
                pool,
                Some(agent_id),
                Some(run_id),
                "reminder",
                "Reminder scheduled",
                json!({
                    "reminder_id": reminder_id,
                    "title": title.trim(),
                    "due_at": due_at.to_rfc3339(),
                    "recurrence": recurrence.unwrap_or_else(|| "none".to_owned())
                })
                .to_string(),
            )
            .await?;
            Ok(format!("reminder created {reminder_id}"))
        }
        AgentEvent::ReminderCancel { reminder_id } => {
            cancel_reminder_in_pool(pool, reminder_id).await?;
            record_agent_activity(
                pool,
                Some(agent_id),
                Some(run_id),
                "reminder",
                "Reminder cancelled",
                json!({ "reminder_id": reminder_id }).to_string(),
            )
            .await?;
            Ok(format!("reminder cancelled {reminder_id}"))
        }
        AgentEvent::Usage {
            input_tokens,
            output_tokens,
            total_tokens,
            cost_micros,
            cost_usd,
        } => {
            let input_tokens = input_tokens.unwrap_or_default().max(0);
            let mut output_tokens = output_tokens.unwrap_or_default().max(0);
            if output_tokens == 0 {
                if let Some(total_tokens) = total_tokens {
                    output_tokens = (total_tokens - input_tokens).max(0);
                }
            }
            let event_cost_micros = cost_micros
                .or_else(|| cost_usd.map(|value| (value.max(0.0) * 1_000_000.0).round() as i64));
            record_run_usage(
                pool,
                agent_id,
                run_id,
                input_tokens,
                output_tokens,
                event_cost_micros,
            )
            .await?;
            record_agent_activity(
                pool,
                Some(agent_id),
                Some(run_id),
                "usage",
                "Usage recorded",
                json!({
                    "input_tokens": input_tokens,
                    "output_tokens": output_tokens,
                    "cost_micros": event_cost_micros
                })
                .to_string(),
            )
            .await?;
            Ok("usage accepted".to_owned())
        }
        AgentEvent::MemoryAppend { body } => {
            append_agent_memory(pool, agent_id, &body).await?;
            record_agent_activity(
                pool,
                Some(agent_id),
                Some(run_id),
                "memory",
                "Memory updated",
                json!({ "operation": "append" }).to_string(),
            )
            .await?;
            Ok("memory appended".to_owned())
        }
        AgentEvent::MemoryCompact { body } => {
            compact_agent_memory(pool, agent_id, &body).await?;
            record_agent_activity(
                pool,
                Some(agent_id),
                Some(run_id),
                "memory",
                "Memory compacted",
                json!({ "operation": "compact" }).to_string(),
            )
            .await?;
            Ok("memory compacted".to_owned())
        }
        AgentEvent::ChannelCreate {
            name,
            description,
            agent_handles,
        } => {
            let channel_id =
                create_channel_in_pool(pool, &name, description.as_deref().unwrap_or("")).await?;
            add_agent_to_channel(pool, channel_id, agent_id).await?;
            let mut invited = Vec::new();
            for handle in agent_handles.unwrap_or_default() {
                let invited_agent_id = resolve_agent_by_handle(pool, &handle).await?;
                add_agent_to_channel(pool, channel_id, invited_agent_id).await?;
                invited.push(handle.trim().trim_start_matches('@').to_owned());
            }
            insert_system_message(
                pool,
                channel_id,
                None,
                format!(
                    "@{} created #{}{}",
                    sqlx::query_scalar::<_, String>("select handle from agents where id = $1")
                        .bind(agent_id)
                        .fetch_one(pool)
                        .await
                        .map_err(to_string)?,
                    normalize_channel_name(&name),
                    if invited.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " and invited {}",
                            invited
                                .iter()
                                .map(|h| format!("@{h}"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    }
                ),
            )
            .await?;
            record_agent_activity(
                pool,
                Some(agent_id),
                Some(run_id),
                "channel",
                "Channel created",
                json!({
                    "channel_id": channel_id,
                    "name": normalize_channel_name(&name),
                    "invited": invited
                })
                .to_string(),
            )
            .await?;
            Ok(format!("channel created {channel_id}"))
        }
        AgentEvent::ChannelInvite {
            channel,
            channel_id,
            agent_handles,
        } => {
            if agent_handles.is_empty() {
                return Err("channel_invite requires agent_handles".to_owned());
            }
            let channel_id = resolve_event_channel(pool, channel_id, channel.as_deref()).await?;
            let mut invited = Vec::new();
            for handle in agent_handles {
                let invited_agent_id = resolve_agent_by_handle(pool, &handle).await?;
                add_agent_to_channel(pool, channel_id, invited_agent_id).await?;
                invited.push(handle.trim().trim_start_matches('@').to_owned());
            }
            record_agent_activity(
                pool,
                Some(agent_id),
                Some(run_id),
                "membership",
                "Agents invited",
                json!({
                    "channel_id": channel_id,
                    "invited": invited
                })
                .to_string(),
            )
            .await?;
            let _ = notify_ui_refresh(pool, "channel_invite").await;
            Ok("agents invited".to_owned())
        }
        AgentEvent::ProfileUpdate {
            display_name,
            role,
            avatar,
            description,
        } => {
            let display_name = display_name
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let role = role
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let avatar = avatar
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let description = description
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            if display_name.is_none() && role.is_none() && avatar.is_none() && description.is_none()
            {
                return Err("profile_update requires at least one non-empty field".to_owned());
            }
            sqlx::query(
                r#"
                update agents
                set display_name = coalesce($2, display_name),
                    role = coalesce($3, role),
                    avatar = coalesce($4, avatar),
                    description = coalesce($5, description)
                where id = $1
                "#,
            )
            .bind(agent_id)
            .bind(display_name)
            .bind(role)
            .bind(avatar)
            .bind(description)
            .execute(pool)
            .await
            .map_err(to_string)?;
            record_agent_activity(
                pool,
                Some(agent_id),
                Some(run_id),
                "profile",
                "Profile updated",
                json!({
                    "display_name": display_name,
                    "role": role,
                    "avatar": avatar,
                    "description": description
                })
                .to_string(),
            )
            .await?;
            let _ = notify_ui_refresh(pool, "profile_update").await;
            Ok("profile updated".to_owned())
        }
        AgentEvent::ArtifactCreate {
            channel,
            channel_id,
            thread_root_id,
            kind,
            title,
            summary,
            content,
            metadata,
        } => {
            let channel_id = resolve_event_channel(pool, channel_id, channel.as_deref()).await?;
            let (artifact_id, message_id) = create_agent_artifact(
                pool,
                agent_id,
                channel_id,
                thread_root_id,
                &kind,
                &title,
                summary.as_deref(),
                &content,
                metadata,
            )
            .await?;
            record_agent_activity(
                pool,
                Some(agent_id),
                Some(run_id),
                "artifact",
                "Artifact created",
                json!({
                    "artifact_id": artifact_id,
                    "message_id": message_id,
                    "kind": kind,
                    "title": title
                })
                .to_string(),
            )
            .await?;
            Ok(format!("artifact created: {artifact_id}"))
        }
        AgentEvent::AttachmentCreate {
            channel,
            channel_id,
            thread_root_id,
            body,
            files,
        } => {
            let channel_id = resolve_event_channel(pool, channel_id, channel.as_deref()).await?;
            let uploads = load_agent_attachment_uploads(files)?;
            let upload_count = uploads.len();
            let body = body
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| default_attachment_message_body(&uploads));
            let message_id = insert_agent_attachment_message(
                pool,
                agent_id,
                channel_id,
                thread_root_id,
                &body,
                uploads,
            )
            .await?;
            record_agent_activity(
                pool,
                Some(agent_id),
                Some(run_id),
                "attachment",
                "Attachment message created",
                json!({
                    "message_id": message_id,
                    "file_count": upload_count
                })
                .to_string(),
            )
            .await?;
            Ok(format!("attachment message created: {message_id}"))
        }
        AgentEvent::HandoffCreate {
            target_agent,
            channel,
            channel_id,
            thread_root_id,
            reason,
            body,
        } => {
            let channel_id = resolve_event_channel(pool, channel_id, channel.as_deref()).await?;
            let (target_agent_id, target_handle, work_item_id, handoff_message_id) =
                create_agent_handoff(
                    pool,
                    agent_id,
                    channel_id,
                    thread_root_id,
                    &target_agent,
                    reason.as_deref(),
                    &body,
                )
                .await?;
            record_agent_activity(
                pool,
                Some(agent_id),
                Some(run_id),
                "handoff",
                "Handoff created",
                json!({
                    "target_agent_id": target_agent_id,
                    "target_handle": target_handle,
                    "work_item_id": work_item_id,
                    "message_id": handoff_message_id,
                    "thread_root_id": thread_root_id
                })
                .to_string(),
            )
            .await?;
            Ok(format!(
                "handoff created for @{target_handle}: {work_item_id}"
            ))
        }
    }
}

#[cfg(test)]
#[path = "../tests/events_control.rs"]
mod relocated_tests;

#[cfg(test)]
mod tests {
    use super::{
        silent_reply_reason, split_complete_streaming_agent_event_lines,
        split_streaming_agent_event_lines, split_terminal_streaming_agent_event_lines,
    };

    #[test]
    fn keeps_trailing_stream_text_after_control_event() {
        let (visible, events) = split_streaming_agent_event_lines(
            r#"LANTOR_EVENT {"type":"activity","title":"Done","detail":"ok"} ## Review"#,
        );

        assert_eq!(
            events,
            vec![r#"{"type":"activity","title":"Done","detail":"ok"}"#]
        );
        assert_eq!(visible, "## Review");
    }

    #[test]
    fn consumes_inline_streaming_control_events_without_newlines() {
        let (visible, events) = split_streaming_agent_event_lines(
            r#"Working patch.LANTOR_EVENT {"type":"activity","title":"Step","detail":"one"}LANTOR_EVENT {"type":"memory_append","body":"saved"}  Final result"#,
        );

        assert_eq!(
            events,
            vec![
                r#"{"type":"activity","title":"Step","detail":"one"}"#,
                r#"{"type":"memory_append","body":"saved"}"#,
            ]
        );
        assert_eq!(visible, "Working patch.  Final result");
    }

    #[test]
    fn complete_split_consumes_inline_control_events_only_after_newline() {
        let (visible, events) = split_complete_streaming_agent_event_lines(
            "Working patch.LANTOR_EVENT {\"type\":\"activity\",\"title\":\"Step\",\"detail\":\"one\"}\npartial LANTOR_EVENT {\"type\":\"activity\",\"title\":\"Later\",\"detail\":\"two\"}",
        );

        assert_eq!(
            events,
            vec![
                r#"{"type":"activity","title":"Step","detail":"one"}"#,
                r#"{"type":"activity","title":"Later","detail":"two"}"#,
            ]
        );
        assert_eq!(visible, "Working patch.\npartial");
    }

    #[test]
    fn consumes_control_events_when_json_starts_on_next_line() {
        let (visible, events) = split_streaming_agent_event_lines(
            "Status update\nLANTOR_EVENT\n{\"type\":\"activity\",\"title\":\"Step\",\"detail\":\"one\"}\nDone",
        );

        assert_eq!(
            events,
            vec![r#"{"type":"activity","title":"Step","detail":"one"}"#]
        );
        assert_eq!(visible, "Status update\n\nDone");
    }

    #[test]
    fn terminal_split_strips_incomplete_control_tail() {
        let (visible, events) = split_terminal_streaming_agent_event_lines(
            "Working patch.\nLANTOR_EVENT\n{\"type\":\"activity\",\"title\":\"Step\"",
        );

        assert!(events.is_empty());
        assert_eq!(visible, "Working patch.");
    }

    #[test]
    fn strips_control_event_with_multiple_whitespace_chars() {
        let (visible, events) = split_streaming_agent_event_lines(
            "LANTOR_EVENT  \t\r\n {\"type\":\"activity\",\"title\":\"x\"}",
        );

        assert_eq!(visible, "");
        assert_eq!(events, vec![r#"{"type":"activity","title":"x"}"#]);
    }

    #[test]
    fn drops_unclosed_control_event_at_terminal_split() {
        let (visible, events) = split_terminal_streaming_agent_event_lines(
            "Hello there.\nLANTOR_EVENT {\"type\":\"activity\"",
        );

        assert_eq!(visible, "Hello there.");
        assert!(events.is_empty());
    }

    #[test]
    fn keeps_unclosed_control_event_during_incremental_split() {
        let body = "Hello there.\nLANTOR_EVENT {\"type\":\"activity\"";
        let (visible, events) = split_complete_streaming_agent_event_lines(body);

        assert_eq!(visible, body);
        assert!(events.is_empty());
    }

    #[test]
    fn preserves_text_when_event_marker_appears_in_prose() {
        let body = "我刚提到 LANTOR_EVENT 是系统级控制行，看附录。";
        let (visible, events) = split_streaming_agent_event_lines(body);
        let (terminal_visible, terminal_events) = split_terminal_streaming_agent_event_lines(body);

        assert_eq!(visible, body);
        assert!(events.is_empty());
        assert_eq!(terminal_visible, body);
        assert!(terminal_events.is_empty());
    }

    #[test]
    fn complete_split_preserves_visible_blank_lines() {
        let (visible, events) = split_complete_streaming_agent_event_lines("First\n\nSecond\n");

        assert!(events.is_empty());
        assert_eq!(visible, "First\n\nSecond");
    }

    #[test]
    fn detects_silent_reply_marker() {
        assert_eq!(
            silent_reply_reason("LANTOR_SILENT_REPLY: greeting only"),
            Some("greeting only".to_owned())
        );
        assert_eq!(
            silent_reply_reason("LANTOR_SILENT_REPLY"),
            Some(String::new())
        );
        assert_eq!(silent_reply_reason("LANTOR_SILENT_REPLYING"), None);
    }
}
