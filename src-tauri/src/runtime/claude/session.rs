//! Which Claude Code session a newly spawned warm runtime continues.
//!
//! The idle reaper, failure resets and environment changes end the `claude`
//! process, but its session transcript stays on disk. A respawn resumes the
//! stored session ID so earlier turns remain in provider context. Claude Code
//! compacts a long session itself (`CLAUDE_CODE_AUTO_COMPACT_WINDOW`), so
//! Lantor never starts over because a session grew.

use std::sync::Arc;

use serde_json::Value;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use super::{steer::settle_claude_follow_ups, ClaudeSurface, WarmClaudeRuntime};
use crate::app::{to_string, CommandResult};
use crate::events::activity::record_agent_activity;
use crate::runtime::{
    lifecycle::WarmStartFailure, process::cleanup_failed_warm_start,
    streaming::delete_streaming_agent_message_by_key,
};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ClaudeSessionStart {
    Fresh,
    Resume {
        session_id: String,
        context_tokens: i64,
        last_surface: Option<ClaudeSurface>,
    },
}

impl ClaudeSessionStart {
    pub(super) fn resume_session_id(&self) -> Option<&str> {
        match self {
            Self::Resume { session_id, .. } => Some(session_id),
            Self::Fresh => None,
        }
    }

    pub(super) fn describe(&self) -> String {
        match self {
            Self::Fresh => "new session".to_owned(),
            Self::Resume {
                session_id,
                context_tokens,
                ..
            } => format!("resumed session {session_id} ({context_tokens} context tokens)"),
        }
    }
}

pub(super) async fn plan_claude_session_start(
    pool: &SqlitePool,
    agent_id: Uuid,
) -> CommandResult<ClaudeSessionStart> {
    let Some(row) = sqlx::query(
        r#"
        select provider_thread_id, context_tokens
        from runtime_sessions
        where agent_id = $1 and runtime = 'claude'
        "#,
    )
    .bind(agent_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?
    else {
        return Ok(ClaudeSessionStart::Fresh);
    };
    // Until its first provider event, a fresh process stores a `pid:` placeholder.
    let session_id = row.get::<String, _>("provider_thread_id").trim().to_owned();
    if Uuid::parse_str(&session_id).is_err() {
        return Ok(ClaudeSessionStart::Fresh);
    }
    Ok(ClaudeSessionStart::Resume {
        session_id,
        context_tokens: row.get("context_tokens"),
        last_surface: last_claude_surface(pool, agent_id).await?,
    })
}

/// The surface the agent's most recent finished run answered, so a resumed
/// session still gets a thread-boundary marker when the surface moves.
async fn last_claude_surface(
    pool: &SqlitePool,
    agent_id: Uuid,
) -> CommandResult<Option<ClaudeSurface>> {
    let row = sqlx::query(
        r#"
        select w.channel_id, w.thread_root_id
        from agent_runs r
        left join agent_work_items w on w.id = r.work_item_id
        where r.agent_id = $1 and r.stopped_at is not null
        order by r.stopped_at desc
        limit 1
        "#,
    )
    .bind(agent_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;
    Ok(row.map(|row| ClaudeSurface {
        channel_id: row.get("channel_id"),
        thread_root_id: row.get("thread_root_id"),
    }))
}

pub(super) async fn store_claude_context_tokens(
    pool: &SqlitePool,
    agent_id: Uuid,
    context_tokens: i64,
) -> CommandResult<()> {
    sqlx::query(
        "update runtime_sessions set context_tokens = $2 where agent_id = $1 and runtime = 'claude'",
    )
    .bind(agent_id)
    .bind(context_tokens.max(0))
    .execute(pool)
    .await
    .map_err(to_string)?;
    Ok(())
}

/// `--resume` of a session whose transcript is gone (Claude Code prunes old
/// transcripts) exits before any turn runs. Forget the session and requeue the
/// request so it runs again in a new session.
pub(super) async fn restart_after_missing_session(
    pool: &SqlitePool,
    agent_id: Uuid,
    runtime: &Arc<WarmClaudeRuntime>,
    value: &Value,
) -> CommandResult<()> {
    let active = {
        let mut state = runtime.state.lock().await;
        state.alive = false;
        state.session_id = None;
        state.active.take()
    };
    sqlx::query(
        r#"
        update runtime_sessions
        set provider_thread_id = '',
            context_tokens = 0,
            status = 'stopped',
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where agent_id = $1 and runtime = 'claude'
        "#,
    )
    .bind(agent_id)
    .execute(pool)
    .await
    .map_err(to_string)?;
    let detail = value
        .get("errors")
        .and_then(Value::as_array)
        .map(|errors| {
            errors
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_default();
    record_agent_activity(
        pool,
        Some(agent_id),
        active.as_ref().map(|active| active.run_id),
        "run",
        "Claude session not found; starting a new session",
        detail.clone(),
    )
    .await?;
    // The process is gone, so follow-ups it never replayed go back to the queue.
    settle_claude_follow_ups(pool, agent_id, runtime).await?;
    let Some(active) = active else {
        return Ok(());
    };
    if active.channel_id.is_some() {
        delete_streaming_agent_message_by_key(pool, &active.stream_key, "claude_session_missing")
            .await?;
    }
    cleanup_failed_warm_start(
        pool,
        "claude",
        agent_id,
        active.run_id,
        active.work_item_id,
        &detail,
        WarmStartFailure::Transient,
    )
    .await
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::runtime::process::upsert_runtime_thread_id;
    use crate::test_support::{
        drop_test_schema, insert_test_agent, insert_test_channel, test_pool,
    };

    #[tokio::test]
    async fn session_start_resumes_stored_session_at_any_size() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "claude-resume").await?;
            assert_eq!(
                plan_claude_session_start(&pool, agent_id).await?,
                ClaudeSessionStart::Fresh
            );

            upsert_runtime_thread_id(&pool, agent_id, "claude", "pid:4242", "idle").await?;
            assert_eq!(
                plan_claude_session_start(&pool, agent_id).await?,
                ClaudeSessionStart::Fresh,
                "a pid placeholder is not a resumable session"
            );

            let session_id = Uuid::new_v4().to_string();
            upsert_runtime_thread_id(&pool, agent_id, "claude", &session_id, "stopped").await?;
            store_claude_context_tokens(&pool, agent_id, 90_000).await?;
            let channel_id = insert_test_channel(&pool, "claude-resume").await?;
            let thread_root_id: Uuid = sqlx::query_scalar(
                "insert into messages (channel_id, sender_name, sender_role, body) values ($1, 'Dylan', 'owner', 'root') returning id",
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let work_item_id: Uuid = sqlx::query_scalar(
                "insert into agent_work_items (agent_id, channel_id, thread_root_id, title, status) values ($1, $2, $3, 'last', 'done') returning id",
            )
            .bind(agent_id)
            .bind(channel_id)
            .bind(thread_root_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            sqlx::query(
                "insert into agent_runs (agent_id, work_item_id, command, status, stopped_at) values ($1, $2, 'claude', 'exited', strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))",
            )
            .bind(agent_id)
            .bind(work_item_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            assert_eq!(
                plan_claude_session_start(&pool, agent_id).await?,
                ClaudeSessionStart::Resume {
                    session_id: session_id.clone(),
                    context_tokens: 90_000,
                    last_surface: Some(ClaudeSurface {
                        channel_id: Some(channel_id),
                        thread_root_id: Some(thread_root_id),
                    }),
                }
            );
            // Claude Code compacts a long session itself; Lantor never drops it.
            store_claude_context_tokens(&pool, agent_id, 900_000).await?;
            assert_eq!(
                plan_claude_session_start(&pool, agent_id)
                    .await?
                    .resume_session_id(),
                Some(session_id.as_str())
            );
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }
}
