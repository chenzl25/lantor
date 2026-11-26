use std::{sync::Arc, time::Instant};

use sqlx::SqlitePool;
use uuid::Uuid;

use super::{AntigravitySurface, WarmAntigravityRuntime, ANTIGRAVITY_RUNTIME};
use crate::events::activity::{record_agent_activity, work_status_title};
use crate::runtime::warm::load_work_item_status;
use crate::runtime::{
    lifecycle::{end_turn, TurnEnd},
    process::{terminate_process_group, upsert_runtime_thread_id},
    session_context::{store_injected_memory, store_surface_delivery},
    streaming::{consume_streaming_agent_control_lines, finish_streaming_agent_message},
    turn_outcome::{
        finished_work_status, keep_first_error, resolve_warm_turn_outcome, retry_transient_sqlite,
    },
};
use crate::ui_notifications::{notify_supervisor_wake, reconcile_work_item_change};
use crate::{app::CommandResult, mark_task_after_work_item_finished};

/// Ends the turn in progress, if any: records what the conversation now
/// holds, finishes the reply, and closes the run and its request.
pub(super) async fn finish_warm_antigravity_turn(
    pool: &SqlitePool,
    agent_id: Uuid,
    runtime: &Arc<WarmAntigravityRuntime>,
    success: bool,
    error: Option<String>,
) -> CommandResult<()> {
    let mut injected_memory = None;
    let (active, conversation_id) = {
        let mut state = runtime.state.lock().await;
        state.last_activity = Instant::now();
        let mut active = state.active.take();
        if success {
            if let Some(next_memory_context) = active
                .as_mut()
                .and_then(|active| active.pending_memory_context.take())
            {
                state.injected_memory_context = next_memory_context.clone();
                injected_memory = Some(next_memory_context);
            }
        }
        if let Some(active) = active.as_ref() {
            state.last_surface = Some(AntigravitySurface {
                channel_id: active.channel_id,
                thread_root_id: active.thread_root_id,
            });
        }
        (active, state.conversation_id.clone())
    };
    let Some(active) = active else {
        return Ok(());
    };
    if let (true, Some(conversation_id)) = (success, conversation_id.as_deref()) {
        // The conversation now holds what this turn's prompt brought. Losing
        // the record only makes a later prompt repeat it.
        if let Some(memory) = &injected_memory {
            let _ = store_injected_memory(
                pool,
                agent_id,
                ANTIGRAVITY_RUNTIME,
                conversation_id,
                memory.as_deref(),
            )
            .await;
        }
        if let (Some(channel_id), Some(seq)) = (active.channel_id, active.surface_delivered_seq) {
            let _ = store_surface_delivery(
                pool,
                agent_id,
                channel_id,
                active.thread_root_id,
                conversation_id,
                seq,
            )
            .await;
        }
    }
    // From here on the turn has left memory, so every step below is best
    // effort except the terminal write: a failed step must not leave the run
    // open, which would block the agent's queue until a restart.
    let mut first_error = None;
    let elapsed_ms = active.started_at.elapsed().as_millis();
    let current_work_status: Option<String> = match active.work_item_id {
        Some(work_item_id) => keep_first_error(
            &mut first_error,
            load_work_item_status(pool, work_item_id).await,
        )
        .flatten(),
        None => None,
    };
    // A stop ends the process, so the turn ends as a failure; the request's
    // `cancelling` status is what makes it a stop.
    let was_cancelled = current_work_status.as_deref() == Some("cancelling");
    let hidden = if success && !was_cancelled {
        keep_first_error(
            &mut first_error,
            consume_streaming_agent_control_lines(
                pool,
                agent_id,
                active.run_id,
                active.work_item_id,
                &active.stream_key,
            )
            .await,
        )
        .unwrap_or(false)
    } else {
        false
    };
    if !hidden {
        let delivery_state = if success && !was_cancelled {
            "complete"
        } else {
            "error"
        };
        keep_first_error(
            &mut first_error,
            retry_transient_sqlite(|| {
                finish_streaming_agent_message(pool, &active.stream_key, delivery_state)
            })
            .await,
        );
    }

    let outcome = resolve_warm_turn_outcome(success, was_cancelled);
    let log_line = if was_cancelled {
        "agy warm turn cancelled\n".to_owned()
    } else {
        error
            .as_ref()
            .map(|error| format!("agy warm turn failed: {error}\n"))
            .unwrap_or_else(|| format!("agy warm turn completed in {elapsed_ms} ms\n"))
    };
    if outcome.should_reset_runtime {
        runtime.state.lock().await.alive = false;
        if let Some(pid) = runtime.pid {
            let _ = terminate_process_group(pid).await;
        }
    }
    let final_work_status = match active.work_item_id {
        Some(work_item_id) => {
            let current = keep_first_error(
                &mut first_error,
                load_work_item_status(pool, work_item_id).await,
            )
            .flatten();
            Some((
                work_item_id,
                finished_work_status(success, was_cancelled, false, current.as_deref()),
            ))
        }
        None => None,
    };
    // If even the retried write fails, the supervisor's orphan sweep closes
    // the run later; the remaining steps still run.
    keep_first_error(
        &mut first_error,
        end_turn(
            pool,
            &TurnEnd {
                agent_id,
                agent_status: outcome.agent_status,
                run_id: active.run_id,
                run_status: outcome.run_status,
                log_line: &log_line,
                run_reason: "antigravity_turn_finished",
                work: final_work_status,
                network_retry: None,
            },
        )
        .await,
    );
    if let Some((work_item_id, work_status)) = final_work_status {
        keep_first_error(
            &mut first_error,
            mark_task_after_work_item_finished(
                pool,
                work_item_id,
                agent_id,
                active.run_id,
                work_status,
            )
            .await,
        );
        keep_first_error(
            &mut first_error,
            reconcile_work_item_change(pool, work_item_id, "work_item_finished").await,
        );
        keep_first_error(
            &mut first_error,
            record_agent_activity(
                pool,
                Some(agent_id),
                Some(active.run_id),
                "dispatch",
                work_status_title(work_status),
                work_item_id.to_string(),
            )
            .await,
        );
    }
    let provider_thread_id = conversation_id.unwrap_or_else(|| {
        runtime
            .pid
            .map(|pid| format!("pid:{pid}"))
            .unwrap_or_else(|| "unknown".to_owned())
    });
    keep_first_error(
        &mut first_error,
        upsert_runtime_thread_id(
            pool,
            agent_id,
            ANTIGRAVITY_RUNTIME,
            &provider_thread_id,
            outcome.runtime_session_status,
        )
        .await,
    );
    keep_first_error(
        &mut first_error,
        record_agent_activity(
            pool,
            Some(agent_id),
            Some(active.run_id),
            outcome.activity_kind,
            outcome.activity_title,
            if success || was_cancelled {
                format!("duration={elapsed_ms} ms")
            } else {
                log_line.trim().to_owned()
            },
        )
        .await,
    );
    let _ = notify_supervisor_wake(pool).await;
    first_error.map_or(Ok(()), Err)
}
