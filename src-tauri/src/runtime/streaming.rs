use serde_json::json;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::agent_routing::queue_agent_message_mentions;
use crate::app::{to_string, CommandResult};
use crate::events::{
    activity::record_agent_activity,
    control::{
        control_event_hides_empty_streaming_reply, handle_streaming_agent_event_json,
        silent_reply_reason, split_complete_streaming_agent_event_lines,
        split_terminal_streaming_agent_event_lines,
    },
};
use crate::message_store::load_message;
use crate::ui_notifications::{
    notify_ui_message_delete, notify_ui_message_delta, notify_ui_message_upsert, notify_ui_refresh,
    notify_ui_work_item_changed,
};

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
        sqlx::query("update messages set body = body || $2, delivery_state = $3 where id = $1")
            .bind(message_id)
            .bind(&append_delta)
            .bind(delivery_state)
            .execute(pool)
            .await
            .map_err(to_string)?;
        let _ = notify_ui_message_delta(
            pool,
            message_id,
            &append_delta,
            delivery_state,
            "stream_delta",
        )
        .await;
        if let Some((control_agent_id, run_id, _)) =
            load_streaming_control_context(pool, stream_key).await?
        {
            let _ = consume_complete_streaming_agent_control_lines(
                pool,
                control_agent_id,
                run_id,
                stream_key,
            )
            .await;
        }
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
    .fetch_one(pool)
    .await
    .map_err(to_string)?;

    if let Ok(message) = load_message(pool, message_id).await {
        let _ = notify_ui_message_upsert(pool, &message, "stream_start").await;
    } else {
        let _ = notify_ui_refresh(pool, "stream_start").await;
    }
    if let Some((control_agent_id, run_id, _)) =
        load_streaming_control_context(pool, stream_key).await?
    {
        let _ = consume_complete_streaming_agent_control_lines(
            pool,
            control_agent_id,
            run_id,
            stream_key,
        )
        .await;
    }
    if truncated && complete_on_truncation {
        queue_agent_message_mentions(pool, message_id).await?;
    }
    Ok(message_id)
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
    .fetch_one(pool)
    .await
    .map_err(to_string)?;

    if let Ok(message) = load_message(pool, message_id).await {
        let _ = notify_ui_message_upsert(pool, &message, "stream_placeholder").await;
    } else {
        let _ = notify_ui_refresh(pool, "stream_placeholder").await;
    }
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
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;

    if let Some(message_id) = message_id {
        if let Ok(message) = load_message(pool, message_id).await {
            let _ = notify_ui_message_upsert(pool, &message, "stream_key_adopted").await;
        } else {
            let _ = notify_ui_refresh(pool, "stream_key_adopted").await;
        }
    }
    Ok(message_id)
}

pub(crate) async fn streaming_message_body_is_empty(
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
    sqlx::query("delete from messages where id = $1")
        .bind(message_id)
        .execute(pool)
        .await
        .map_err(to_string)?;
    let _ = notify_ui_message_delete(pool, message_id, reason).await;
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

pub(crate) async fn delete_intermediate_run_messages(
    pool: &SqlitePool,
    agent_id: Uuid,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    run_id: Uuid,
) -> CommandResult<()> {
    let superseded_ids: Vec<Uuid> = sqlx::query_scalar(
        r#"
        select id
        from messages
        where sender_agent_id = $1
          and channel_id = $2
          and thread_root_id is not distinct from $3
          and stream_key like $4
          and body <> ''
          and delivery_state in ('streaming', 'complete')
        "#,
    )
    .bind(agent_id)
    .bind(channel_id)
    .bind(thread_root_id)
    .bind(format!("{run_id}:%"))
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    for message_id in superseded_ids {
        delete_streaming_agent_message(pool, message_id, "superseded_intermediate_reply").await?;
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
        }
    }

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
    .execute(pool)
    .await
    .map_err(to_string)?
    .rows_affected();
    if affected > 0 {
        let message_id: Option<Uuid> =
            sqlx::query_scalar("select id from messages where stream_key = $1")
                .bind(stream_key)
                .fetch_optional(pool)
                .await
                .map_err(to_string)?;
        if let Some(message_id) = message_id {
            if let Ok(message) = load_message(pool, message_id).await {
                let _ = notify_ui_message_upsert(pool, &message, "stream_finish").await;
            } else {
                let _ = notify_ui_refresh(pool, "stream_finish").await;
            }
            if delivery_state == "complete" && dispatch_mentions {
                queue_agent_message_mentions(pool, message_id).await?;
            }
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
    .execute(pool)
    .await
    .map_err(to_string)?;
    notify_ui_work_item_changed(pool, work_item_id, "work_item_silent").await;
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

async fn consume_complete_streaming_agent_control_lines(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
    stream_key: &str,
) -> CommandResult<bool> {
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
    let (visible_body, event_jsons) = split_complete_streaming_agent_event_lines(&body);
    if event_jsons.is_empty() {
        return Ok(false);
    }

    for json in &event_jsons {
        handle_streaming_agent_event_json(pool, agent_id, run_id, json).await?;
    }

    if visible_body.is_empty()
        && event_jsons
            .iter()
            .any(|json| control_event_hides_empty_streaming_reply(json))
    {
        delete_streaming_agent_message(pool, message_id, "stream_event_consumed").await?;
        return Ok(true);
    }

    sqlx::query("update messages set body = $2, updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') where id = $1")
        .bind(message_id)
        .bind(&visible_body)
        .execute(pool)
        .await
        .map_err(to_string)?;
    if let Ok(message) = load_message(pool, message_id).await {
        let _ = notify_ui_message_upsert(pool, &message, "stream_event_consumed").await;
    } else {
        let _ = notify_ui_refresh(pool, "stream_event_consumed").await;
    }
    Ok(true)
}

pub(crate) async fn consume_streaming_agent_control_lines(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
    work_item_id: Option<Uuid>,
    stream_key: &str,
) -> CommandResult<bool> {
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

    sqlx::query("update messages set body = $2 where id = $1")
        .bind(message_id)
        .bind(&visible_body)
        .execute(pool)
        .await
        .map_err(to_string)?;
    if let Ok(message) = load_message(pool, message_id).await {
        let _ = notify_ui_message_upsert(pool, &message, "stream_event_consumed").await;
    } else {
        let _ = notify_ui_refresh(pool, "stream_event_consumed").await;
    }
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
