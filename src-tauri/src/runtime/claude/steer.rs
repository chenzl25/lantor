//! Same-surface follow-ups for a busy warm Claude runtime.
//!
//! A follow-up is written to the CLI's stdin as another user message while a
//! turn runs. The CLI replays it, with the uuid Lantor gave it, at the moment
//! it consumes it. A replay before the turn's result means it joined that
//! turn (Claude Code merges input at tool-call boundaries); otherwise the CLI
//! runs it as its own turn right after the result, and Lantor gives that turn
//! a run of its own. Merging is observed CLI behavior, not a documented
//! contract, so nothing here assumes it happens.

use std::{sync::Arc, time::Instant};

use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use super::{
    protocol::{claude_stream_key, claude_user_input_with_uuid, claude_write_input},
    ClaudeActiveTurn, ClaudeTextState, WarmClaudeRegistry, WarmClaudeRuntime,
};
use crate::agent_memory::append_run_log;
use crate::app::{to_string, CommandResult};
use crate::events::activity::record_agent_activity;
use crate::freshness::advance_agent_target_watermark_for_work_item;
use crate::runtime::{
    lifecycle::WarmStartFailure, process::cleanup_failed_warm_start,
    streaming::ensure_streaming_agent_message, surface::same_codex_surface,
};
use crate::ui_notifications::{
    enqueue_ui_agent_run_changed_in_tx, enqueue_ui_work_item_changed_in_tx,
    reconcile_work_item_change,
};
use crate::{build_steer_followup_prompt, load_inbox_wake_items_for_work_item};

/// A follow-up written to the CLI that it has not replayed yet.
#[derive(Clone, Debug)]
pub(super) struct ClaudePendingSteer {
    pub(super) uuid: String,
    pub(super) work_item_id: Uuid,
    pub(super) channel_id: Option<Uuid>,
    pub(super) thread_root_id: Option<Uuid>,
    /// Context the follow-up prompt showed, for the freshness watermark.
    pub(super) seen_seq: i64,
    pub(super) command_text: String,
    pub(super) working_directory: String,
}

pub(super) struct ClaudeSteerRequest<'a> {
    pub(super) work_item_id: Uuid,
    /// The full work item prompt, used when no compact follow-up applies.
    pub(super) prompt: &'a str,
    pub(super) rendered_context_max_seq: i64,
    pub(super) command_text: &'a str,
    pub(super) working_directory: &'a str,
}

/// Surface of the active turn when it can take a follow-up.
/// Whether the agent's warm Claude process still owns a turn (running, or
/// owed for a written follow-up). Runs of an agent without one are orphans.
pub(crate) async fn claude_runtime_owns_turn(
    registry: &WarmClaudeRegistry,
    agent_id: Uuid,
) -> bool {
    let runtime = {
        let runtimes = registry.runtimes.lock().await;
        runtimes.get(&agent_id).cloned()
    };
    match runtime {
        Some(runtime) => runtime.state.lock().await.is_busy(),
        None => false,
    }
}

pub(crate) async fn steerable_claude_turn_surface(
    registry: &WarmClaudeRegistry,
    agent_id: Uuid,
) -> Option<(Option<Uuid>, Option<Uuid>)> {
    let runtime = {
        let runtimes = registry.runtimes.lock().await;
        runtimes.get(&agent_id).cloned()
    }?;
    let state = runtime.state.lock().await;
    if !state.alive {
        return None;
    }
    let active = state.active.as_ref()?;
    (!active.interrupt_requested).then_some((active.channel_id, active.thread_root_id))
}

pub(super) async fn steer_warm_claude_turn_if_same_surface(
    pool: &SqlitePool,
    agent_id: Uuid,
    runtime: &Arc<WarmClaudeRuntime>,
    request: ClaudeSteerRequest<'_>,
) -> CommandResult<bool> {
    let work_item_id = request.work_item_id;
    let row = sqlx::query(
        r#"
        select channel_id, thread_root_id, source_kind
        from agent_work_items
        where id = $1 and agent_id = $2 and status = 'queued'
        "#,
    )
    .bind(work_item_id)
    .bind(agent_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;
    let Some(row) = row else {
        return Ok(false);
    };
    let channel_id: Option<Uuid> = row.get("channel_id");
    let thread_root_id: Option<Uuid> = row.get("thread_root_id");
    let source_kind: String = row.get("source_kind");

    let Some((active_run_id, active_channel_id, active_thread_root_id)) = ({
        let state = runtime.state.lock().await;
        state
            .active
            .as_ref()
            .filter(|active| state.alive && !active.interrupt_requested)
            .map(|active| (active.run_id, active.channel_id, active.thread_root_id))
    }) else {
        return Ok(false);
    };
    if !same_codex_surface(
        pool,
        channel_id,
        thread_root_id,
        active_channel_id,
        active_thread_root_id,
    )
    .await?
    {
        return Ok(false);
    }

    let compact_items = if source_kind == "inbox_wake" {
        load_inbox_wake_items_for_work_item(pool, work_item_id).await?
    } else {
        Vec::new()
    };
    let (prompt, seen_seq) = if compact_items.is_empty() {
        (request.prompt.to_owned(), request.rendered_context_max_seq)
    } else {
        (build_steer_followup_prompt(&compact_items), 0)
    };

    // Attach before registering: a run that later adopts the follow-up sets
    // its own run id, and must not be overwritten by this one.
    if !set_follow_up_status(
        pool,
        work_item_id,
        "queued",
        "running",
        Some(active_run_id),
        "claude_follow_up_attached",
    )
    .await?
    {
        return Ok(false);
    }
    let steer = ClaudePendingSteer {
        uuid: Uuid::new_v4().to_string(),
        work_item_id,
        channel_id,
        thread_root_id,
        seen_seq,
        command_text: request.command_text.to_owned(),
        working_directory: request.working_directory.to_owned(),
    };
    let registered = {
        let mut state = runtime.state.lock().await;
        let same_turn = state.alive
            && state.active.as_ref().is_some_and(|active| {
                active.run_id == active_run_id && !active.interrupt_requested
            });
        if same_turn {
            state.pending_steers.push(steer.clone());
            state.last_activity = Instant::now();
        }
        same_turn
    };
    if !registered {
        set_follow_up_status(
            pool,
            work_item_id,
            "running",
            "queued",
            None,
            "claude_follow_up_queued",
        )
        .await?;
        return Ok(false);
    }

    let write_result = {
        let mut stdin = runtime.stdin.lock().await;
        claude_write_input(
            &mut stdin,
            claude_user_input_with_uuid(&prompt, &steer.uuid),
        )
        .await
    };
    if let Err(err) = write_result {
        if take_pending_steer(runtime, &steer.uuid).await.is_some() {
            set_follow_up_status(
                pool,
                work_item_id,
                "running",
                "queued",
                None,
                "claude_follow_up_queued",
            )
            .await?;
        }
        return Err(err);
    }
    append_run_log(
        pool,
        active_run_id,
        format!(
            "[claude] follow-up written work_item={work_item_id} uuid={}\n",
            steer.uuid
        ),
    )
    .await?;
    Ok(true)
}

/// Settle a follow-up the CLI just consumed. If the active turn is not the
/// follow-up's own, it joined that turn.
pub(super) async fn note_claude_input_replayed(
    pool: &SqlitePool,
    agent_id: Uuid,
    runtime: &Arc<WarmClaudeRuntime>,
    uuid: &str,
) -> CommandResult<()> {
    let (steer, merged_into) = {
        let mut state = runtime.state.lock().await;
        let Some(index) = state
            .pending_steers
            .iter()
            .position(|steer| steer.uuid == uuid)
        else {
            return Ok(());
        };
        let steer = state.pending_steers.remove(index);
        state.last_activity = Instant::now();
        let merged_into = state
            .active
            .as_ref()
            .filter(|active| active.work_item_id != Some(steer.work_item_id))
            .map(|active| active.run_id);
        (steer, merged_into)
    };
    // Joined or not, the model has now seen the follow-up.
    advance_agent_target_watermark_for_work_item(
        pool,
        agent_id,
        steer.work_item_id,
        steer.seen_seq,
    )
    .await?;
    let Some(run_id) = merged_into else {
        return Ok(());
    };
    let mut transaction = pool.begin().await.map_err(to_string)?;
    sqlx::query(
        r#"
        update agent_work_items
        set status = 'done',
            run_id = $2,
            completed_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1
        "#,
    )
    .bind(steer.work_item_id)
    .bind(run_id)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;
    enqueue_ui_work_item_changed_in_tx(
        &mut transaction,
        steer.work_item_id,
        "claude_follow_up_merged",
    )
    .await?;
    transaction.commit().await.map_err(to_string)?;
    reconcile_work_item_change(pool, steer.work_item_id, "claude_follow_up_merged").await?;
    append_run_log(
        pool,
        run_id,
        format!(
            "[claude] follow-up joined the turn work_item={}\n",
            steer.work_item_id
        ),
    )
    .await?;
    record_agent_activity(
        pool,
        Some(agent_id),
        Some(run_id),
        "dispatch",
        "Follow-up added",
        steer.work_item_id.to_string(),
    )
    .await
}

/// Called once a turn has ended. A live CLI runs every follow-up it has not
/// replayed yet as a new turn (an interrupt does not drop them), so the next
/// one gets a run; a dead or reset process never will, so they are requeued.
pub(super) async fn settle_claude_follow_ups(
    pool: &SqlitePool,
    agent_id: Uuid,
    runtime: &Arc<WarmClaudeRuntime>,
) -> CommandResult<()> {
    let next = {
        let mut state = runtime.state.lock().await;
        if state.pending_steers.is_empty() || state.active.is_some() {
            return Ok(());
        }
        if !state.alive {
            let steers = std::mem::take(&mut state.pending_steers);
            drop(state);
            for steer in steers {
                requeue_follow_up(pool, agent_id, &steer).await?;
            }
            return Ok(());
        }
        state.pending_steers[0].clone()
    };
    adopt_follow_up_turn(pool, agent_id, runtime, next).await
}

async fn adopt_follow_up_turn(
    pool: &SqlitePool,
    agent_id: Uuid,
    runtime: &Arc<WarmClaudeRuntime>,
    steer: ClaudePendingSteer,
) -> CommandResult<()> {
    let mut transaction = pool.begin().await.map_err(to_string)?;
    let run_id: Uuid = sqlx::query_scalar(
        r#"
        insert into agent_runs (agent_id, work_item_id, command, working_directory, status, log, pid)
        values ($1, $2, $3, $4, 'running', $5, $6)
        returning id
        "#,
    )
    .bind(agent_id)
    .bind(steer.work_item_id)
    .bind(&steer.command_text)
    .bind(&steer.working_directory)
    .bind(format!(
        "$ {}\n[warm process reused]\n[follow-up turn uuid={}]\n",
        steer.command_text, steer.uuid
    ))
    .bind(runtime.pid)
    .fetch_one(&mut *transaction)
    .await
    .map_err(to_string)?;
    enqueue_ui_agent_run_changed_in_tx(&mut transaction, run_id, "run_created").await?;
    sqlx::query(
        r#"
        update agent_work_items
        set status = 'running',
            run_id = $2,
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1
        "#,
    )
    .bind(steer.work_item_id)
    .bind(run_id)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;
    enqueue_ui_work_item_changed_in_tx(&mut transaction, steer.work_item_id, "work_item_running")
        .await?;
    sqlx::query("update agents set status = 'running' where id = $1")
        .bind(agent_id)
        .execute(&mut *transaction)
        .await
        .map_err(to_string)?;
    enqueue_ui_agent_run_changed_in_tx(&mut transaction, run_id, "run_running").await?;
    transaction.commit().await.map_err(to_string)?;
    reconcile_work_item_change(pool, steer.work_item_id, "work_item_running").await?;
    record_agent_activity(
        pool,
        Some(agent_id),
        Some(run_id),
        "dispatch",
        "Follow-up started",
        steer.work_item_id.to_string(),
    )
    .await?;

    let stream_key = claude_stream_key(run_id);
    let mut failure = None;
    if let Some(channel_id) = steer.channel_id {
        if let Err(err) = ensure_streaming_agent_message(
            pool,
            agent_id,
            channel_id,
            steer.thread_root_id,
            &stream_key,
        )
        .await
        {
            failure = Some(err);
        }
    }
    if failure.is_none() {
        let mut state = runtime.state.lock().await;
        if state.alive && state.active.is_none() {
            state.last_activity = Instant::now();
            state.active = Some(ClaudeActiveTurn {
                run_id,
                started_at: Instant::now(),
                first_delta_at: None,
                work_item_id: Some(steer.work_item_id),
                channel_id: steer.channel_id,
                thread_root_id: steer.thread_root_id,
                stream_key,
                pending_memory_context: None,
                text: ClaudeTextState::default(),
                interrupt_requested: false,
                auth_required: false,
            });
        } else {
            failure = Some("Claude warm runtime changed before the follow-up turn".to_owned());
        }
    }
    let Some(err) = failure else {
        return Ok(());
    };
    // Without an owner the turn's output is dropped; stop waiting on its replay.
    take_pending_steer(runtime, &steer.uuid).await;
    cleanup_failed_warm_start(
        pool,
        "claude",
        agent_id,
        run_id,
        Some(steer.work_item_id),
        &err,
        WarmStartFailure::Transient,
    )
    .await?;
    Err(err)
}

async fn take_pending_steer(
    runtime: &Arc<WarmClaudeRuntime>,
    uuid: &str,
) -> Option<ClaudePendingSteer> {
    let mut state = runtime.state.lock().await;
    let index = state
        .pending_steers
        .iter()
        .position(|steer| steer.uuid == uuid)?;
    Some(state.pending_steers.remove(index))
}

async fn requeue_follow_up(
    pool: &SqlitePool,
    agent_id: Uuid,
    steer: &ClaudePendingSteer,
) -> CommandResult<()> {
    if !set_follow_up_status(
        pool,
        steer.work_item_id,
        "running",
        "queued",
        None,
        "claude_follow_up_queued",
    )
    .await?
    {
        return Ok(());
    }
    record_agent_activity(
        pool,
        Some(agent_id),
        None,
        "dispatch",
        "Follow-up queued",
        steer.work_item_id.to_string(),
    )
    .await
}

/// Move a follow-up's work item from `from` to `to`; false if it was not in `from`.
async fn set_follow_up_status(
    pool: &SqlitePool,
    work_item_id: Uuid,
    from: &str,
    to: &str,
    run_id: Option<Uuid>,
    reason: &'static str,
) -> CommandResult<bool> {
    let mut transaction = pool.begin().await.map_err(to_string)?;
    let updated = sqlx::query(
        r#"
        update agent_work_items
        set status = $3,
            run_id = $4,
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1 and status = $2
        "#,
    )
    .bind(work_item_id)
    .bind(from)
    .bind(to)
    .bind(run_id)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?
    .rows_affected();
    if updated == 0 {
        return Ok(false);
    }
    enqueue_ui_work_item_changed_in_tx(&mut transaction, work_item_id, reason).await?;
    transaction.commit().await.map_err(to_string)?;
    reconcile_work_item_change(pool, work_item_id, reason).await?;
    Ok(true)
}
