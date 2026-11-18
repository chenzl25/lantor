use std::time::Duration;

use sqlx::{sqlite::SqliteRow, Row, SqlitePool};
use uuid::Uuid;

use super::session_context::load_surface_delivery;
use crate::app::{to_string, CommandResult};
use crate::attachments::attachment_summary_sql;
use crate::context_tool::{format_context_message_row, short_id};

const SURFACE_CONTEXT_MESSAGE_LIMIT: i64 = 16;
const SURFACE_CONTEXT_BODY_LIMIT: usize = 1_200;
const SURFACE_CONTEXT_HEADING: &str = "Same-thread recent context (auto-injected by Lantor):";

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

/// A work item's context with the surface's recent messages appended, plus
/// the shorter form for a session that already holds the earlier ones.
pub(crate) struct RenderedSurfaceContext {
    pub(crate) context: String,
    pub(crate) max_seq: i64,
    /// Highest seq the block shows in its final form: a message still being
    /// streamed, and everything after it, is shown again once it is complete.
    pub(crate) delivered_seq: i64,
    /// The session that holds the earlier messages, and the context listing
    /// only what was posted since.
    pub(crate) delta: Option<(String, String)>,
}

/// Appends the surface's recent messages, read when the turn starts, to a
/// work item's stored context. Every runtime gets this block, so queued work
/// stores only what woke the agent, never a copy of the thread.
///
/// When a provider session of `agent_id` already got this surface's messages,
/// the result also carries the context for that session: only what others
/// posted since. The agent's own streamed replies are left out of it, because
/// the same session wrote them.
pub(crate) async fn render_surface_context(
    pool: &SqlitePool,
    context: &str,
    agent_id: Option<Uuid>,
    channel_id: Option<Uuid>,
    channel_name: Option<&str>,
    thread_root_id: Option<Uuid>,
) -> CommandResult<RenderedSurfaceContext> {
    let unchanged = || RenderedSurfaceContext {
        context: context.to_owned(),
        max_seq: 0,
        delivered_seq: 0,
        delta: None,
    };
    let Some(channel_id) = channel_id else {
        return Ok(unchanged());
    };
    // Skip empty messages (streaming placeholders) but keep attachment-only
    // ones, such as a screenshot posted without text.
    let rows = if let Some(thread_root_id) = thread_root_id {
        sqlx::query(&format!(
            r#"
            select
                m.id, m.sender_name, m.sender_role, m.body, m.delivery_state, m.seq, m.created_at,
                m.sender_agent_id, m.stream_key,
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
                m.sender_agent_id, m.stream_key,
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
        return Ok(unchanged());
    }
    let seq = |row: &SqliteRow| row.get::<i64, _>("seq");
    let streaming = |row: &SqliteRow| row.get::<String, _>("delivery_state") == "streaming";
    let max_seq = rows.iter().map(seq).max().unwrap_or(0);
    let delivered_seq = rows
        .iter()
        .filter(|row| streaming(row))
        .map(seq)
        .min()
        .map_or(max_seq, |first_streaming| first_streaming - 1);

    let target = surface_target_label(channel_name, channel_id, thread_root_id);
    let with_block = |lines: Vec<String>| {
        let mut enriched = context.trim().to_owned();
        if !enriched.is_empty() {
            enriched.push('\n');
        }
        enriched.push_str(&lines.join("\n"));
        enriched
    };
    let row_line = |row: &SqliteRow| {
        format_context_message_row(row, Some(&target), SURFACE_CONTEXT_BODY_LIMIT)
    };

    let mut lines = vec![
        String::new(),
        SURFACE_CONTEXT_HEADING.to_owned(),
        "Use these messages as current-surface evidence. Resolve contextual follow-ups from this block when possible; if it is insufficient, use the Lantor history/search context tools before answering. Do not mix in older warm-runtime turns from other channels or threads unless they are explicitly quoted here.".to_owned(),
    ];
    lines.extend(rows.iter().rev().map(row_line));

    let delivery = match agent_id {
        Some(agent_id) => load_surface_delivery(pool, agent_id, channel_id, thread_root_id)
            .await?
            .map(|delivery| (agent_id, delivery)),
        None => None,
    };
    let delta = delivery.map(|(agent_id, (thread_id, held_seq))| {
        let own_reply = |row: &SqliteRow| {
            row.get::<Option<Uuid>, _>("sender_agent_id") == Some(agent_id)
                && row
                    .get::<Option<String>, _>("stream_key")
                    .is_some_and(|key| !key.is_empty())
                && !streaming(row)
        };
        let since: Vec<String> = rows
            .iter()
            .rev()
            .filter(|row| seq(row) > held_seq && !own_reply(row))
            .map(row_line)
            .collect();
        let mut lines = vec![String::new(), SURFACE_CONTEXT_HEADING.to_owned()];
        if since.is_empty() {
            lines.push("No one else has posted on this surface since your last turn here. Its earlier messages and your own replies are already in this session; use the Lantor history/search context tools to read them again.".to_owned());
        } else {
            lines.push("Only what others posted on this surface since your last turn here is listed. Its earlier messages and your own replies are already in this session; use them together as current-surface evidence, and the Lantor history/search context tools when that is insufficient. Do not mix in older warm-runtime turns from other channels or threads unless they are explicitly quoted here.".to_owned());
            if rows.len() as i64 == SURFACE_CONTEXT_MESSAGE_LIMIT
                && rows.iter().all(|row| seq(row) > held_seq)
            {
                lines.push(format!("More was posted since then than fits here: these are the latest {SURFACE_CONTEXT_MESSAGE_LIMIT} messages, and the history tools show the ones before them."));
            }
            lines.extend(since);
        }
        (thread_id, with_block(lines))
    });

    Ok(RenderedSurfaceContext {
        context: with_block(lines),
        max_seq,
        delivered_seq,
        delta,
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
