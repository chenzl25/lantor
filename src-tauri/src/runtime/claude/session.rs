//! Which Claude Code session a newly spawned warm runtime continues.
//!
//! The idle reaper, failure resets and environment changes end the `claude`
//! process, but its session transcript stays on disk. A respawn resumes the
//! stored session ID so earlier turns remain in provider context. A session
//! that has grown past the rotation threshold starts over instead, like Codex
//! context rotation, and its first turn says where the previous run can be read.

use std::{env, sync::Arc};

use serde_json::Value;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use super::{ClaudeSurface, WarmClaudeRuntime};
use crate::app::{to_string, CommandResult};
use crate::events::activity::record_agent_activity;
use crate::runtime::{
    process::{cleanup_failed_warm_start, WarmStartFailure},
    streaming::delete_streaming_agent_message_by_key,
};

const CLAUDE_CONTEXT_ROTATE_DEFAULT_TOKENS: i64 = 200_000;
const CLAUDE_CONTEXT_ROTATE_MIN_TOKENS: i64 = 50_000;
const CLAUDE_CONTEXT_ROTATE_ENV: &str = "LANTOR_CLAUDE_CONTEXT_ROTATE_TOKENS";

fn claude_context_rotate_tokens_from_env(value: Option<&str>) -> i64 {
    value
        .and_then(|value| value.trim().parse::<i64>().ok())
        .filter(|tokens| *tokens >= CLAUDE_CONTEXT_ROTATE_MIN_TOKENS)
        .unwrap_or(CLAUDE_CONTEXT_ROTATE_DEFAULT_TOKENS)
}

pub(super) fn claude_context_rotate_tokens() -> i64 {
    claude_context_rotate_tokens_from_env(env::var(CLAUDE_CONTEXT_ROTATE_ENV).ok().as_deref())
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ClaudeSessionStart {
    Fresh,
    Resume {
        session_id: String,
        context_tokens: i64,
        last_surface: Option<ClaudeSurface>,
    },
    Rotate {
        previous_run_id: Option<Uuid>,
        context_tokens: i64,
        threshold: i64,
    },
}

impl ClaudeSessionStart {
    pub(super) fn resume_session_id(&self) -> Option<&str> {
        match self {
            Self::Resume { session_id, .. } => Some(session_id),
            _ => None,
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
            Self::Rotate {
                context_tokens,
                threshold,
                ..
            } => format!(
                "new session; rotated after {context_tokens} context tokens (threshold {threshold}, env {CLAUDE_CONTEXT_ROTATE_ENV})"
            ),
        }
    }

    pub(super) fn rotation_marker(&self) -> Option<String> {
        let Self::Rotate {
            previous_run_id,
            context_tokens,
            threshold,
        } = self
        else {
            return None;
        };
        let mut marker = format!(
            "Lantor started a new Claude session because the previous one reached {context_tokens} context tokens (threshold {threshold}). Earlier turns are not in this conversation."
        );
        if let Some(run_id) = previous_run_id {
            marker.push_str(&format!(
                " If continuity matters for this request, inspect the previous run on demand with:\n\"$LANTOR_CONTEXT_TOOL\" --agent-context-tool run-read --run-id {run_id}"
            ));
        }
        Some(marker)
    }
}

pub(super) fn prepend_claude_rotation_marker(prompt: &str, marker: &str) -> String {
    if prompt.trim().is_empty() {
        return marker.to_owned();
    }
    format!("{marker}\n\nCurrent Lantor request after context rotation:\n{prompt}")
}

pub(super) async fn plan_claude_session_start(
    pool: &SqlitePool,
    agent_id: Uuid,
    threshold: i64,
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
    let context_tokens: i64 = row.get("context_tokens");
    let last_turn = last_claude_turn(pool, agent_id).await?;
    if context_tokens >= threshold {
        return Ok(ClaudeSessionStart::Rotate {
            previous_run_id: last_turn.map(|(run_id, _)| run_id),
            context_tokens,
            threshold,
        });
    }
    Ok(ClaudeSessionStart::Resume {
        session_id,
        context_tokens,
        last_surface: last_turn.map(|(_, surface)| surface),
    })
}

/// The agent's most recent finished run and the surface it answered, so a
/// resumed session still gets a thread-boundary marker when the surface moves.
async fn last_claude_turn(
    pool: &SqlitePool,
    agent_id: Uuid,
) -> CommandResult<Option<(Uuid, ClaudeSurface)>> {
    let row = sqlx::query(
        r#"
        select r.id, w.channel_id, w.thread_root_id
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
    Ok(row.map(|row| {
        (
            row.get("id"),
            ClaudeSurface {
                channel_id: row.get("channel_id"),
                thread_root_id: row.get("thread_root_id"),
            },
        )
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

    #[test]
    fn rotate_threshold_reads_env_with_floor() {
        assert_eq!(
            claude_context_rotate_tokens_from_env(None),
            CLAUDE_CONTEXT_ROTATE_DEFAULT_TOKENS
        );
        assert_eq!(
            claude_context_rotate_tokens_from_env(Some(" 300000 ")),
            300_000
        );
        assert_eq!(
            claude_context_rotate_tokens_from_env(Some("1000")),
            CLAUDE_CONTEXT_ROTATE_DEFAULT_TOKENS
        );
        assert_eq!(
            claude_context_rotate_tokens_from_env(Some("lots")),
            CLAUDE_CONTEXT_ROTATE_DEFAULT_TOKENS
        );
    }

    #[test]
    fn rotation_marker_points_to_previous_run() {
        let run_id = Uuid::new_v4();
        let marker = ClaudeSessionStart::Rotate {
            previous_run_id: Some(run_id),
            context_tokens: 250_000,
            threshold: 200_000,
        }
        .rotation_marker()
        .unwrap();
        assert!(marker.contains("250000 context tokens (threshold 200000)"));
        assert!(marker.contains(&format!("run-read --run-id {run_id}")));
        let prompt = prepend_claude_rotation_marker("hello", &marker);
        assert!(prompt.starts_with(&marker));
        assert!(prompt.ends_with("Current Lantor request after context rotation:\nhello"));
        assert!(ClaudeSessionStart::Fresh.rotation_marker().is_none());
    }

    #[tokio::test]
    async fn session_start_resumes_stored_session_below_threshold() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "claude-resume").await?;
            assert_eq!(
                plan_claude_session_start(&pool, agent_id, 200_000).await?,
                ClaudeSessionStart::Fresh
            );

            upsert_runtime_thread_id(&pool, agent_id, "claude", "pid:4242", "idle").await?;
            assert_eq!(
                plan_claude_session_start(&pool, agent_id, 200_000).await?,
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
            let run_id: Uuid = sqlx::query_scalar(
                "insert into agent_runs (agent_id, work_item_id, command, status, stopped_at) values ($1, $2, 'claude', 'exited', strftime('%Y-%m-%dT%H:%M:%f+00:00','now')) returning id",
            )
            .bind(agent_id)
            .bind(work_item_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            assert_eq!(
                plan_claude_session_start(&pool, agent_id, 200_000).await?,
                ClaudeSessionStart::Resume {
                    session_id: session_id.clone(),
                    context_tokens: 90_000,
                    last_surface: Some(ClaudeSurface {
                        channel_id: Some(channel_id),
                        thread_root_id: Some(thread_root_id),
                    }),
                }
            );
            assert_eq!(
                plan_claude_session_start(&pool, agent_id, 80_000).await?,
                ClaudeSessionStart::Rotate {
                    previous_run_id: Some(run_id),
                    context_tokens: 90_000,
                    threshold: 80_000,
                }
            );
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }
}
