use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use sqlx::SqlitePool;
use uuid::Uuid;

use super::{
    protocol::{claude_interrupt_request, claude_write_input},
    WarmClaudeRegistry, WarmClaudeRuntime,
};
use crate::agent_memory::append_run_log;
use crate::app::CommandResult;
use crate::events::activity::record_agent_activity;
use crate::runtime::process::terminate_process_group;

/// How long a stop waits for the CLI to end an interrupted turn before it
/// terminates the process group instead.
const CLAUDE_INTERRUPT_GRACE: Duration = Duration::from_secs(10);

/// Ask the warm Claude process to abort `run_id`'s turn without killing the
/// process. Returns false when the run is not the active turn of a live warm
/// runtime, so the caller falls back to signaling the process group.
pub(crate) async fn interrupt_warm_claude_run(
    pool: &SqlitePool,
    registry: &WarmClaudeRegistry,
    agent_id: Uuid,
    run_id: Uuid,
) -> CommandResult<bool> {
    let runtime = registry.get(agent_id).await;
    let Some(runtime) = runtime else {
        return Ok(false);
    };
    if !interrupt_warm_claude_turn(pool, &runtime, run_id).await? {
        return Ok(false);
    }
    tokio::spawn(terminate_if_interrupt_ignored(
        pool.clone(),
        agent_id,
        runtime,
        run_id,
        CLAUDE_INTERRUPT_GRACE,
    ));
    Ok(true)
}

pub(super) async fn interrupt_warm_claude_turn(
    pool: &SqlitePool,
    runtime: &Arc<WarmClaudeRuntime>,
    run_id: Uuid,
) -> CommandResult<bool> {
    let request_id = format!("lantor-interrupt-{run_id}");
    let write_error = {
        let mut state = runtime.state.lock().await;
        if !state.alive {
            return Ok(false);
        }
        state.last_activity = Instant::now();
        let Some(active) = state.active.as_mut() else {
            return Ok(false);
        };
        if active.run_id != run_id {
            return Ok(false);
        }
        if active.interrupt_requested {
            return Ok(true);
        }
        active.interrupt_requested = true;
        // Hold the state lock while writing: a later turn can only start after
        // this one is released, so the interrupt cannot reach the wrong turn.
        let mut stdin = runtime.stdin.lock().await;
        let write_error = claude_write_input(&mut stdin, claude_interrupt_request(&request_id))
            .await
            .err();
        if write_error.is_some() {
            active.interrupt_requested = false;
        }
        write_error
    };
    if let Some(err) = write_error {
        append_run_log(
            pool,
            run_id,
            format!("[claude] interrupt request failed: {err}\n"),
        )
        .await?;
        return Ok(false);
    }
    append_run_log(
        pool,
        run_id,
        format!("[claude] interrupt requested id={request_id}\n"),
    )
    .await?;
    Ok(true)
}

/// Fall back to the old hard stop if the CLI never ends the interrupted turn.
/// The process exit then finishes the turn as cancelled.
pub(super) async fn terminate_if_interrupt_ignored(
    pool: SqlitePool,
    agent_id: Uuid,
    runtime: Arc<WarmClaudeRuntime>,
    run_id: Uuid,
    grace: Duration,
) {
    tokio::time::sleep(grace).await;
    let still_active = {
        let state = runtime.state.lock().await;
        state.alive
            && state
                .active
                .as_ref()
                .is_some_and(|active| active.run_id == run_id)
    };
    if !still_active {
        return;
    }
    let Some(pid) = runtime.pid else {
        return;
    };
    let detail = match terminate_process_group(pid).await {
        Ok(()) => format!(
            "turn still running {}s after interrupt; sent SIGTERM to process group {pid}",
            grace.as_secs()
        ),
        Err(err) => format!("turn still running after interrupt; SIGTERM failed: {err}"),
    };
    let _ = append_run_log(&pool, run_id, format!("[claude] {detail}\n")).await;
    let _ = record_agent_activity(
        &pool,
        Some(agent_id),
        Some(run_id),
        "warning",
        "Interrupt ignored",
        detail,
    )
    .await;
}
