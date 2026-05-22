use std::sync::Arc;

use sqlx::SqlitePool;
use uuid::Uuid;

use super::{CodexSteerRequest, WarmCodexRuntime};
use crate::events::activity::{record_agent_activity, work_status_title};
use crate::runtime::{
    process::upsert_runtime_thread_id,
    streaming::{
        consume_streaming_agent_control_lines, dispatch_streaming_agent_message_mentions,
        finish_streaming_agent_message,
    },
};
use crate::ui_notifications::{
    notify_supervisor_wake, notify_ui_agent_run_changed, notify_ui_work_item_changed,
};
use crate::{
    app::{to_string, CommandResult},
    mark_task_after_work_item_finished,
};

pub(super) async fn finish_codex_steer_request(
    pool: &SqlitePool,
    agent_id: Uuid,
    steer: CodexSteerRequest,
    success: bool,
    error: Option<String>,
) -> CommandResult<()> {
    let (status, completed_at, run_id) = if success {
        (
            "done",
            "strftime('%Y-%m-%dT%H:%M:%f+00:00','now')",
            Some(steer.run_id),
        )
    } else {
        ("queued", "null", None)
    };
    sqlx::query(&format!(
        r#"
        update agent_work_items
        set status = $2,
            run_id = $3,
            completed_at = {completed_at},
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1
        "#
    ))
    .bind(steer.work_item_id)
    .bind(status)
    .bind(run_id)
    .execute(pool)
    .await
    .map_err(to_string)?;
    notify_ui_work_item_changed(pool, steer.work_item_id, "codex_turn_steer_result").await;

    record_agent_activity(
        pool,
        Some(agent_id),
        Some(steer.run_id),
        if success { "dispatch" } else { "run_error" },
        if success {
            "Follow-up added"
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
        state.active.take()
    };
    let Some(active) = active else {
        return Ok(());
    };
    let elapsed_ms = active.started_at.elapsed().as_millis();
    let current_work_status: Option<String> = match active.work_item_id {
        Some(work_item_id) => {
            sqlx::query_scalar("select status from agent_work_items where id = $1")
                .bind(work_item_id)
                .fetch_optional(pool)
                .await
                .map_err(to_string)?
        }
        None => None,
    };
    let was_cancelled = current_work_status.as_deref() == Some("cancelling");

    for stream_key in active.stream_keys {
        let hidden = if success && !was_cancelled {
            consume_streaming_agent_control_lines(
                pool,
                agent_id,
                active.run_id,
                active.work_item_id,
                &stream_key,
            )
            .await?
        } else {
            false
        };
        if !hidden {
            if active
                .completed_agent_message_stream_keys
                .contains(&stream_key)
            {
                finish_streaming_agent_message(pool, &stream_key, "complete").await?;
                dispatch_streaming_agent_message_mentions(pool, &stream_key).await?;
            } else {
                finish_streaming_agent_message(
                    pool,
                    &stream_key,
                    if success && !was_cancelled {
                        "complete"
                    } else {
                        "error"
                    },
                )
                .await?;
            }
        }
    }

    for steer in active.steer_requests.into_values() {
        finish_codex_steer_request(
            pool,
            agent_id,
            steer,
            success && !was_cancelled,
            if was_cancelled {
                Some("cancelled".to_owned())
            } else {
                error.clone()
            },
        )
        .await?;
    }

    let run_status = if was_cancelled {
        "cancelled"
    } else if success {
        "exited"
    } else {
        "failed"
    };
    let agent_status = if success || was_cancelled {
        "idle"
    } else {
        "error"
    };
    let log_line = if was_cancelled {
        "codex warm turn cancelled\n".to_owned()
    } else {
        error
            .as_ref()
            .map(|error| format!("codex warm turn failed: {error}\n"))
            .unwrap_or_else(|| format!("codex warm turn completed in {elapsed_ms} ms\n"))
    };
    sqlx::query(
        r#"
        update agent_runs
        set status = $2,
            exit_code = null,
            log = substr(log || $3, -20000),
            stopped_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1
        "#,
    )
    .bind(active.run_id)
    .bind(run_status)
    .bind(&log_line)
    .execute(pool)
    .await
    .map_err(to_string)?;
    notify_ui_agent_run_changed(pool, active.run_id, "codex_turn_finished").await;

    sqlx::query("update agents set status = $2 where id = $1")
        .bind(agent_id)
        .bind(agent_status)
        .execute(pool)
        .await
        .map_err(to_string)?;

    if let Some(work_item_id) = active.work_item_id {
        let current_work_status: Option<String> =
            sqlx::query_scalar("select status from agent_work_items where id = $1")
                .bind(work_item_id)
                .fetch_optional(pool)
                .await
                .map_err(to_string)?;
        let was_silent = current_work_status.as_deref() == Some("silent");
        let work_status = if was_cancelled {
            "cancelled"
        } else if was_silent && success {
            "silent"
        } else if success {
            "done"
        } else {
            "failed"
        };
        sqlx::query(
            r#"
            update agent_work_items
            set status = $2,
                completed_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
                updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
            where id = $1
            "#,
        )
        .bind(work_item_id)
        .bind(work_status)
        .execute(pool)
        .await
        .map_err(to_string)?;
        mark_task_after_work_item_finished(
            pool,
            work_item_id,
            agent_id,
            active.run_id,
            work_status,
        )
        .await?;
        notify_ui_work_item_changed(pool, work_item_id, "work_item_finished").await;
        record_agent_activity(
            pool,
            Some(agent_id),
            Some(active.run_id),
            "dispatch",
            work_status_title(work_status),
            work_item_id.to_string(),
        )
        .await?;
    }

    upsert_runtime_thread_id(
        pool,
        agent_id,
        "codex",
        &runtime.thread_id,
        if success || was_cancelled {
            "idle"
        } else {
            "failed"
        },
    )
    .await?;
    record_agent_activity(
        pool,
        Some(agent_id),
        Some(active.run_id),
        if success || was_cancelled {
            "run"
        } else {
            "run_error"
        },
        if was_cancelled {
            "Stopped"
        } else if success {
            "Completed"
        } else {
            "Failed"
        },
        if success || was_cancelled {
            format!("duration={elapsed_ms} ms")
        } else {
            log_line.trim().to_owned()
        },
    )
    .await?;
    let _ = notify_supervisor_wake(pool).await;
    Ok(())
}
