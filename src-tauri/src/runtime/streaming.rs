use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

use serde_json::json;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::agent_routing::queue_agent_message_mentions;
use crate::app::{to_string, CommandResult};
use crate::events::{
    activity::record_agent_activity,
    control::{
        control_event_hides_empty_streaming_reply, handle_streaming_agent_event_json,
        silent_reply_reason, split_terminal_streaming_agent_event_lines, StreamControlGate,
    },
};
use crate::freshness::{
    hold_work_item_output_if_stale, stale_output_for_work_item,
    try_complete_streaming_message_if_fresh,
};
use crate::message_store::load_message_patch_in_tx;
use crate::ui_notifications::{
    enqueue_ui_event_in_tx, enqueue_ui_work_item_changed_in_tx, reconcile_work_item_change, UiEvent,
};

struct PendingStream {
    gate: StreamControlGate,
    agent_id: Uuid,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    hide_empty_reply: bool,
    has_visible_text: bool,
    queued_events: Vec<String>,
}

type StreamBufferKey = (PathBuf, String);
fn stream_buffers() -> &'static Mutex<HashMap<StreamBufferKey, PendingStream>> {
    static BUFFERS: OnceLock<Mutex<HashMap<StreamBufferKey, PendingStream>>> = OnceLock::new();
    BUFFERS.get_or_init(|| Mutex::new(HashMap::new()))
}
fn stream_buffer_key(pool: &SqlitePool, stream_key: &str) -> StreamBufferKey {
    (
        pool.connect_options().get_filename().to_path_buf(),
        stream_key.to_owned(),
    )
}

pub(crate) const STREAMING_MESSAGE_BODY_LIMIT: usize = 200_000;
pub(crate) const STREAMING_TRUNCATION_MARKER: &str = "\n\n[stream truncated by Lantor]";

pub(crate) fn capped_stream_delta(delta: &str, current_len: usize) -> (String, bool) {
    if current_len >= STREAMING_MESSAGE_BODY_LIMIT {
        return (String::new(), true);
    }
    let remaining = STREAMING_MESSAGE_BODY_LIMIT - current_len;
    let delta_len = delta.chars().count();
    if delta_len <= remaining {
        return (delta.to_owned(), false);
    }

    let marker_len = STREAMING_TRUNCATION_MARKER.chars().count();
    let keep = remaining.saturating_sub(marker_len);
    let mut capped: String = delta.chars().take(keep).collect();
    if remaining >= marker_len {
        capped.push_str(STREAMING_TRUNCATION_MARKER);
    }
    (capped, true)
}

pub(crate) async fn append_streaming_agent_message(
    pool: &SqlitePool,
    agent_id: Uuid,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    stream_key: &str,
    delta: &str,
) -> CommandResult<Uuid> {
    append_streaming_agent_message_inner(
        pool,
        agent_id,
        channel_id,
        thread_root_id,
        stream_key,
        delta,
        true,
    )
    .await
}

pub(crate) async fn append_streaming_agent_message_deferred_completion(
    pool: &SqlitePool,
    agent_id: Uuid,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    stream_key: &str,
    delta: &str,
) -> CommandResult<Uuid> {
    append_streaming_agent_message_inner(
        pool,
        agent_id,
        channel_id,
        thread_root_id,
        stream_key,
        delta,
        false,
    )
    .await
}

async fn append_streaming_agent_message_inner(
    pool: &SqlitePool,
    agent_id: Uuid,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    stream_key: &str,
    delta: &str,
    complete_on_truncation: bool,
) -> CommandResult<Uuid> {
    if stream_key.trim().is_empty() {
        return Err("stream_key is empty".to_owned());
    }
    let control_context = load_streaming_control_context(pool, stream_key).await?;
    if let Some(row) = sqlx::query("select id, delivery_state from messages where stream_key = $1")
        .bind(stream_key)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?
    {
        if row.get::<String, _>("delivery_state") != "streaming" {
            return Ok(row.get("id"));
        }
    }
    if delta.is_empty() {
        return ensure_streaming_agent_message(
            pool,
            agent_id,
            channel_id,
            thread_root_id,
            stream_key,
        )
        .await;
    }
    let (output, hide_empty_reply) = {
        let mut buffers = stream_buffers().lock().unwrap();
        let state = buffers
            .entry(stream_buffer_key(pool, stream_key))
            .or_insert_with(|| PendingStream {
                gate: StreamControlGate::new(true),
                agent_id,
                channel_id,
                thread_root_id,
                hide_empty_reply: false,
                has_visible_text: false,
                queued_events: Vec::new(),
            });
        let mut output = state.gate.push(delta);
        state.has_visible_text |= !output.visible.trim().is_empty();
        if !state.has_visible_text {
            output.visible.clear();
        }
        state.hide_empty_reply |= output
            .events
            .iter()
            .any(|json| control_event_hides_empty_streaming_reply(json));
        if control_context.is_none() {
            state.queued_events.append(&mut output.events);
        } else {
            output.events.splice(..0, state.queued_events.drain(..));
        }
        (output, state.hide_empty_reply)
    };
    // Persist and broadcast only visible text. Pending controls never touch the
    // message body or message outbox, even when split across arbitrary deltas.
    let message_id = append_visible_streaming_agent_message(
        pool,
        agent_id,
        channel_id,
        thread_root_id,
        stream_key,
        &output.visible,
        complete_on_truncation,
    )
    .await?;
    if let Some((control_agent_id, run_id, _)) = control_context {
        for json in output.events {
            handle_streaming_agent_event_json(pool, control_agent_id, run_id, &json).await?;
        }
    }
    if control_context.is_some()
        && hide_empty_reply
        && stored_streaming_message_body_is_empty(pool, stream_key).await?
    {
        delete_streaming_agent_message(pool, message_id, "stream_event_consumed").await?;
    }
    Ok(message_id)
}

async fn append_visible_streaming_agent_message(
    pool: &SqlitePool,
    agent_id: Uuid,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    stream_key: &str,
    delta: &str,
    complete_on_truncation: bool,
) -> CommandResult<Uuid> {
    if stream_key.trim().is_empty() {
        return Err("stream_key is empty".to_owned());
    }
    if delta.is_empty() {
        return ensure_streaming_agent_message(
            pool,
            agent_id,
            channel_id,
            thread_root_id,
            stream_key,
        )
        .await;
    }

    if let Some(row) = sqlx::query(
        "select id, delivery_state, length(body) as body_len from messages where stream_key = $1",
    )
    .bind(stream_key)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?
    {
        let message_id: Uuid = row.get("id");
        let delivery_state: String = row.get("delivery_state");
        if delivery_state != "streaming" {
            return Ok(message_id);
        }
        let body_len: i32 = row.get("body_len");
        let (append_delta, truncated) = capped_stream_delta(delta, body_len.max(0) as usize);
        if append_delta.is_empty() && truncated {
            if complete_on_truncation {
                finish_streaming_agent_message(pool, stream_key, "complete").await?;
            }
            return Ok(message_id);
        }
        let delivery_state = if truncated && complete_on_truncation {
            "complete"
        } else {
            "streaming"
        };
        let mut transaction = pool.begin().await.map_err(to_string)?;
        sqlx::query("update messages set body = body || $2, delivery_state = $3 where id = $1")
            .bind(message_id)
            .bind(&append_delta)
            .bind(delivery_state)
            .execute(&mut *transaction)
            .await
            .map_err(to_string)?;
        enqueue_ui_event_in_tx(
            &mut transaction,
            &UiEvent::MessageDelta {
                reason: "stream_delta",
                message_id,
                append: &append_delta,
                body_length: body_len.max(0) as usize + append_delta.chars().count(),
                delivery_state,
            },
        )
        .await?;
        transaction.commit().await.map_err(to_string)?;
        if truncated && complete_on_truncation {
            queue_agent_message_mentions(pool, message_id).await?;
        }
        return Ok(message_id);
    }

    delete_superseded_empty_run_progress_messages(
        pool,
        agent_id,
        channel_id,
        thread_root_id,
        stream_key,
    )
    .await?;

    let sender = sqlx::query("select display_name, role from agents where id = $1")
        .bind(agent_id)
        .fetch_one(pool)
        .await
        .map_err(to_string)?;
    let sender_name: String = sender.get("display_name");
    let sender_role: String = sender.get("role");
    let (initial_body, truncated) = capped_stream_delta(delta, 0);
    let delivery_state = if truncated && complete_on_truncation {
        "complete"
    } else {
        "streaming"
    };

    let mut transaction = pool.begin().await.map_err(to_string)?;
    let message_id: Uuid = sqlx::query_scalar(
        r#"
        insert into messages (
            channel_id,
            thread_root_id,
            sender_agent_id,
            sender_name,
            sender_role,
            body,
            is_task,
            delivery_state,
            stream_key
        )
        values ($1, $2, $3, $4, $5, $6, false, $7, $8)
        returning id
        "#,
    )
    .bind(channel_id)
    .bind(thread_root_id)
    .bind(agent_id)
    .bind(sender_name)
    .bind(sender_role)
    .bind(initial_body)
    .bind(delivery_state)
    .bind(stream_key)
    .fetch_one(&mut *transaction)
    .await
    .map_err(to_string)?;

    let message = load_message_patch_in_tx(&mut transaction, message_id).await?;
    enqueue_ui_event_in_tx(
        &mut transaction,
        &UiEvent::MessageUpsert {
            reason: "stream_start",
            message: &message,
        },
    )
    .await?;
    transaction.commit().await.map_err(to_string)?;
    if truncated && complete_on_truncation {
        queue_agent_message_mentions(pool, message_id).await?;
    }
    Ok(message_id)
}

/// Claude reuses a stream key across separate text blocks. Retain the logical
/// boundary so a control at the next block's start cannot join prior prose.
pub(crate) async fn start_streaming_agent_text_block(
    pool: &SqlitePool,
    agent_id: Uuid,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    stream_key: &str,
) -> CommandResult<()> {
    let context = load_streaming_control_context(pool, stream_key).await?;
    flush_stream_control_buffer(pool, context, stream_key).await?;
    if !stored_streaming_message_body_is_empty(pool, stream_key).await? {
        append_visible_streaming_agent_message(
            pool,
            agent_id,
            channel_id,
            thread_root_id,
            stream_key,
            "\n\n",
            false,
        )
        .await?;
    }
    Ok(())
}

pub(crate) async fn ensure_streaming_agent_message(
    pool: &SqlitePool,
    agent_id: Uuid,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    stream_key: &str,
) -> CommandResult<Uuid> {
    if stream_key.trim().is_empty() {
        return Err("stream_key is empty".to_owned());
    }

    if let Some(existing) = sqlx::query_scalar("select id from messages where stream_key = $1")
        .bind(stream_key)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?
    {
        return Ok(existing);
    }

    let sender = sqlx::query("select display_name, role from agents where id = $1")
        .bind(agent_id)
        .fetch_one(pool)
        .await
        .map_err(to_string)?;
    let sender_name: String = sender.get("display_name");
    let sender_role: String = sender.get("role");
    let mut transaction = pool.begin().await.map_err(to_string)?;
    let message_id: Uuid = sqlx::query_scalar(
        r#"
        insert into messages (
            channel_id,
            thread_root_id,
            sender_agent_id,
            sender_name,
            sender_role,
            body,
            is_task,
            delivery_state,
            stream_key
        )
        values ($1, $2, $3, $4, $5, '', false, 'streaming', $6)
        returning id
        "#,
    )
    .bind(channel_id)
    .bind(thread_root_id)
    .bind(agent_id)
    .bind(sender_name)
    .bind(sender_role)
    .bind(stream_key)
    .fetch_one(&mut *transaction)
    .await
    .map_err(to_string)?;

    let message = load_message_patch_in_tx(&mut transaction, message_id).await?;
    enqueue_ui_event_in_tx(
        &mut transaction,
        &UiEvent::MessageUpsert {
            reason: "stream_placeholder",
            message: &message,
        },
    )
    .await?;
    transaction.commit().await.map_err(to_string)?;
    Ok(message_id)
}

pub(crate) async fn adopt_streaming_agent_message_key(
    pool: &SqlitePool,
    pending_stream_key: &str,
    stream_key: &str,
) -> CommandResult<Option<Uuid>> {
    if pending_stream_key == stream_key {
        return Ok(None);
    }
    if streaming_message_exists(pool, stream_key).await? {
        return Ok(None);
    }

    let mut transaction = pool.begin().await.map_err(to_string)?;
    let message_id: Option<Uuid> = sqlx::query_scalar(
        r#"
        update messages
        set stream_key = $2,
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where stream_key = $1
          and delivery_state = 'streaming'
          and body = ''
        returning id
        "#,
    )
    .bind(pending_stream_key)
    .bind(stream_key)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(to_string)?;

    if let Some(message_id) = message_id {
        let message = load_message_patch_in_tx(&mut transaction, message_id).await?;
        enqueue_ui_event_in_tx(
            &mut transaction,
            &UiEvent::MessageUpsert {
                reason: "stream_key_adopted",
                message: &message,
            },
        )
        .await?;
    }
    transaction.commit().await.map_err(to_string)?;
    if message_id.is_some() {
        let mut buffers = stream_buffers().lock().unwrap();
        if let Some(pending) = buffers.remove(&stream_buffer_key(pool, pending_stream_key)) {
            buffers.insert(stream_buffer_key(pool, stream_key), pending);
        }
    }
    Ok(message_id)
}

pub(crate) async fn streaming_message_body_is_empty(
    pool: &SqlitePool,
    stream_key: &str,
) -> CommandResult<bool> {
    if stream_buffers()
        .lock()
        .unwrap()
        .contains_key(&stream_buffer_key(pool, stream_key))
    {
        return Ok(false);
    }
    stored_streaming_message_body_is_empty(pool, stream_key).await
}

async fn stored_streaming_message_body_is_empty(
    pool: &SqlitePool,
    stream_key: &str,
) -> CommandResult<bool> {
    let body: Option<String> =
        sqlx::query_scalar("select body from messages where stream_key = $1")
            .bind(stream_key)
            .fetch_optional(pool)
            .await
            .map_err(to_string)?;
    Ok(body.map(|body| body.is_empty()).unwrap_or(true))
}

async fn delete_streaming_agent_message(
    pool: &SqlitePool,
    message_id: Uuid,
    reason: &str,
) -> CommandResult<()> {
    if reason != "stream_event_consumed" {
        if let Some(key) =
            sqlx::query_scalar::<_, String>("select stream_key from messages where id=$1")
                .bind(message_id)
                .fetch_optional(pool)
                .await
                .map_err(to_string)?
        {
            stream_buffers()
                .lock()
                .unwrap()
                .remove(&stream_buffer_key(pool, &key));
        }
    }
    let mut transaction = pool.begin().await.map_err(to_string)?;
    sqlx::query("delete from messages where id = $1")
        .bind(message_id)
        .execute(&mut *transaction)
        .await
        .map_err(to_string)?;
    enqueue_ui_event_in_tx(
        &mut transaction,
        &UiEvent::MessageDelete { reason, message_id },
    )
    .await?;
    transaction.commit().await.map_err(to_string)?;
    Ok(())
}

pub(crate) async fn delete_streaming_agent_message_by_key(
    pool: &SqlitePool,
    stream_key: &str,
    reason: &str,
) -> CommandResult<()> {
    stream_buffers()
        .lock()
        .unwrap()
        .remove(&stream_buffer_key(pool, stream_key));
    let message_id: Option<Uuid> =
        sqlx::query_scalar("select id from messages where stream_key = $1")
            .bind(stream_key)
            .fetch_optional(pool)
            .await
            .map_err(to_string)?;
    if let Some(message_id) = message_id {
        delete_streaming_agent_message(pool, message_id, reason).await?;
    }
    Ok(())
}

async fn delete_superseded_empty_run_progress_messages(
    pool: &SqlitePool,
    agent_id: Uuid,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    stream_key: &str,
) -> CommandResult<()> {
    let Some(run_prefix) = stream_key
        .split(':')
        .next()
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    if Uuid::parse_str(run_prefix).is_err() {
        return Ok(());
    }

    let superseded_ids: Vec<Uuid> = sqlx::query_scalar(
        r#"
        select id
        from messages
        where sender_agent_id = $1
          and channel_id = $2
          and thread_root_id is not distinct from $3
          and stream_key <> $4
          and stream_key like $5
          and body = ''
          and delivery_state in ('streaming', 'complete')
        "#,
    )
    .bind(agent_id)
    .bind(channel_id)
    .bind(thread_root_id)
    .bind(stream_key)
    .bind(format!("{run_prefix}:%"))
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    for message_id in superseded_ids {
        delete_streaming_agent_message(pool, message_id, "superseded_progress_status").await?;
    }
    Ok(())
}

pub(crate) async fn finish_streaming_agent_message(
    pool: &SqlitePool,
    stream_key: &str,
    delivery_state: &str,
) -> CommandResult<()> {
    finish_streaming_agent_message_inner(pool, stream_key, delivery_state, true).await
}

pub(crate) async fn finish_streaming_agent_message_deferred_mentions(
    pool: &SqlitePool,
    stream_key: &str,
    delivery_state: &str,
) -> CommandResult<()> {
    finish_streaming_agent_message_inner(pool, stream_key, delivery_state, false).await
}

async fn finish_streaming_agent_message_inner(
    pool: &SqlitePool,
    stream_key: &str,
    delivery_state: &str,
    dispatch_mentions: bool,
) -> CommandResult<()> {
    if delivery_state != "streaming" {
        if let Some((agent_id, run_id, work_item_id)) =
            load_streaming_control_context(pool, stream_key).await?
        {
            if consume_streaming_agent_control_lines(
                pool,
                agent_id,
                run_id,
                work_item_id,
                stream_key,
            )
            .await?
            {
                return Ok(());
            }
        } else {
            flush_stream_control_buffer(pool, None, stream_key).await?;
        }
    }

    if delivery_state == "complete" {
        if let Some((agent_id, run_id, Some(work_item_id))) =
            load_streaming_control_context(pool, stream_key).await?
        {
            if let Some(message_id) =
                try_complete_streaming_message_if_fresh(pool, agent_id, work_item_id, stream_key)
                    .await?
            {
                if dispatch_mentions {
                    queue_agent_message_mentions(pool, message_id).await?;
                }
                return Ok(());
            }

            if stale_output_for_work_item(pool, agent_id, work_item_id)
                .await?
                .is_some()
            {
                let row = sqlx::query(
                    "select id, body, delivery_state from messages where stream_key = $1",
                )
                .bind(stream_key)
                .fetch_optional(pool)
                .await
                .map_err(to_string)?;
                if let Some(row) = row {
                    let message_id: Uuid = row.get("id");
                    let body: String = row.get("body");
                    let current_delivery_state: String = row.get("delivery_state");
                    if current_delivery_state != "streaming" {
                        return Ok(());
                    }
                    if hold_work_item_output_if_stale(
                        pool,
                        agent_id,
                        run_id,
                        work_item_id,
                        "visible_reply",
                        &body,
                    )
                    .await?
                    {
                        delete_streaming_agent_message(pool, message_id, "freshness_hold").await?;
                        return Ok(());
                    }
                }
            }
        }
    }

    let mut transaction = pool.begin().await.map_err(to_string)?;
    let affected = sqlx::query(
        r#"
        update messages
        set delivery_state = $2
        where stream_key = $1
          and delivery_state = 'streaming'
        "#,
    )
    .bind(stream_key)
    .bind(delivery_state)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?
    .rows_affected();
    let mut completed_message_id = None;
    if affected > 0 {
        let message_id: Option<Uuid> =
            sqlx::query_scalar("select id from messages where stream_key = $1")
                .bind(stream_key)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(to_string)?;
        if let Some(message_id) = message_id {
            let message = load_message_patch_in_tx(&mut transaction, message_id).await?;
            enqueue_ui_event_in_tx(
                &mut transaction,
                &UiEvent::MessageUpsert {
                    reason: "stream_finish",
                    message: &message,
                },
            )
            .await?;
            completed_message_id = Some(message_id);
        }
    }
    transaction.commit().await.map_err(to_string)?;
    if delivery_state == "complete" && dispatch_mentions {
        if let Some(message_id) = completed_message_id {
            queue_agent_message_mentions(pool, message_id).await?;
        }
    }
    Ok(())
}

pub(crate) async fn dispatch_streaming_agent_message_mentions(
    pool: &SqlitePool,
    stream_key: &str,
) -> CommandResult<()> {
    let message_id: Option<Uuid> = sqlx::query_scalar(
        "select id from messages where stream_key = $1 and delivery_state = 'complete'",
    )
    .bind(stream_key)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;
    if let Some(message_id) = message_id {
        queue_agent_message_mentions(pool, message_id).await?;
    }
    Ok(())
}

async fn load_streaming_control_context(
    pool: &SqlitePool,
    stream_key: &str,
) -> CommandResult<Option<(Uuid, Uuid, Option<Uuid>)>> {
    let Some(run_prefix) = stream_key
        .split(':')
        .next()
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let Ok(run_id) = Uuid::parse_str(run_prefix) else {
        return Ok(None);
    };
    let Some(row) = sqlx::query("select agent_id, work_item_id from agent_runs where id = $1")
        .bind(run_id)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?
    else {
        return Ok(None);
    };
    let agent_id: Uuid = row.get("agent_id");
    let work_item_id: Option<Uuid> = row.get("work_item_id");
    Ok(Some((agent_id, run_id, work_item_id)))
}

async fn mark_work_item_silent(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
    work_item_id: Uuid,
    reason: &str,
) -> CommandResult<()> {
    let mut transaction = pool.begin().await.map_err(to_string)?;
    sqlx::query(
        r#"
        update agent_work_items
        set status = 'silent',
            completed_at = coalesce(completed_at, strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1
          and status not in ('cancelled', 'failed')
        "#,
    )
    .bind(work_item_id)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;
    enqueue_ui_work_item_changed_in_tx(&mut transaction, work_item_id, "work_item_silent").await?;
    transaction.commit().await.map_err(to_string)?;
    reconcile_work_item_change(pool, work_item_id, "work_item_silent").await?;
    record_agent_activity(
        pool,
        Some(agent_id),
        Some(run_id),
        "decision",
        "No visible reply needed",
        json!({
            "work_item_id": work_item_id,
            "reason": if reason.trim().is_empty() {
                "Agent judged the message as non-actionable."
            } else {
                reason.trim()
            }
        })
        .to_string(),
    )
    .await?;
    Ok(())
}

pub(crate) async fn mark_run_work_item_silent(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
    reason: &str,
) -> CommandResult<()> {
    let work_item_id: Option<Uuid> =
        sqlx::query_scalar("select work_item_id from agent_runs where id = $1 and agent_id = $2")
            .bind(run_id)
            .bind(agent_id)
            .fetch_optional(pool)
            .await
            .map_err(to_string)?
            .flatten();
    if let Some(work_item_id) = work_item_id {
        mark_work_item_silent(pool, agent_id, run_id, work_item_id, reason).await?;
    } else {
        record_agent_activity(
            pool,
            Some(agent_id),
            Some(run_id),
            "decision",
            "No visible reply needed",
            reason.trim(),
        )
        .await?;
    }
    Ok(())
}

pub(crate) async fn maybe_hide_silent_streaming_reply(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
    work_item_id: Option<Uuid>,
    stream_key: &str,
) -> CommandResult<bool> {
    if flush_stream_control_buffer(pool, Some((agent_id, run_id, work_item_id)), stream_key).await?
    {
        return Ok(true);
    }
    let Some(row) = sqlx::query("select id, body from messages where stream_key = $1")
        .bind(stream_key)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?
    else {
        return Ok(false);
    };
    let message_id: Uuid = row.get("id");
    let body: String = row.get("body");
    let Some(reason) = silent_reply_reason(&body) else {
        return Ok(false);
    };

    delete_streaming_agent_message(pool, message_id, "silent_reply").await?;
    if let Some(work_item_id) = work_item_id {
        mark_work_item_silent(pool, agent_id, run_id, work_item_id, &reason).await?;
    } else {
        record_agent_activity(
            pool,
            Some(agent_id),
            Some(run_id),
            "decision",
            "No visible reply needed",
            reason.trim(),
        )
        .await?;
    }
    Ok(true)
}

async fn flush_stream_control_buffer(
    pool: &SqlitePool,
    context: Option<(Uuid, Uuid, Option<Uuid>)>,
    stream_key: &str,
) -> CommandResult<bool> {
    let pending = stream_buffers()
        .lock()
        .unwrap()
        .remove(&stream_buffer_key(pool, stream_key));
    let Some(mut state) = pending else {
        return Ok(false);
    };
    let mut output = state.gate.finish(false);
    state.has_visible_text |= !output.visible.trim().is_empty();
    if !state.has_visible_text {
        output.visible.clear();
    }
    state.hide_empty_reply |= output
        .events
        .iter()
        .any(|json| control_event_hides_empty_streaming_reply(json));
    if !output.visible.is_empty() {
        // Avoid recursively completing a capped message while flushing its gate.
        Box::pin(append_visible_streaming_agent_message(
            pool,
            state.agent_id,
            state.channel_id,
            state.thread_root_id,
            stream_key,
            &output.visible,
            false,
        ))
        .await?;
    }
    if let Some((agent_id, run_id, work_item_id)) = context {
        for json in state.queued_events.into_iter().chain(output.events) {
            handle_streaming_agent_event_json(pool, agent_id, run_id, &json).await?;
            // Legacy callers supply a work item explicitly even when the run
            // has no work_item_id link. Preserve that silent-reply contract.
            if let Some(work_item_id) = work_item_id {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&json) {
                    if value["type"] == "silent" {
                        let already_silent: bool = sqlx::query_scalar("select exists(select 1 from agent_work_items where id=$1 and status='silent')")
                            .bind(work_item_id).fetch_one(pool).await.map_err(to_string)?;
                        if !already_silent {
                            mark_work_item_silent(
                                pool,
                                agent_id,
                                run_id,
                                work_item_id,
                                value["reason"].as_str().unwrap_or(""),
                            )
                            .await?;
                        }
                    }
                }
            }
        }
    }
    if context.is_some()
        && state.hide_empty_reply
        && stored_streaming_message_body_is_empty(pool, stream_key).await?
    {
        delete_streaming_agent_message_by_key(pool, stream_key, "stream_event_consumed").await?;
        return Ok(true);
    }
    Ok(false)
}

pub(crate) async fn consume_streaming_agent_control_lines(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
    work_item_id: Option<Uuid>,
    stream_key: &str,
) -> CommandResult<bool> {
    if flush_stream_control_buffer(pool, Some((agent_id, run_id, work_item_id)), stream_key).await?
    {
        return Ok(true);
    }
    if maybe_hide_silent_streaming_reply(pool, agent_id, run_id, work_item_id, stream_key).await? {
        return Ok(true);
    }

    let Some(row) = sqlx::query("select id, body from messages where stream_key = $1")
        .bind(stream_key)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?
    else {
        return Ok(false);
    };
    let message_id: Uuid = row.get("id");
    let body: String = row.get("body");
    let (visible_body, event_jsons) = split_terminal_streaming_agent_event_lines(&body);
    let body_changed = visible_body != body;
    if event_jsons.is_empty() && !body_changed {
        return Ok(false);
    }

    for json in &event_jsons {
        handle_streaming_agent_event_json(pool, agent_id, run_id, json).await?;
    }

    if visible_body.is_empty()
        && ((body_changed && event_jsons.is_empty())
            || event_jsons
                .iter()
                .any(|json| control_event_hides_empty_streaming_reply(json)))
    {
        delete_streaming_agent_message(pool, message_id, "stream_event_consumed").await?;
        return Ok(true);
    }

    let mut transaction = pool.begin().await.map_err(to_string)?;
    sqlx::query("update messages set body = $2 where id = $1")
        .bind(message_id)
        .bind(&visible_body)
        .execute(&mut *transaction)
        .await
        .map_err(to_string)?;
    let message = load_message_patch_in_tx(&mut transaction, message_id).await?;
    enqueue_ui_event_in_tx(
        &mut transaction,
        &UiEvent::MessageUpsert {
            reason: "stream_event_consumed",
            message: &message,
        },
    )
    .await?;
    transaction.commit().await.map_err(to_string)?;
    Ok(false)
}

pub(crate) async fn streaming_message_exists(
    pool: &SqlitePool,
    stream_key: &str,
) -> CommandResult<bool> {
    let exists: bool =
        sqlx::query_scalar("select exists(select 1 from messages where stream_key = $1)")
            .bind(stream_key)
            .fetch_one(pool)
            .await
            .map_err(to_string)?;
    Ok(exists)
}

#[cfg(test)]
#[path = "../tests/runtime_streaming.rs"]
mod relocated_tests;

#[cfg(test)]
mod tests {
    use super::{capped_stream_delta, STREAMING_MESSAGE_BODY_LIMIT, STREAMING_TRUNCATION_MARKER};

    #[test]
    fn caps_streaming_deltas_with_marker() {
        let remaining = STREAMING_MESSAGE_BODY_LIMIT - 4;
        let delta = "x".repeat(remaining + 16);
        let (capped, truncated) = capped_stream_delta(&delta, 4);
        assert!(truncated);
        assert!(capped.ends_with(STREAMING_TRUNCATION_MARKER));
        assert_eq!(capped.chars().count() + 4, STREAMING_MESSAGE_BODY_LIMIT);
    }
}
