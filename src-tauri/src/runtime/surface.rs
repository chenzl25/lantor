use std::time::Duration;

use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::app::{to_string, CommandResult};
use crate::attachments::attachment_summary_sql;
use crate::context_tool::{format_context_message_row, short_id};

const SURFACE_CONTEXT_MESSAGE_LIMIT: i64 = 16;
const SURFACE_CONTEXT_BODY_LIMIT: usize = 1_200;

pub(crate) struct RenderedThreadContext {
    pub(crate) context: String,
    pub(crate) max_seq: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CodexActiveTurnScheduleState {
    ReadyForSteer,
    WaitingForTurnId,
    StuckBeforeTurnId,
}

pub(crate) fn codex_active_turn_schedule_state(
    turn_id: Option<&str>,
    steer_disabled: bool,
    elapsed: Duration,
    turn_start_timeout: Duration,
) -> CodexActiveTurnScheduleState {
    if turn_id.is_some() && !steer_disabled {
        return CodexActiveTurnScheduleState::ReadyForSteer;
    }
    if turn_id.is_none() && elapsed >= turn_start_timeout {
        return CodexActiveTurnScheduleState::StuckBeforeTurnId;
    }
    CodexActiveTurnScheduleState::WaitingForTurnId
}

pub(crate) async fn same_codex_surface(
    _pool: &SqlitePool,
    channel_id: Option<Uuid>,
    thread_root_id: Option<Uuid>,
    active_channel_id: Option<Uuid>,
    active_thread_root_id: Option<Uuid>,
) -> CommandResult<bool> {
    if channel_id.is_none() || channel_id != active_channel_id {
        return Ok(false);
    }
    Ok(thread_root_id == active_thread_root_id)
}

/// Appends the surface's recent messages, read when the turn starts, to a
/// work item's stored context. Every runtime gets this block, so queued work
/// stores only what woke the agent, never a copy of the thread.
pub(crate) async fn append_surface_context(
    pool: &SqlitePool,
    context: &str,
    channel_id: Option<Uuid>,
    channel_name: Option<&str>,
    thread_root_id: Option<Uuid>,
) -> CommandResult<RenderedThreadContext> {
    let Some(channel_id) = channel_id else {
        return Ok(RenderedThreadContext {
            context: context.to_owned(),
            max_seq: 0,
        });
    };
    // Skip empty messages (streaming placeholders) but keep attachment-only
    // ones, such as a screenshot posted without text.
    let rows = if let Some(thread_root_id) = thread_root_id {
        sqlx::query(&format!(
            r#"
            select
                m.id, m.sender_name, m.sender_role, m.body, m.delivery_state, m.seq, m.created_at,
                t.number as task_number, t.status as task_status,
                {}
            from messages m
            left join tasks t on t.message_id = m.id
            where m.channel_id = $1
              and (m.id = $2 or m.thread_root_id = $2)
              and (length(trim(m.body)) > 0
                   or exists (select 1 from message_attachments ma where ma.message_id = m.id))
            order by m.seq desc
            limit $3
            "#,
            attachment_summary_sql()
        ))
        .bind(channel_id)
        .bind(thread_root_id)
        .bind(SURFACE_CONTEXT_MESSAGE_LIMIT)
        .fetch_all(pool)
        .await
        .map_err(to_string)?
    } else {
        sqlx::query(&format!(
            r#"
            select
                m.id, m.sender_name, m.sender_role, m.body, m.delivery_state, m.seq, m.created_at,
                t.number as task_number, t.status as task_status,
                {}
            from messages m
            left join tasks t on t.message_id = m.id
            where m.channel_id = $1
              and m.thread_root_id is null
              and (length(trim(m.body)) > 0
                   or exists (select 1 from message_attachments ma where ma.message_id = m.id))
            order by m.seq desc
            limit $2
            "#,
            attachment_summary_sql()
        ))
        .bind(channel_id)
        .bind(SURFACE_CONTEXT_MESSAGE_LIMIT)
        .fetch_all(pool)
        .await
        .map_err(to_string)?
    };
    if rows.is_empty() {
        return Ok(RenderedThreadContext {
            context: context.to_owned(),
            max_seq: 0,
        });
    }
    let max_seq = rows
        .iter()
        .map(|row| row.get::<i64, _>("seq"))
        .max()
        .unwrap_or(0);

    let target = surface_target_label(channel_name, channel_id, thread_root_id);
    let mut lines = vec![
        String::new(),
        "Same-thread recent context (auto-injected by Lantor):".to_owned(),
        "Use these messages as current-surface evidence. Resolve contextual follow-ups from this block when possible; if it is insufficient, use the Lantor history/search context tools before answering. Do not mix in older warm-runtime turns from other channels or threads unless they are explicitly quoted here.".to_owned(),
    ];
    for row in rows.iter().rev() {
        lines.push(format_context_message_row(
            row,
            Some(&target),
            SURFACE_CONTEXT_BODY_LIMIT,
        ));
    }

    let mut enriched = context.trim().to_owned();
    if !enriched.is_empty() {
        enriched.push('\n');
    }
    enriched.push_str(&lines.join("\n"));
    Ok(RenderedThreadContext {
        context: enriched,
        max_seq,
    })
}

fn surface_target_label(
    channel_name: Option<&str>,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
) -> String {
    let channel_label = channel_name
        .filter(|name| !name.trim().is_empty())
        .map(|name| format!("#{name}"))
        .unwrap_or_else(|| channel_id.to_string());
    thread_root_id
        .map(|thread_root_id| format!("{channel_label}:{}", short_id(thread_root_id)))
        .unwrap_or(channel_label)
}

#[cfg(test)]
#[path = "../tests/runtime_surface.rs"]
mod relocated_tests;
