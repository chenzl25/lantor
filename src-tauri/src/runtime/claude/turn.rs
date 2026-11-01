use std::{sync::Arc, time::Instant};

use sqlx::SqlitePool;
use uuid::Uuid;

use super::{
    session::store_claude_context_tokens, steer::settle_claude_follow_ups, ClaudeSurface,
    WarmClaudeRuntime,
};
use crate::events::activity::{record_agent_activity, work_status_title};
use crate::provider_auth::{record_provider_auth_failure, record_provider_turn_success};
use crate::runtime::warm::load_work_item_status;
use crate::runtime::{
    lifecycle::{end_turn, TurnEnd},
    process::{terminate_process_group, upsert_runtime_thread_id},
    streaming::{
        consume_streaming_agent_control_lines, delete_streaming_agent_message_by_key,
        finish_streaming_agent_message,
    },
    turn_outcome::{
        finished_work_status, keep_first_error, resolve_warm_turn_outcome, retry_transient_sqlite,
    },
};
use crate::ui_notifications::{notify_supervisor_wake, reconcile_work_item_change};
use crate::{app::CommandResult, mark_task_after_work_item_finished};

pub(super) async fn finish_warm_claude_active_turn(
    pool: &SqlitePool,
    agent_id: Uuid,
    runtime: &Arc<WarmClaudeRuntime>,
    success: bool,
    error: Option<String>,
) -> CommandResult<()> {
    let (active, session_id, context_tokens) = {
        let mut state = runtime.state.lock().await;
        state.last_activity = Instant::now();
        let mut active = state.active.take();
        if success {
            if let Some(next_memory_context) = active
                .as_mut()
                .and_then(|active| active.pending_memory_context.take())
            {
                state.injected_memory_context = next_memory_context;
            }
        }
        if let Some(active) = active.as_ref() {
            state.last_surface = Some(ClaudeSurface {
                channel_id: active.channel_id,
                thread_root_id: active.thread_root_id,
            });
        }
        let session_id = state.session_id.clone();
        (active, session_id, state.context_tokens)
    };
    let Some(active) = active else {
        // A process that died between turns still owes its follow-ups a turn.
        return settle_claude_follow_ups(pool, agent_id, runtime).await;
    };
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
    // Runs without a work item have no `cancelling` status to read, so an
    // interrupted turn's error result must count as a stop on its own.
    let was_cancelled =
        active.interrupt_requested || current_work_status.as_deref() == Some("cancelling");
    let auth_required = !success && !was_cancelled && active.auth_required;

    if auth_required {
        // The CLI's "Failed to authenticate" text is not an answer; the
        // request runs again once the login works.
        keep_first_error(
            &mut first_error,
            delete_streaming_agent_message_by_key(
                pool,
                &active.stream_key,
                "provider_auth_required",
            )
            .await,
        );
    } else {
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
    }

    let outcome = resolve_warm_turn_outcome(success, was_cancelled);
    let log_line = if was_cancelled {
        "claude warm turn cancelled\n".to_owned()
    } else if auth_required {
        format!(
            "claude warm turn needs login; request requeued: {}\n",
            error.as_deref().unwrap_or("authentication failed")
        )
    } else {
        error
            .as_ref()
            .map(|error| format!("claude warm turn failed: {error}\n"))
            .unwrap_or_else(|| format!("claude warm turn completed in {elapsed_ms} ms\n"))
    };
    if outcome.should_reset_runtime {
        {
            let mut state = runtime.state.lock().await;
            state.alive = false;
        }
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
                finished_work_status(success, was_cancelled, auth_required, current.as_deref()),
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
                run_reason: "claude_turn_finished",
                work: final_work_status,
            },
        )
        .await,
    );
    if let Some((work_item_id, work_status)) = final_work_status {
        if work_status == "queued" {
            keep_first_error(
                &mut first_error,
                reconcile_work_item_change(pool, work_item_id, "work_item_requeued").await,
            );
        } else {
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
        }
        keep_first_error(
            &mut first_error,
            record_agent_activity(
                pool,
                Some(agent_id),
                Some(active.run_id),
                "dispatch",
                if auth_required {
                    "Request requeued until login works"
                } else {
                    work_status_title(work_status)
                },
                work_item_id.to_string(),
            )
            .await,
        );
    }

    let provider_thread_id = session_id.unwrap_or_else(|| {
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
            "claude",
            &provider_thread_id,
            outcome.runtime_session_status,
        )
        .await,
    );
    keep_first_error(
        &mut first_error,
        store_claude_context_tokens(pool, agent_id, context_tokens).await,
    );
    keep_first_error(
        &mut first_error,
        record_agent_activity(
            pool,
            Some(agent_id),
            Some(active.run_id),
            outcome.activity_kind,
            if auth_required {
                "Login required"
            } else {
                outcome.activity_title
            },
            if success || was_cancelled {
                format!("duration={elapsed_ms} ms")
            } else {
                log_line.trim().to_owned()
            },
        )
        .await,
    );
    if auth_required {
        keep_first_error(
            &mut first_error,
            record_provider_auth_failure(
                pool,
                "claude",
                active.channel_id,
                error.as_deref().unwrap_or_default(),
            )
            .await,
        );
    } else if success && !was_cancelled {
        keep_first_error(
            &mut first_error,
            record_provider_turn_success(pool, "claude", active.run_id).await,
        );
    }
    keep_first_error(
        &mut first_error,
        settle_claude_follow_ups(pool, agent_id, runtime).await,
    );
    let _ = notify_supervisor_wake(pool).await;
    first_error.map_or(Ok(()), Err)
}
