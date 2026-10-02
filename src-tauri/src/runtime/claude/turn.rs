use std::{sync::Arc, time::Instant};

use sqlx::SqlitePool;
use uuid::Uuid;

use super::{session::store_claude_context_tokens, ClaudeSurface, WarmClaudeRuntime};
use crate::events::activity::{record_agent_activity, work_status_title};
use crate::runtime::{
    process::{terminate_process_group, upsert_runtime_thread_id},
    streaming::{consume_streaming_agent_control_lines, finish_streaming_agent_message},
    turn_outcome::resolve_warm_turn_outcome,
};
use crate::ui_notifications::{
    enqueue_ui_agent_run_changed_in_tx, enqueue_ui_work_item_changed_in_tx, notify_supervisor_wake,
    reconcile_work_item_change,
};
use crate::{
    app::{to_string, CommandResult},
    mark_task_after_work_item_finished,
};

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

    let hidden = if success && !was_cancelled {
        consume_streaming_agent_control_lines(
            pool,
            agent_id,
            active.run_id,
            active.work_item_id,
            &active.stream_key,
        )
        .await?
    } else {
        false
    };
    if !hidden {
        finish_streaming_agent_message(
            pool,
            &active.stream_key,
            if success && !was_cancelled {
                "complete"
            } else {
                "error"
            },
        )
        .await?;
    }

    let outcome = resolve_warm_turn_outcome(success, was_cancelled);
    let log_line = if was_cancelled {
        "claude warm turn cancelled\n".to_owned()
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
    let final_work_status = if let Some(work_item_id) = active.work_item_id {
        let current_work_status: Option<String> =
            sqlx::query_scalar("select status from agent_work_items where id = $1")
                .bind(work_item_id)
                .fetch_optional(pool)
                .await
                .map_err(to_string)?;
        let was_silent = current_work_status.as_deref() == Some("silent");
        let was_held = current_work_status.as_deref() == Some("held");
        Some((
            work_item_id,
            if was_cancelled {
                "cancelled"
            } else if was_silent && success {
                "silent"
            } else if was_held && success {
                "held"
            } else if success {
                "done"
            } else {
                "failed"
            },
        ))
    } else {
        None
    };
    let mut transaction = pool.begin().await.map_err(to_string)?;
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
    .bind(outcome.run_status)
    .bind(&log_line)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;

    sqlx::query("update agents set status = $2 where id = $1")
        .bind(agent_id)
        .bind(outcome.agent_status)
        .execute(&mut *transaction)
        .await
        .map_err(to_string)?;
    enqueue_ui_agent_run_changed_in_tx(&mut transaction, active.run_id, "claude_turn_finished")
        .await?;

    if let Some((work_item_id, work_status)) = final_work_status {
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
        .execute(&mut *transaction)
        .await
        .map_err(to_string)?;
        enqueue_ui_work_item_changed_in_tx(&mut transaction, work_item_id, "work_item_finished")
            .await?;
    }
    transaction.commit().await.map_err(to_string)?;
    if let Some((work_item_id, work_status)) = final_work_status {
        mark_task_after_work_item_finished(
            pool,
            work_item_id,
            agent_id,
            active.run_id,
            work_status,
        )
        .await?;
        reconcile_work_item_change(pool, work_item_id, "work_item_finished").await?;
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

    let provider_thread_id = session_id.unwrap_or_else(|| {
        runtime
            .pid
            .map(|pid| format!("pid:{pid}"))
            .unwrap_or_else(|| "unknown".to_owned())
    });
    upsert_runtime_thread_id(
        pool,
        agent_id,
        "claude",
        &provider_thread_id,
        outcome.runtime_session_status,
    )
    .await?;
    store_claude_context_tokens(pool, agent_id, context_tokens).await?;
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
    .await?;
    let _ = notify_supervisor_wake(pool).await;
    Ok(())
}
