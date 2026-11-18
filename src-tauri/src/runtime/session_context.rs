//! What a provider session already holds from Lantor.
//!
//! A warm runtime resumes one Claude session or Codex thread across turns and
//! process restarts, so a turn's prompt need not repeat what earlier turns
//! put there: the memory snapshot and the surface's earlier messages. Both
//! are recorded for the session that received them, once its turn succeeded.
//! Another session, or the same one after the provider compacted it, gets
//! everything again.

use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::app::{to_string, CommandResult};

/// How a work item's prompt may shrink for a session that holds the surface's
/// earlier messages, and what the session holds once the turn succeeds.
#[derive(Clone, Debug, Default)]
pub(crate) struct SurfaceDelivery {
    /// Highest message seq the prompt shows in its final form.
    pub(crate) delivered_seq: i64,
    pub(crate) delta: Option<SurfaceContextDelta>,
}

/// The prompt listing only the surface's messages since `thread_id` last got
/// them. Only that session may receive it.
#[derive(Clone, Debug)]
pub(crate) struct SurfaceContextDelta {
    pub(crate) thread_id: String,
    pub(crate) prompt: String,
}

impl SurfaceDelivery {
    /// The delta prompt when `thread_id` is the session it was made for.
    pub(crate) fn delta_prompt_for(&self, thread_id: Option<&str>) -> Option<&str> {
        let delta = self.delta.as_ref()?;
        (thread_id == Some(delta.thread_id.as_str())).then_some(delta.prompt.as_str())
    }
}

/// The session a surface's messages were last delivered to, and up to which
/// seq.
pub(crate) async fn load_surface_delivery(
    pool: &SqlitePool,
    agent_id: Uuid,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
) -> CommandResult<Option<(String, i64)>> {
    let row = sqlx::query(
        r#"
        select context_thread_id, context_seq
        from agent_target_watermarks
        where agent_id = $1 and channel_id = $2 and thread_root_id is $3
        "#,
    )
    .bind(agent_id)
    .bind(channel_id)
    .bind(thread_root_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;
    Ok(row.and_then(|row| {
        let thread_id: String = row.get("context_thread_id");
        let seq: i64 = row.get("context_seq");
        (!thread_id.is_empty() && seq > 0).then_some((thread_id, seq))
    }))
}

/// Records that `thread_id` now holds the surface's messages up to `seq`. The
/// surface's watermark row exists by then: a turn's start creates it.
pub(crate) async fn store_surface_delivery(
    pool: &SqlitePool,
    agent_id: Uuid,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    thread_id: &str,
    seq: i64,
) -> CommandResult<()> {
    if thread_id.is_empty() || seq <= 0 {
        return Ok(());
    }
    sqlx::query(
        r#"
        update agent_target_watermarks
        set context_seq = case
                when context_thread_id = $4 then max(context_seq, $5)
                else $5
            end,
            context_thread_id = $4
        where agent_id = $1 and channel_id = $2 and thread_root_id is $3
        "#,
    )
    .bind(agent_id)
    .bind(channel_id)
    .bind(thread_root_id)
    .bind(thread_id)
    .bind(seq)
    .execute(pool)
    .await
    .map_err(to_string)?;
    Ok(())
}

/// The memory snapshot `thread_id` last received, when it received one.
pub(crate) async fn load_injected_memory(
    pool: &SqlitePool,
    agent_id: Uuid,
    runtime: &str,
    thread_id: &str,
) -> CommandResult<Option<String>> {
    let memory: Option<Option<String>> = sqlx::query_scalar(
        r#"
        select memory_context
        from runtime_sessions
        where agent_id = $1 and runtime = $2 and memory_thread_id = $3 and $3 <> ''
        "#,
    )
    .bind(agent_id)
    .bind(runtime)
    .bind(thread_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;
    Ok(memory.flatten())
}

/// Records the memory snapshot `thread_id` now holds; `None` when its last
/// turn was told the memory is empty.
pub(crate) async fn store_injected_memory(
    pool: &SqlitePool,
    agent_id: Uuid,
    runtime: &str,
    thread_id: &str,
    memory: Option<&str>,
) -> CommandResult<()> {
    sqlx::query(
        r#"
        update runtime_sessions
        set memory_context = $4, memory_thread_id = $3
        where agent_id = $1 and runtime = $2
        "#,
    )
    .bind(agent_id)
    .bind(runtime)
    .bind(thread_id)
    .bind(memory)
    .execute(pool)
    .await
    .map_err(to_string)?;
    Ok(())
}

/// The provider compacted the agent's session: what it held in full is now a
/// summary, so its next turns get the memory snapshot and each surface's
/// messages again.
pub(crate) async fn forget_session_context(
    pool: &SqlitePool,
    agent_id: Uuid,
    runtime: &str,
) -> CommandResult<()> {
    let mut transaction = pool.begin().await.map_err(to_string)?;
    sqlx::query(
        "update runtime_sessions set memory_context = null, memory_thread_id = '' where agent_id = $1 and runtime = $2",
    )
    .bind(agent_id)
    .bind(runtime)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;
    sqlx::query(
        "update agent_target_watermarks set context_seq = 0, context_thread_id = '' where agent_id = $1",
    )
    .bind(agent_id)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;
    transaction.commit().await.map_err(to_string)?;
    Ok(())
}

#[cfg(test)]
#[path = "../tests/runtime_session_context.rs"]
mod tests;
