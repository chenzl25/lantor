use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::agent_inbox_wake::ensure_agent_inbox_wake_work_item;
use crate::agent_routing::queue_agent_message_mentions;
use crate::app::{to_string, CommandResult};
use crate::events::{
    activity::record_agent_activity,
    control::{
        handle_claimed_agent_event_json, handle_streaming_agent_event_json,
        split_complete_streaming_agent_event_lines, split_terminal_streaming_agent_event_lines,
        streaming_agent_event_is_visible_side_effect,
    },
};
use crate::message_store::{insert_agent_message_with_options, load_message};
use crate::ui_notifications::{
    notify_ui_message_upsert, notify_ui_refresh, notify_ui_work_item_changed,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PublishActionKind {
    ReplyText,
    ChannelMessageCreate,
    ArtifactCreate,
    AttachmentCreate,
    InternalControl,
}

impl PublishActionKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ReplyText => "reply_text",
            Self::ChannelMessageCreate => "channel_message_create",
            Self::ArtifactCreate => "artifact_create",
            Self::AttachmentCreate => "attachment_create",
            Self::InternalControl => "internal_control",
        }
    }

    pub(crate) fn is_visible(self) -> bool {
        !matches!(self, Self::InternalControl)
    }
}

/// Shape of a held interruption: either a draft public reply that the agent
/// can rewrite, or a side-effect-only buffer (artifact/attachment/cross-channel
/// message) with no draft body to revise.
///
/// Single source of truth for `interrupted_action` payload, `allowed_actions`,
/// and the backend guard that `resolve_interrupted_action` enforces. Keep this
/// in sync with the prompt rendering in `agent_inbox_wake::context_detail_lines`
/// (which reads `allowed_actions` from the payload).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InterruptedActionKind {
    PublicReply,
    VisibleControlEvent,
}

impl InterruptedActionKind {
    /// Derive the kind from the current buffer contents. A non-empty draft body
    /// means there is something to revise (PublicReply); otherwise the buffer
    /// only carries held side effects with no semantically meaningful "draft"
    /// to rewrite (VisibleControlEvent).
    pub(crate) fn from_buffer(body: &str, _held_visible_events_count: usize) -> Self {
        if body.trim().is_empty() {
            Self::VisibleControlEvent
        } else {
            Self::PublicReply
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::PublicReply => "public_reply",
            Self::VisibleControlEvent => "visible_control_event",
        }
    }

    pub(crate) fn allowed_actions(self) -> &'static [&'static str] {
        match self {
            Self::PublicReply => &["revise", "yield", "force_send"],
            Self::VisibleControlEvent => &["yield", "force_send"],
        }
    }

    /// Whether the given resolve action is semantically valid for this kind.
    /// Accepts `send_as_is` as an alias for `force_send` to match the parser.
    pub(crate) fn allows(self, action: &str) -> bool {
        let normalized = if action == "send_as_is" {
            "force_send"
        } else {
            action
        };
        self.allowed_actions().contains(&normalized)
    }
}

pub(crate) fn control_action_kind_for_event_type(event_type: &str) -> PublishActionKind {
    match event_type {
        "channel_message_create" | "message" | "task_create" | "handoff_create" => {
            PublishActionKind::ChannelMessageCreate
        }
        "artifact_create" => PublishActionKind::ArtifactCreate,
        "attachment_create" => PublishActionKind::AttachmentCreate,
        _ => PublishActionKind::InternalControl,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PublishDecision {
    Allow,
    HoldStale {
        base_version: i64,
        current_version: i64,
    },
    HoldNotOwner {
        current_owner: Uuid,
    },
}

impl PublishDecision {
    pub(crate) fn reason(&self) -> Option<&'static str> {
        match self {
            Self::Allow => None,
            Self::HoldStale { .. } => Some("stale_context"),
            Self::HoldNotOwner { .. } => Some("not_owner"),
        }
    }
}

fn surface(channel_id: Uuid, thread_root_id: Option<Uuid>) -> (&'static str, Uuid) {
    match thread_root_id {
        Some(thread_root_id) => ("thread", thread_root_id),
        None => ("channel", channel_id),
    }
}

pub(crate) async fn current_thread_version(
    pool: &SqlitePool,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
) -> CommandResult<i64> {
    let (surface_kind, surface_id) = surface(channel_id, thread_root_id);
    let version: Option<i64> = sqlx::query_scalar(
        "select version from thread_versions where surface_kind = $1 and surface_id = $2",
    )
    .bind(surface_kind)
    .bind(surface_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;
    Ok(version.unwrap_or(0))
}

pub(crate) async fn bump_thread_version(
    pool: &SqlitePool,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
) -> CommandResult<()> {
    let (surface_kind, surface_id) = surface(channel_id, thread_root_id);
    sqlx::query(
        r#"
        insert into thread_versions (surface_kind, surface_id, version)
        values ($1, $2, 1)
        on conflict(surface_kind, surface_id) do update
        set version = version + 1,
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        "#,
    )
    .bind(surface_kind)
    .bind(surface_id)
    .execute(pool)
    .await
    .map_err(to_string)?;
    Ok(())
}

pub(crate) async fn inbox_payload_with_base_thread_version(
    pool: &SqlitePool,
    payload: &Value,
    channel_id: Option<Uuid>,
    thread_root_id: Option<Uuid>,
) -> CommandResult<Value> {
    let mut payload = payload.clone();
    let Some(channel_id) = channel_id else {
        return Ok(payload);
    };
    let version = current_thread_version(pool, channel_id, thread_root_id).await?;
    if let Some(object) = payload.as_object_mut() {
        object.insert("base_thread_version".to_owned(), json!(version));
    }
    Ok(payload)
}

async fn work_item_base_thread_version(
    pool: &SqlitePool,
    work_item_id: Option<Uuid>,
) -> CommandResult<Option<i64>> {
    let Some(work_item_id) = work_item_id else {
        return Ok(None);
    };
    let payloads: Vec<String> = sqlx::query_scalar(
        r#"
        select i.payload
        from agent_inbox_items i
        where i.work_item_id = $1
        order by i.created_at asc
        "#,
    )
    .bind(work_item_id)
    .fetch_all(pool)
    .await
    .map_err(to_string)?;
    Ok(payloads
        .iter()
        .filter_map(|payload| serde_json::from_str::<Value>(payload).ok())
        .filter_map(|value| value.get("base_thread_version").and_then(Value::as_i64))
        .max())
}

async fn repin_work_item_base_thread_version(
    pool: &SqlitePool,
    work_item_id: Option<Uuid>,
    base_thread_version: i64,
) -> CommandResult<()> {
    let Some(work_item_id) = work_item_id else {
        return Ok(());
    };
    sqlx::query(
        r#"
        update agent_inbox_items
        set payload = json_set(payload, '$.base_thread_version', $2),
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where work_item_id = $1
        "#,
    )
    .bind(work_item_id)
    .bind(base_thread_version)
    .execute(pool)
    .await
    .map_err(to_string)?;
    Ok(())
}

pub(crate) async fn repin_run_work_item_base_thread_version_for_surface(
    pool: &SqlitePool,
    run_id: Uuid,
    fallback_channel_id: Uuid,
    fallback_thread_root_id: Option<Uuid>,
) -> CommandResult<()> {
    let work_item_id = run_work_item_id(pool, run_id).await?;
    let Some(work_item_id) = work_item_id else {
        return Ok(());
    };
    let (channel_id, thread_root_id) = work_item_public_surface(pool, Some(work_item_id))
        .await?
        .unwrap_or((fallback_channel_id, fallback_thread_root_id));
    let current_version = current_thread_version(pool, channel_id, thread_root_id).await?;
    repin_work_item_base_thread_version(pool, Some(work_item_id), current_version).await
}

pub(crate) async fn work_item_public_surface(
    pool: &SqlitePool,
    work_item_id: Option<Uuid>,
) -> CommandResult<Option<(Uuid, Option<Uuid>)>> {
    let Some(work_item_id) = work_item_id else {
        return Ok(None);
    };
    let row = sqlx::query("select channel_id, thread_root_id from agent_work_items where id = $1")
        .bind(work_item_id)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let channel_id: Option<Uuid> = row.get("channel_id");
    Ok(channel_id.map(|channel_id| {
        let thread_root_id: Option<Uuid> = row.get("thread_root_id");
        (channel_id, thread_root_id)
    }))
}

async fn task_owner_for_thread(
    pool: &SqlitePool,
    thread_root_id: Option<Uuid>,
) -> CommandResult<Option<Uuid>> {
    let Some(thread_root_id) = thread_root_id else {
        return Ok(None);
    };
    sqlx::query_scalar("select assignee_agent_id from tasks where message_id = $1")
        .bind(thread_root_id)
        .fetch_optional(pool)
        .await
        .map_err(to_string)
}

pub(crate) async fn can_publish_public_output(
    pool: &SqlitePool,
    agent_id: Uuid,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    work_item_id: Option<Uuid>,
    action_kind: PublishActionKind,
) -> CommandResult<PublishDecision> {
    if !action_kind.is_visible() {
        return Ok(PublishDecision::Allow);
    }
    if let Some(owner_id) = task_owner_for_thread(pool, thread_root_id).await? {
        if owner_id != agent_id {
            return Ok(PublishDecision::HoldNotOwner {
                current_owner: owner_id,
            });
        }
    }

    let current_version = current_thread_version(pool, channel_id, thread_root_id).await?;
    let Some(base_version) = work_item_base_thread_version(pool, work_item_id).await? else {
        return Ok(PublishDecision::Allow);
    };
    if base_version < current_version {
        return Ok(PublishDecision::HoldStale {
            base_version,
            current_version,
        });
    }
    Ok(PublishDecision::Allow)
}

pub(crate) async fn run_work_item_id(
    pool: &SqlitePool,
    run_id: Uuid,
) -> CommandResult<Option<Uuid>> {
    sqlx::query_scalar("select work_item_id from agent_runs where id = $1")
        .bind(run_id)
        .fetch_optional(pool)
        .await
        .map_err(to_string)
        .map(|value| value.flatten())
}

async fn held_buffer_stream_key_for_work(
    pool: &SqlitePool,
    agent_id: Uuid,
    work_item_id: Option<Uuid>,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    except_stream_key: Option<&str>,
) -> CommandResult<Option<String>> {
    let Some(work_item_id) = work_item_id else {
        return Ok(None);
    };
    let row: Option<String> = sqlx::query_scalar(
        r#"
        select stream_key
        from agent_output_buffers
        where agent_id = $1
          and work_item_id = $2
          and channel_id = $3
          and thread_root_id is not distinct from $4
          and state = 'held'
          and ($5 is null or stream_key <> $5)
        order by created_at asc
        limit 1
        "#,
    )
    .bind(agent_id)
    .bind(work_item_id)
    .bind(channel_id)
    .bind(thread_root_id)
    .bind(except_stream_key)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;
    Ok(row)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn hold_visible_control_event(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
    work_item_id: Option<Uuid>,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    action_kind: PublishActionKind,
    event_json: &str,
    decision: PublishDecision,
) -> CommandResult<String> {
    let stream_key = held_buffer_stream_key_for_work(
        pool,
        agent_id,
        work_item_id,
        channel_id,
        thread_root_id,
        None,
    )
    .await?
    .unwrap_or_else(|| format!("{run_id}:event:{}", Uuid::new_v4()));
    let reason = decision.reason().unwrap_or("stale_context");
    let base_version = match decision {
        PublishDecision::HoldStale { base_version, .. } => base_version,
        _ => work_item_base_thread_version(pool, work_item_id)
            .await?
            .unwrap_or(0),
    };
    let current_version = current_thread_version(pool, channel_id, thread_root_id).await?;
    let existing_buffer: Option<(String, String)> = sqlx::query_as(
        "select body, held_visible_events from agent_output_buffers where stream_key = $1 and state = 'held'",
    )
    .bind(&stream_key)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;
    let existing_body = existing_buffer
        .as_ref()
        .map(|(body, _)| body.as_str())
        .unwrap_or("");
    let mut held_visible_events = existing_buffer
        .as_ref()
        .map(|(_, events)| events.as_str())
        .and_then(|value| serde_json::from_str::<Vec<String>>(value).ok())
        .unwrap_or_default();
    held_visible_events.push(event_json.to_owned());
    sqlx::query(
        r#"
        insert into agent_output_buffers (
            stream_key, agent_id, run_id, work_item_id, channel_id, thread_root_id,
            reason, base_version, current_version, body, held_visible_events
        )
        values ($1, $2, $3, $4, $5, $6, $7, $8, $9, '', $10)
        on conflict(stream_key) do update
        set reason = excluded.reason,
            current_version = excluded.current_version,
            held_visible_events = excluded.held_visible_events,
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        "#,
    )
    .bind(&stream_key)
    .bind(agent_id)
    .bind(run_id)
    .bind(work_item_id)
    .bind(channel_id)
    .bind(thread_root_id)
    .bind(reason)
    .bind(base_version)
    .bind(current_version)
    .bind(json!(held_visible_events).to_string())
    .execute(pool)
    .await
    .map_err(to_string)?;
    if let Some(work_item_id) = work_item_id {
        sqlx::query(
            "update agent_work_items set status = 'interrupted', updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') where id = $1 and status not in ('cancelled', 'failed', 'done', 'silent')",
        )
        .bind(work_item_id)
        .execute(pool)
        .await
        .map_err(to_string)?;
        notify_ui_work_item_changed(pool, work_item_id, "work_item_interrupted").await;
    }
    let kind = InterruptedActionKind::from_buffer(existing_body, held_visible_events.len());
    let payload_action_kind = match kind {
        InterruptedActionKind::PublicReply => PublishActionKind::ReplyText.as_str(),
        InterruptedActionKind::VisibleControlEvent => action_kind.as_str(),
    };
    let payload = json!({
        "interrupted_action": kind.as_str(),
        "reason": reason,
        "draft_body": existing_body,
        "base_version": base_version,
        "current_version": current_version,
        "base_thread_version": current_version,
        "stream_key": stream_key,
        "action_kind": payload_action_kind,
        "held_visible_events": held_visible_events,
        "allowed_actions": kind.allowed_actions(),
    });
    upsert_interrupted_action(
        pool,
        agent_id,
        work_item_id,
        channel_id,
        thread_root_id,
        &stream_key,
        reason,
        payload,
    )
    .await?;
    record_agent_activity(
        pool,
        Some(agent_id),
        Some(run_id),
        "decision",
        "Visible control event held",
        json!({
            "reason": reason,
            "action_kind": action_kind.as_str(),
            "stream_key": stream_key,
        })
        .to_string(),
    )
    .await?;
    Ok(format!("{} held {stream_key}", action_kind.as_str()))
}

async fn parse_and_apply_buffer_control_events(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
    body: &str,
    terminal: bool,
) -> CommandResult<(String, Vec<String>)> {
    let (visible_body, event_jsons) = if terminal {
        split_terminal_streaming_agent_event_lines(body)
    } else {
        split_complete_streaming_agent_event_lines(body)
    };
    let mut held_visible_events = Vec::new();
    for event_json in event_jsons {
        if streaming_agent_event_is_visible_side_effect(&event_json) {
            held_visible_events.push(event_json);
        } else {
            handle_streaming_agent_event_json(pool, agent_id, run_id, &event_json).await?;
        }
    }
    Ok((visible_body, held_visible_events))
}

#[allow(clippy::too_many_arguments)]
async fn upsert_interrupted_action(
    pool: &SqlitePool,
    agent_id: Uuid,
    _work_item_id: Option<Uuid>,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    stream_key: &str,
    reason: &str,
    payload: Value,
) -> CommandResult<()> {
    let title = match reason {
        "not_owner" => "Public reply held because another agent owns this task",
        _ => "Public reply held because the thread changed",
    };
    if let Some(existing_id) = sqlx::query_scalar::<_, Uuid>(
        r#"
        select id
        from agent_inbox_items
        where agent_id = $1
          and kind = 'interrupted_action'
          and json_extract(payload, '$.stream_key') = $2
          and state <> 'archived'
        limit 1
        "#,
    )
    .bind(agent_id)
    .bind(stream_key)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?
    {
        sqlx::query(
            r#"
            update agent_inbox_items
            set title = $2,
                body_preview = $3,
                payload = $4,
                work_item_id = null,
                state = 'unread',
                archived_at = null,
                updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
            where id = $1
            "#,
        )
        .bind(existing_id)
        .bind(title)
        .bind(reason)
        .bind(payload.to_string())
        .execute(pool)
        .await
        .map_err(to_string)?;
        let _ = Box::pin(ensure_agent_inbox_wake_work_item(pool, agent_id)).await?;
        return Ok(());
    }

    sqlx::query(
        r#"
        insert into agent_inbox_items (
            agent_id, channel_id, thread_root_id, kind, priority, state,
            title, body_preview, payload
        )
        values ($1, $2, $3, 'interrupted_action', 95, 'unread', $4, $5, $6)
        "#,
    )
    .bind(agent_id)
    .bind(channel_id)
    .bind(thread_root_id)
    .bind(title)
    .bind(reason)
    .bind(payload.to_string())
    .execute(pool)
    .await
    .map_err(to_string)?;
    let _ = Box::pin(ensure_agent_inbox_wake_work_item(pool, agent_id)).await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn hold_streaming_public_output(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
    work_item_id: Option<Uuid>,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    stream_key: &str,
    delta: &str,
    terminal: bool,
    decision: PublishDecision,
) -> CommandResult<Uuid> {
    let buffer_stream_key = if output_buffer_exists(pool, stream_key).await? {
        stream_key.to_owned()
    } else {
        held_buffer_stream_key_for_work(
            pool,
            agent_id,
            work_item_id,
            channel_id,
            thread_root_id,
            Some(stream_key),
        )
        .await?
        .unwrap_or_else(|| stream_key.to_owned())
    };
    let existing: Option<(String, String, String, i64, i64)> = sqlx::query_as(
        "select body, held_visible_events, reason, base_version, current_version from agent_output_buffers where stream_key = $1",
    )
    .bind(&buffer_stream_key)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;
    let existing_reason = existing.as_ref().map(|row| row.2.clone());
    let existing_base_version = existing.as_ref().map(|row| row.3);
    let existing_current_version = existing.as_ref().map(|row| row.4);
    let reason = existing_reason
        .as_deref()
        .or_else(|| decision.reason())
        .unwrap_or("stale_context");
    let base_version = existing_base_version.unwrap_or(match decision {
        PublishDecision::HoldStale { base_version, .. } => base_version,
        _ => work_item_base_thread_version(pool, work_item_id)
            .await?
            .unwrap_or(0),
    });
    let current_version = existing_current_version
        .unwrap_or(current_thread_version(pool, channel_id, thread_root_id).await?);

    let (existing_body, existing_held_events) = existing
        .map(|(body, events, _, _, _)| (body, events))
        .unwrap_or_default();
    let combined_body = if !delta.is_empty() && existing_body == delta {
        existing_body
    } else {
        format!("{existing_body}{delta}")
    };
    let (visible_body, mut held_visible_events) =
        parse_and_apply_buffer_control_events(pool, agent_id, run_id, &combined_body, terminal)
            .await?;
    let mut previous_held_events =
        serde_json::from_str::<Vec<String>>(&existing_held_events).unwrap_or_default();
    previous_held_events.append(&mut held_visible_events);
    let held_visible_events = previous_held_events;
    let buffer_id = Uuid::new_v4();
    sqlx::query(
        r#"
        insert into agent_output_buffers (
            stream_key, agent_id, run_id, work_item_id, channel_id, thread_root_id,
            reason, base_version, current_version, body, held_visible_events
        )
        values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
        on conflict(stream_key) do update
        set reason = excluded.reason,
            current_version = excluded.current_version,
            body = excluded.body,
            held_visible_events = excluded.held_visible_events,
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        "#,
    )
    .bind(&buffer_stream_key)
    .bind(agent_id)
    .bind(run_id)
    .bind(work_item_id)
    .bind(channel_id)
    .bind(thread_root_id)
    .bind(reason)
    .bind(base_version)
    .bind(current_version)
    .bind(&visible_body)
    .bind(json!(held_visible_events).to_string())
    .execute(pool)
    .await
    .map_err(to_string)?;

    if let Some(work_item_id) = work_item_id {
        sqlx::query(
            r#"
            update agent_work_items
            set status = 'interrupted',
                updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
            where id = $1
              and status not in ('cancelled', 'failed', 'done', 'silent')
            "#,
        )
        .bind(work_item_id)
        .execute(pool)
        .await
        .map_err(to_string)?;
        notify_ui_work_item_changed(pool, work_item_id, "work_item_interrupted").await;
    }

    let kind = InterruptedActionKind::from_buffer(&visible_body, held_visible_events.len());
    let payload = json!({
        "interrupted_action": kind.as_str(),
        "reason": reason,
        "draft_body": visible_body,
        "base_version": base_version,
        "current_version": current_version,
        "base_thread_version": current_version,
        "stream_key": buffer_stream_key,
        "action_kind": PublishActionKind::ReplyText.as_str(),
        "held_visible_events": held_visible_events,
        "allowed_actions": kind.allowed_actions(),
    });
    upsert_interrupted_action(
        pool,
        agent_id,
        work_item_id,
        channel_id,
        thread_root_id,
        &buffer_stream_key,
        reason,
        payload,
    )
    .await?;
    record_agent_activity(
        pool,
        Some(agent_id),
        Some(run_id),
        "decision",
        "Public reply held",
        json!({
            "reason": reason,
            "work_item_id": work_item_id,
            "stream_key": buffer_stream_key,
            "base_version": base_version,
            "current_version": current_version,
        })
        .to_string(),
    )
    .await?;
    Ok(buffer_id)
}

pub(crate) async fn consume_accumulated_control_only_output(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
    stream_key: &str,
    delta: &str,
) -> CommandResult<Option<Uuid>> {
    let Some(row) = sqlx::query(
        "select body from agent_output_buffers where stream_key = $1 and agent_id = $2 and state = 'held'",
    )
    .bind(stream_key)
    .bind(agent_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?
    else {
        return Ok(None);
    };
    let existing_body: String = row.get("body");
    let combined_body = if !delta.is_empty() && existing_body == delta {
        existing_body
    } else {
        format!("{existing_body}{delta}")
    };
    let (visible_body, event_jsons) = split_complete_streaming_agent_event_lines(&combined_body);
    if event_jsons.is_empty() || !visible_body.trim().is_empty() {
        return Ok(None);
    }
    if event_jsons
        .iter()
        .any(|json| streaming_agent_event_is_visible_side_effect(json))
    {
        return Ok(None);
    }

    for event_json in &event_jsons {
        handle_streaming_agent_event_json(pool, agent_id, run_id, event_json).await?;
    }

    sqlx::query(
        r#"
        update agent_output_buffers
        set state = 'yielded',
            body = '',
            held_visible_events = '[]',
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where stream_key = $1 and agent_id = $2 and state = 'held'
        "#,
    )
    .bind(stream_key)
    .bind(agent_id)
    .execute(pool)
    .await
    .map_err(to_string)?;
    sqlx::query(
        r#"
        update agent_inbox_items
        set state = 'archived',
            archived_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where agent_id = $1
          and kind = 'interrupted_action'
          and json_extract(payload, '$.stream_key') = $2
          and state <> 'archived'
        "#,
    )
    .bind(agent_id)
    .bind(stream_key)
    .execute(pool)
    .await
    .map_err(to_string)?;
    record_agent_activity(
        pool,
        Some(agent_id),
        Some(run_id),
        "decision",
        "Control-only held output consumed",
        json!({ "stream_key": stream_key }).to_string(),
    )
    .await?;
    Ok(Some(Uuid::new_v4()))
}

pub(crate) async fn output_buffer_exists(
    pool: &SqlitePool,
    stream_key: &str,
) -> CommandResult<bool> {
    let exists: bool = sqlx::query_scalar(
        "select exists(select 1 from agent_output_buffers where stream_key = $1 and state = 'held')",
    )
    .bind(stream_key)
    .fetch_one(pool)
    .await
    .map_err(to_string)?;
    Ok(exists)
}

pub(crate) async fn resolve_interrupted_action(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
    stream_key: &str,
    action: &str,
) -> CommandResult<String> {
    let requested_terminal_state = match action {
        "yield" => Some("yielded"),
        "revise" => Some("revised"),
        "force_send" | "send_as_is" => Some("force_sent"),
        _ => None,
    };
    if let Some(state) = sqlx::query_scalar::<_, String>(
        r#"
        select state
        from agent_output_buffers
        where stream_key = $1 and agent_id = $2 and state in ('yielded', 'revised', 'force_sent')
        "#,
    )
    .bind(stream_key)
    .bind(agent_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?
    {
        if requested_terminal_state == Some(state.as_str()) {
            return Ok(format!("interrupted action already resolved as {state}"));
        }
        return Err(format!(
            "interrupted action already resolved as {state}; cannot apply action '{action}'"
        ));
    }

    let Some(row) = sqlx::query(
        r#"
        select work_item_id, channel_id, thread_root_id, body, held_visible_events, current_version
        from agent_output_buffers
        where stream_key = $1 and agent_id = $2 and state = 'held'
        "#,
    )
    .bind(stream_key)
    .bind(agent_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?
    else {
        return Err("interrupted action buffer not found".to_owned());
    };
    let work_item_id: Option<Uuid> = row.get("work_item_id");
    let channel_id: Uuid = row.get("channel_id");
    let thread_root_id: Option<Uuid> = row.get("thread_root_id");
    let body: String = row.get("body");
    let held_visible_events: String = row.get("held_visible_events");
    let current_version: i64 = row.get("current_version");

    // Reject actions that are not semantically valid for this buffer shape
    // *before* mutating any state. In particular, a side-effect-only buffer
    // (no draft body) must not accept `revise`, otherwise the resolve path
    // would clear `held_visible_events` and silently drop the side effects.
    let held_events_count = serde_json::from_str::<Vec<String>>(&held_visible_events)
        .map(|events| events.len())
        .unwrap_or(0);
    let kind = InterruptedActionKind::from_buffer(&body, held_events_count);
    if !kind.allows(action) {
        record_agent_activity(
            pool,
            Some(agent_id),
            Some(run_id),
            "decision",
            "Interrupted action resolve rejected",
            json!({
                "stream_key": stream_key,
                "action": action,
                "interrupted_action": kind.as_str(),
                "allowed_actions": kind.allowed_actions(),
            })
            .to_string(),
        )
        .await?;
        return Err(format!(
            "action '{action}' is not allowed for interrupted_action '{}'; allowed: {}",
            kind.as_str(),
            kind.allowed_actions().join(", "),
        ));
    }

    match action {
        "yield" => {
            sqlx::query(
                "update agent_output_buffers set state = 'yielded', updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') where stream_key = $1",
            )
            .bind(stream_key)
            .execute(pool)
            .await
            .map_err(to_string)?;
            if let Some(work_item_id) = work_item_id {
                sqlx::query(
                    r#"
                    update agent_work_items
                    set status = 'silent',
                        completed_at = coalesce(completed_at, strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
                        updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
                    where id = $1
                    "#,
                )
                .bind(work_item_id)
                .execute(pool)
                .await
                .map_err(to_string)?;
                notify_ui_work_item_changed(pool, work_item_id, "interrupted_action_yielded").await;
            }
        }
        "revise" => {
            repin_work_item_base_thread_version(pool, work_item_id, current_version).await?;
            sqlx::query(
                "update agent_output_buffers set state = 'revised', body = '', held_visible_events = '[]', updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') where stream_key = $1",
            )
            .bind(stream_key)
            .execute(pool)
            .await
            .map_err(to_string)?;
            if let Some(work_item_id) = work_item_id {
                sqlx::query(
                    r#"
                    update agent_work_items
                    set status = 'done',
                        completed_at = coalesce(completed_at, strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
                        updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
                    where id = $1
                    "#,
                )
                .bind(work_item_id)
                .execute(pool)
                .await
                .map_err(to_string)?;
                notify_ui_work_item_changed(pool, work_item_id, "interrupted_action_revised").await;
            }
        }
        "force_send" | "send_as_is" => {
            if !body.trim().is_empty() {
                let msg_id = insert_agent_message_with_options(
                    pool,
                    agent_id,
                    channel_id,
                    thread_root_id,
                    body.trim(),
                    false,
                    false,
                )
                .await?;
                queue_agent_message_mentions(pool, msg_id).await?;
                if let Ok(message) = load_message(pool, msg_id).await {
                    let _ =
                        notify_ui_message_upsert(pool, &message, "interrupted_force_send").await;
                } else {
                    let _ = notify_ui_refresh(pool, "interrupted_force_send").await;
                }
            }
            for event_json in
                serde_json::from_str::<Vec<String>>(&held_visible_events).unwrap_or_default()
            {
                handle_claimed_agent_event_json(pool, agent_id, run_id, &event_json).await?;
            }
            sqlx::query(
                "update agent_output_buffers set state = 'force_sent', updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') where stream_key = $1",
            )
            .bind(stream_key)
            .execute(pool)
            .await
            .map_err(to_string)?;
            if let Some(work_item_id) = work_item_id {
                sqlx::query(
                    r#"
                    update agent_work_items
                    set status = 'done',
                        completed_at = coalesce(completed_at, strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
                        updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
                    where id = $1
                    "#,
                )
                .bind(work_item_id)
                .execute(pool)
                .await
                .map_err(to_string)?;
                notify_ui_work_item_changed(pool, work_item_id, "interrupted_action_force_sent")
                    .await;
            }
        }
        other => return Err(format!("unsupported interrupted action resolve: {other}")),
    }

    sqlx::query(
        r#"
        update agent_inbox_items
        set state = 'archived',
            archived_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where agent_id = $1
          and kind = 'interrupted_action'
          and json_extract(payload, '$.stream_key') = $2
          and state <> 'archived'
        "#,
    )
    .bind(agent_id)
    .bind(stream_key)
    .execute(pool)
    .await
    .map_err(to_string)?;
    record_agent_activity(
        pool,
        Some(agent_id),
        Some(run_id),
        "decision",
        "Interrupted action resolved",
        json!({ "stream_key": stream_key, "action": action }).to_string(),
    )
    .await?;
    Ok(format!("interrupted action {action} accepted"))
}
