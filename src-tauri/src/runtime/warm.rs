//! The process half of a warm runtime, shared by Claude and Codex.
//!
//! A warm runtime is one long-lived provider process per agent that answers
//! turn after turn. This module owns what does not depend on the provider's
//! protocol: the registry of live processes, reusing or retiring a process
//! before a start, the stderr log, what happens when the process exits, and
//! the idle stop. Each provider implements [`WarmProvider`] and keeps the
//! rest: spawning and the handshake, the stdout protocol, turns, follow-ups
//! and interrupts.

use std::{collections::HashMap, future::Future, sync::Arc};

use sqlx::{Sqlite, SqlitePool, Transaction};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    process::{Child, ChildStdin},
    sync::Mutex as AsyncMutex,
};
use uuid::Uuid;

use super::{
    process::{classify_agent_output_activity, terminate_process_group},
    runtime_environment_changed,
};
use crate::agent_memory::append_run_log;
use crate::app::{to_string, CommandResult};
use crate::events::activity::{record_agent_activity, record_agent_activity_throttled};
use crate::ui_notifications::notify_supervisor_wake;

/// What the shared code reads and writes in a provider's process state.
pub(crate) trait WarmState: Send + 'static {
    fn alive(&self) -> bool;
    fn set_alive(&mut self, alive: bool);
    /// Records activity now, which postpones the idle stop.
    fn touch(&mut self);
    /// The run of the turn in progress, if any.
    fn active_run_id(&self) -> Option<Uuid>;
    /// A turn is in progress or owed, so the process must not be replaced.
    fn is_busy(&self) -> bool;
}

/// A provider of warm runtimes. The implementing type is also the data a
/// process keeps for its whole life, such as the Codex thread id.
pub(crate) trait WarmProvider: Send + Sync + Sized + 'static {
    type State: WarmState;
    /// The provider's `runtime_sessions.runtime`.
    const RUNTIME: &'static str;
    /// The process as activity titles name it.
    const PROCESS_TITLE: &'static str;
    /// The process as exit details name it.
    const PROCESS_NAME: &'static str;

    /// The provider thread a session row must match to be this process's,
    /// or `None` when the agent has only one session row for the provider.
    fn session_thread_id(&self) -> Option<&str>;

    /// Ends the turn in progress, if any, once the process has exited.
    fn finish_turn_after_exit(
        pool: &SqlitePool,
        agent_id: Uuid,
        runtime: &Arc<WarmRuntime<Self>>,
        detail: String,
    ) -> impl Future<Output = ()> + Send;
}

/// A live provider process.
pub(crate) struct WarmRuntime<P: WarmProvider> {
    pub(super) stdin: AsyncMutex<ChildStdin>,
    pub(super) state: AsyncMutex<P::State>,
    pub(super) pid: Option<i32>,
    pub(super) environment_variables: String,
    pub(super) provider: P,
}

/// The warm processes of one provider, one per agent at most.
pub(crate) struct WarmRegistry<P: WarmProvider> {
    runtimes: Arc<AsyncMutex<HashMap<Uuid, Arc<WarmRuntime<P>>>>>,
}

impl<P: WarmProvider> Clone for WarmRegistry<P> {
    fn clone(&self) -> Self {
        Self {
            runtimes: self.runtimes.clone(),
        }
    }
}

impl<P: WarmProvider> Default for WarmRegistry<P> {
    fn default() -> Self {
        Self {
            runtimes: Arc::default(),
        }
    }
}

impl<P: WarmProvider> WarmRegistry<P> {
    pub(crate) async fn get(&self, agent_id: Uuid) -> Option<Arc<WarmRuntime<P>>> {
        self.runtimes.lock().await.get(&agent_id).cloned()
    }

    pub(crate) async fn insert(&self, agent_id: Uuid, runtime: Arc<WarmRuntime<P>>) {
        self.runtimes.lock().await.insert(agent_id, runtime);
    }

    pub(crate) async fn remove(&self, agent_id: Uuid) {
        self.runtimes.lock().await.remove(&agent_id);
    }

    /// Removes the agent's entry only while it is still `runtime`, so a
    /// process that already replaced it stays registered.
    pub(crate) async fn remove_if_same(&self, agent_id: Uuid, runtime: &Arc<WarmRuntime<P>>) {
        let mut runtimes = self.runtimes.lock().await;
        if runtimes
            .get(&agent_id)
            .map(|current| Arc::ptr_eq(current, runtime))
            .unwrap_or(false)
        {
            runtimes.remove(&agent_id);
        }
    }

    /// Whether the agent's process still owns a turn. Runs of an agent
    /// without one are orphans.
    pub(crate) async fn owns_turn(&self, agent_id: Uuid) -> bool {
        match self.get(agent_id).await {
            Some(runtime) => runtime.state.lock().await.is_busy(),
            None => false,
        }
    }

    fn snapshot(runtimes: &HashMap<Uuid, Arc<WarmRuntime<P>>>) -> Vec<(Uuid, Arc<WarmRuntime<P>>)> {
        runtimes
            .iter()
            .map(|(agent_id, runtime)| (*agent_id, runtime.clone()))
            .collect()
    }

    /// Whether any process is in a turn.
    pub(crate) async fn any_busy(&self) -> bool {
        let runtimes = Self::snapshot(&*self.runtimes.lock().await);
        for (_, runtime) in runtimes {
            if runtime.state.lock().await.is_busy() {
                return true;
            }
        }
        false
    }

    pub(crate) async fn is_empty(&self) -> bool {
        self.runtimes.lock().await.is_empty()
    }

    /// Stops every process that is not in a turn, as the idle reaper does. A
    /// stopped process leaves the registry once it has exited.
    pub(crate) async fn stop_idle_processes(&self, pool: &SqlitePool, reason: &str) {
        let runtimes = Self::snapshot(&*self.runtimes.lock().await);
        for (agent_id, runtime) in runtimes {
            let stop = {
                let mut state = runtime.state.lock().await;
                let stop = state.alive() && !state.is_busy();
                if stop {
                    state.set_alive(false);
                }
                stop
            };
            if stop {
                stop_idle_process(pool, agent_id, &runtime, reason).await;
            }
        }
    }
}

/// Asks for a fresh process at the agent's next start. Restart in the agent
/// panel uses it, so that the provider re-reads what it loads only when it
/// starts, such as MCP logins and skills. A process in a turn is replaced at
/// the first start after the turn.
pub(crate) async fn request_restart_in_tx(
    transaction: &mut Transaction<'_, Sqlite>,
    agent_id: Uuid,
) -> CommandResult<()> {
    sqlx::query(
        "update agents set runtime_restart_requested_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') where id = $1",
    )
    .bind(agent_id)
    .execute(&mut **transaction)
    .await
    .map_err(to_string)?;
    Ok(())
}

/// The agent's live process when it can take the next start. A process
/// whose environment changed, that the provider wants replaced, or whose
/// restart was requested is retired first unless a turn still needs it; a
/// dead one is dropped. `None` means the caller spawns a new process, which
/// also serves a pending restart request.
pub(crate) async fn reusable_runtime<P: WarmProvider>(
    pool: &SqlitePool,
    registry: &WarmRegistry<P>,
    agent_id: Uuid,
    environment_variables: &str,
    replace: bool,
) -> CommandResult<Option<Arc<WarmRuntime<P>>>> {
    let restart_requested_at: Option<String> =
        sqlx::query_scalar("select runtime_restart_requested_at from agents where id = $1")
            .bind(agent_id)
            .fetch_optional(pool)
            .await
            .map_err(to_string)?
            .flatten();
    let runtime = live_runtime(
        registry,
        agent_id,
        environment_variables,
        replace || restart_requested_at.is_some(),
    )
    .await;
    if let (None, Some(requested_at)) = (&runtime, restart_requested_at) {
        // Only the request this start read: a newer one waits for the next.
        sqlx::query(
            "update agents set runtime_restart_requested_at = null where id = $1 and runtime_restart_requested_at = $2",
        )
        .bind(agent_id)
        .bind(requested_at)
        .execute(pool)
        .await
        .map_err(to_string)?;
    }
    Ok(runtime)
}

async fn live_runtime<P: WarmProvider>(
    registry: &WarmRegistry<P>,
    agent_id: Uuid,
    environment_variables: &str,
    replace: bool,
) -> Option<Arc<WarmRuntime<P>>> {
    let runtime = registry.get(agent_id).await?;
    let mut state = runtime.state.lock().await;
    let environment_changed =
        runtime_environment_changed(&runtime.environment_variables, environment_variables);
    if state.alive() && (replace || environment_changed) && !state.is_busy() {
        state.set_alive(false);
        drop(state);
        if let Some(pid) = runtime.pid {
            let _ = terminate_process_group(pid).await;
        }
        registry.remove_if_same(agent_id, &runtime).await;
        None
    } else if state.alive() {
        drop(state);
        Some(runtime)
    } else {
        drop(state);
        registry.remove(agent_id).await;
        None
    }
}

/// Copies the process's stderr into the log of the turn in progress.
pub(crate) async fn warm_stderr_reader<P, R>(
    pool: SqlitePool,
    agent_id: Uuid,
    runtime: Arc<WarmRuntime<P>>,
    stream: R,
) where
    P: WarmProvider,
    R: AsyncRead + Unpin,
{
    let mut lines = BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let active_run_id = { runtime.state.lock().await.active_run_id() };
        if let Some(run_id) = active_run_id {
            let _ = append_run_log(&pool, run_id, format!("[stderr] {line}\n")).await;
            if let Some((kind, title, detail)) = classify_agent_output_activity("stderr", &line) {
                let _ = record_agent_activity_throttled(
                    &pool,
                    Some(agent_id),
                    Some(run_id),
                    kind,
                    title,
                    detail,
                )
                .await;
            }
        }
    }
}

/// Waits for the process to exit, then ends its turn, stops its session and
/// unregisters it so the next start spawns a new one.
pub(crate) async fn wait_for_warm_process<P: WarmProvider>(
    pool: SqlitePool,
    registry: WarmRegistry<P>,
    agent_id: Uuid,
    runtime: Arc<WarmRuntime<P>>,
    mut child: Child,
) {
    let wait_result = child.wait().await;
    runtime.state.lock().await.set_alive(false);
    let detail = match wait_result {
        Ok(status) => format!("{} exited: {status}", P::PROCESS_NAME),
        Err(err) => format!("{} wait failed: {err}", P::PROCESS_NAME),
    };
    P::finish_turn_after_exit(&pool, agent_id, &runtime, detail.clone()).await;
    let _ = set_session_status(&pool, agent_id, &runtime, "stopped").await;
    let _ = record_agent_activity(
        &pool,
        Some(agent_id),
        None,
        "run",
        format!("{} exited", P::PROCESS_TITLE),
        detail,
    )
    .await;
    registry.remove_if_same(agent_id, &runtime).await;
    let _ = notify_supervisor_wake(&pool).await;
}

/// Stops a process found idle and already marked not alive; `reason` ends
/// the activity title. If the signal fails, the process is alive again and
/// its idle time restarts.
pub(crate) async fn stop_idle_process<P: WarmProvider>(
    pool: &SqlitePool,
    agent_id: Uuid,
    runtime: &WarmRuntime<P>,
    reason: &str,
) {
    let Some(pid) = runtime.pid else {
        return;
    };
    match terminate_process_group(pid).await {
        Ok(()) => {
            let _ = sqlx::query(
                "update runtime_sessions set status = 'stopping', updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') where agent_id = $1 and runtime = $2",
            )
            .bind(agent_id)
            .bind(P::RUNTIME)
            .execute(pool)
            .await;
            let _ = record_agent_activity(
                pool,
                Some(agent_id),
                None,
                "run",
                format!("{} {reason}", P::PROCESS_TITLE),
                format!("sent SIGTERM to process_group={pid}"),
            )
            .await;
        }
        Err(err) => {
            {
                let mut state = runtime.state.lock().await;
                state.set_alive(true);
                state.touch();
            }
            let _ = record_agent_activity(
                pool,
                Some(agent_id),
                None,
                "run_error",
                format!("{} idle stop failed", P::PROCESS_TITLE),
                err,
            )
            .await;
        }
    }
}

/// Sets the status of the process's session row.
async fn set_session_status<P: WarmProvider>(
    pool: &SqlitePool,
    agent_id: Uuid,
    runtime: &WarmRuntime<P>,
    status: &str,
) -> CommandResult<()> {
    sqlx::query(
        r#"
        update runtime_sessions
        set status = $3, updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where agent_id = $1
          and runtime = $2
          and ($4 is null or provider_thread_id = $4)
        "#,
    )
    .bind(agent_id)
    .bind(P::RUNTIME)
    .bind(status)
    .bind(runtime.provider.session_thread_id())
    .execute(pool)
    .await
    .map_err(to_string)?;
    Ok(())
}

/// The status of a work item, or `None` when it no longer exists.
pub(crate) async fn load_work_item_status(
    pool: &SqlitePool,
    work_item_id: Uuid,
) -> CommandResult<Option<String>> {
    sqlx::query_scalar("select status from agent_work_items where id = $1")
        .bind(work_item_id)
        .fetch_optional(pool)
        .await
        .map_err(to_string)
}

#[cfg(test)]
#[path = "../tests/runtime_warm.rs"]
mod tests;
