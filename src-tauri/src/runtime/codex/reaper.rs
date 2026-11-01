use std::{sync::Arc, time::Duration};

use sqlx::SqlitePool;
use tokio::time::sleep;
use uuid::Uuid;

use super::{
    turn::finish_warm_codex_active_turn, WarmCodexRegistry, WarmCodexRuntime,
    CODEX_TURN_START_TIMEOUT,
};
use crate::app::CommandResult;
use crate::events::activity::record_agent_activity;
use crate::runtime::process::terminate_process_group;
use crate::runtime::warm::stop_idle_process;
use crate::ui_notifications::notify_supervisor_wake;

const CODEX_IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const CODEX_IDLE_REAPER_INTERVAL: Duration = Duration::from_secs(30);
// An in-flight turn that started (has a turn_id) but produced no codex output
// for this long is treated as wedged: codex never sent `turn/completed`, so
// without this the initiating work_item would stay `running` forever. Kept
// comfortably above any realistic single silent tool/model step.
const CODEX_TURN_STALL_TIMEOUT: Duration = Duration::from_secs(20 * 60);

// Shared teardown: finalize the active turn, kill the wedged app-server, mark
// the session failed and drop the runtime so the next wake spawns a fresh one.
async fn recover_wedged_codex_runtime(
    pool: &SqlitePool,
    registry: &WarmCodexRegistry,
    agent_id: Uuid,
    runtime: &Arc<WarmCodexRuntime>,
    pid: Option<i32>,
    detail: String,
    recovered_title: &str,
) -> CommandResult<()> {
    finish_warm_codex_active_turn(pool, agent_id, runtime, false, Some(detail.clone())).await?;
    if let Some(pid) = pid {
        if let Err(err) = terminate_process_group(pid).await {
            record_agent_activity(
                pool,
                Some(agent_id),
                None,
                "run_error",
                "Codex stuck turn stop failed",
                err,
            )
            .await?;
        }
    }
    let _ = sqlx::query(
        "update runtime_sessions set status = 'failed', updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') where agent_id = $1 and runtime = 'codex'",
    )
    .bind(agent_id)
    .execute(pool)
    .await;
    registry.remove_if_same(agent_id, runtime).await;
    record_agent_activity(
        pool,
        Some(agent_id),
        None,
        "run_error",
        recovered_title,
        detail,
    )
    .await?;
    let _ = notify_supervisor_wake(pool).await;
    Ok(())
}

async fn reap_stuck_codex_runtime(
    pool: &SqlitePool,
    registry: &WarmCodexRegistry,
    agent_id: Uuid,
    runtime: &Arc<WarmCodexRuntime>,
    source: &str,
) -> CommandResult<bool> {
    let (pid, elapsed_ms) = {
        let mut state = runtime.state.lock().await;
        let Some(active) = state.active.as_ref() else {
            return Ok(false);
        };
        let elapsed = active.started_at.elapsed();
        if active.turn_id.is_some() || elapsed < CODEX_TURN_START_TIMEOUT {
            return Ok(false);
        }
        let elapsed_ms = elapsed.as_millis();
        state.alive = false;
        (runtime.pid, elapsed_ms)
    };

    let detail = format!(
        "no turn id after {elapsed_ms} ms; source={source}; process_group={}",
        pid.map(|pid| pid.to_string())
            .unwrap_or_else(|| "unavailable".to_owned())
    );
    recover_wedged_codex_runtime(
        pool,
        registry,
        agent_id,
        runtime,
        pid,
        detail,
        "Codex zombie turn recovered",
    )
    .await?;
    Ok(true)
}

// Reap an in-flight turn that started but stalled (no codex events for
// CODEX_TURN_STALL_TIMEOUT) and would otherwise never be finalized.
async fn reap_stalled_codex_turn(
    pool: &SqlitePool,
    registry: &WarmCodexRegistry,
    agent_id: Uuid,
    runtime: &Arc<WarmCodexRuntime>,
    stalled_ms: u128,
) -> CommandResult<()> {
    let pid = runtime.pid;
    let detail = format!(
        "active turn produced no codex events for {stalled_ms} ms; source=idle_reaper; process_group={}",
        pid.map(|pid| pid.to_string())
            .unwrap_or_else(|| "unavailable".to_owned())
    );
    recover_wedged_codex_runtime(
        pool,
        registry,
        agent_id,
        runtime,
        pid,
        detail,
        "Codex stalled turn recovered",
    )
    .await
}

pub(crate) async fn reap_stuck_codex_turn(
    pool: &SqlitePool,
    registry: &WarmCodexRegistry,
    agent_id: Uuid,
    source: &str,
) -> CommandResult<bool> {
    let Some(runtime) = registry.get(agent_id).await else {
        return Ok(false);
    };
    reap_stuck_codex_runtime(pool, registry, agent_id, &runtime, source).await
}

pub(super) async fn codex_warm_idle_reaper(
    pool: SqlitePool,
    registry: WarmCodexRegistry,
    agent_id: Uuid,
    runtime: Arc<WarmCodexRuntime>,
) {
    loop {
        sleep(CODEX_IDLE_REAPER_INTERVAL).await;
        let (stop_reason, stalled_ms) = {
            let mut state = runtime.state.lock().await;
            if !state.alive {
                return;
            }
            if let Some(active) = state.active.as_ref() {
                if active.turn_id.is_none()
                    && active.started_at.elapsed() >= CODEX_TURN_START_TIMEOUT
                {
                    (Some("zombie"), 0)
                } else if active.turn_id.is_some()
                    && active.last_event_at.elapsed() >= CODEX_TURN_STALL_TIMEOUT
                {
                    let stalled_ms = active.last_event_at.elapsed().as_millis();
                    state.alive = false;
                    (Some("stalled"), stalled_ms)
                } else {
                    (None, 0)
                }
            } else if state.last_activity.elapsed() >= CODEX_IDLE_TIMEOUT {
                state.alive = false;
                (Some("idle"), 0)
            } else {
                (None, 0)
            }
        };
        let Some(stop_reason) = stop_reason else {
            continue;
        };
        if stop_reason == "zombie" {
            let _ =
                reap_stuck_codex_runtime(&pool, &registry, agent_id, &runtime, "idle_reaper").await;
            return;
        }
        if stop_reason == "stalled" {
            let _ = reap_stalled_codex_turn(&pool, &registry, agent_id, &runtime, stalled_ms).await;
            return;
        }

        stop_idle_process(&pool, agent_id, &runtime).await;
        return;
    }
}
