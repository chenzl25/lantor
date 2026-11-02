use std::sync::Arc;

use sqlx::SqlitePool;
use uuid::Uuid;

use super::{CodexSteerRequest, WarmCodexRuntime};
use crate::events::activity::{record_agent_activity, work_status_title};
use crate::freshness::advance_agent_target_watermark_for_work_item;
use crate::provider_auth::{record_provider_auth_failure, record_provider_turn_success};
use crate::runtime::warm::load_work_item_status;
use crate::runtime::{
    lifecycle::{end_turn, settle_codex_steer, TurnEnd},
    process::{terminate_process_group, upsert_runtime_thread_id},
    streaming::{
        consume_streaming_agent_control_lines, delete_streaming_agent_message_by_key,
        dispatch_streaming_agent_message_mentions, finish_streaming_agent_message,
        finish_streaming_agent_message_deferred_mentions,
    },
    turn_outcome::{
        finished_work_status, keep_first_error, resolve_warm_turn_outcome, retry_transient_sqlite,
    },
};
use crate::ui_notifications::{notify_supervisor_wake, reconcile_work_item_change};
use crate::{app::CommandResult, mark_task_after_work_item_finished};

pub(super) async fn finish_codex_steer_request(
    pool: &SqlitePool,
    agent_id: Uuid,
    steer: CodexSteerRequest,
    success: bool,
    cancelled: bool,
    error: Option<String>,
) -> CommandResult<()> {
    let (status, completed, run_id) = if success {
        ("done", true, Some(steer.run_id))
    } else if cancelled {
        // The user stopped this turn; follow-ups merged into it must not
        // requeue and revive the agent.
        ("cancelled", true, Some(steer.run_id))
    } else {
        ("queued", false, None)
    };
    settle_codex_steer(pool, steer.work_item_id, status, run_id, completed).await?;
    reconcile_work_item_change(pool, steer.work_item_id, "codex_turn_steer_result").await?;
    if success {
        advance_agent_target_watermark_for_work_item(
            pool,
            agent_id,
            steer.work_item_id,
            steer.seen_seq,
        )
        .await?;
    } else if cancelled {
        let _ = mark_task_after_work_item_finished(
            pool,
            steer.work_item_id,
            agent_id,
            steer.run_id,
            "cancelled",
        )
        .await;
    }

    record_agent_activity(
        pool,
        Some(agent_id),
        Some(steer.run_id),
        if success || cancelled {
            "dispatch"
        } else {
            "run_error"
        },
        if success {
            "Follow-up added"
        } else if cancelled {
            "Follow-up cancelled"
        } else {
            "Follow-up queued"
        },
        error.unwrap_or_else(|| steer.work_item_id.to_string()),
    )
    .await?;
    Ok(())
}

pub(super) async fn finish_warm_codex_active_turn(
    pool: &SqlitePool,
    agent_id: Uuid,
    runtime: &Arc<WarmCodexRuntime>,
    success: bool,
    error: Option<String>,
) -> CommandResult<()> {
    let active = {
        let mut state = runtime.state.lock().await;
        state.last_activity = std::time::Instant::now();
        let mut active = state.active.take();
        if success {
            if let Some(next_memory_context) = active
                .as_mut()
                .and_then(|active| active.pending_memory_context.take())
            {
                state.injected_memory_context = next_memory_context;
            }
        }
        active
    };
    let Some(active) = active else {
        return Ok(());
    };
    // The turn has left memory: every step below is best effort except the
    // terminal write, so a failed step cannot leave the run open.
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
    let was_cancelled = current_work_status.as_deref() == Some("cancelling");
    let auth_required = !success && !was_cancelled && active.auth_required;

    let final_stream_key = active.latest_agent_message_stream_key.clone();
    for stream_key in active.stream_keys {
        if auth_required
            || final_stream_key
                .as_ref()
                .is_some_and(|final_key| final_key != &stream_key)
        {
            keep_first_error(
                &mut first_error,
                delete_streaming_agent_message_by_key(
                    pool,
                    &stream_key,
                    if auth_required {
                        "provider_auth_required"
                    } else {
                        "superseded_intermediate_reply"
                    },
                )
                .await,
            );
            continue;
        }

        let hidden = if success && !was_cancelled {
            keep_first_error(
                &mut first_error,
                consume_streaming_agent_control_lines(
                    pool,
                    agent_id,
                    active.run_id,
                    active.work_item_id,
                    &stream_key,
                )
                .await,
            )
            .unwrap_or(false)
        } else {
            false
        };
        if !hidden {
            if success
                && !was_cancelled
                && active
                    .completed_agent_message_stream_keys
                    .contains(&stream_key)
            {
                keep_first_error(
                    &mut first_error,
                    retry_transient_sqlite(|| {
                        finish_streaming_agent_message_deferred_mentions(
                            pool,
                            &stream_key,
                            "complete",
                        )
                    })
                    .await,
                );
                keep_first_error(
                    &mut first_error,
                    dispatch_streaming_agent_message_mentions(pool, &stream_key).await,
                );
            } else {
                let delivery_state = if success && !was_cancelled {
                    "complete"
                } else {
                    "error"
                };
                keep_first_error(
                    &mut first_error,
                    retry_transient_sqlite(|| {
                        finish_streaming_agent_message(pool, &stream_key, delivery_state)
                    })
                    .await,
                );
            }
        }
    }

    for steer in active.steer_requests.into_values() {
        keep_first_error(
            &mut first_error,
            finish_codex_steer_request(
                pool,
                agent_id,
                steer,
                success && !was_cancelled,
                was_cancelled,
                if was_cancelled {
                    Some("cancelled".to_owned())
                } else {
                    error.clone()
                },
            )
            .await,
        );
    }

    let outcome = resolve_warm_turn_outcome(success, was_cancelled);
    let log_line = if was_cancelled {
        "codex warm turn cancelled\n".to_owned()
    } else if auth_required {
        format!(
            "codex warm turn needs login; request requeued: {}\n",
            error.as_deref().unwrap_or("unauthorized")
        )
    } else {
        error
            .as_ref()
            .map(|error| format!("codex warm turn failed: {error}\n"))
            .unwrap_or_else(|| format!("codex warm turn completed in {elapsed_ms} ms\n"))
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
                run_reason: "codex_turn_finished",
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

    keep_first_error(
        &mut first_error,
        upsert_runtime_thread_id(
            pool,
            agent_id,
            "codex",
            &runtime.provider.thread_id,
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
                "codex",
                active.channel_id,
                error.as_deref().unwrap_or_default(),
            )
            .await,
        );
    } else if success && !was_cancelled {
        keep_first_error(
            &mut first_error,
            record_provider_turn_success(pool, "codex", active.run_id).await,
        );
    }
    let _ = notify_supervisor_wake(pool).await;
    first_error.map_or(Ok(()), Err)
}
