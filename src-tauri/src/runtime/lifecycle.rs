//! Lifecycle writes for agent runs, the work items they carry, and the agents
//! running them.
//!
//! This module is the only code that changes their statuses. A run, its work
//! item and its agent move together in one transaction with their UI events.
//! Callers decide the outcome; what follows a committed transition (task
//! updates, reconciliation, the activity log) stays with them. A test keeps
//! status writes from appearing anywhere else. See
//! docs/agent-runtime.md#run-lifecycle.

use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use uuid::Uuid;

use crate::app::{to_string, CommandResult};
use crate::runtime::turn_outcome::retry_transient_sqlite;
use crate::ui_notifications::{
    enqueue_ui_agent_run_changed_in_tx, enqueue_ui_work_item_changed_in_tx,
};

/// How a warm runtime start failed before its turn was actually sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WarmStartFailure {
    /// Another turn already occupies the runtime: requeue the request and keep
    /// the agent running so it is picked up once that turn finishes.
    RuntimeBusy,
    /// The runtime is healthy but the start hit a transient problem (database
    /// contention, runtime exited moments earlier): requeue and retry a bounded
    /// number of times, then fail the request without flagging the agent.
    Transient,
    /// The turn cannot run at all: fail the request and flag the agent.
    Fatal,
}

/// Maximum number of runs a single request may burn on transient start failures.
pub(crate) const MAX_WARM_START_ATTEMPTS: i64 = 3;

/// The end of a warm turn: the run closes, the agent takes its next status,
/// and the work item, if any, finishes or goes back to the queue.
pub(crate) struct TurnEnd<'a> {
    pub(crate) agent_id: Uuid,
    pub(crate) agent_status: &'a str,
    pub(crate) run_id: Uuid,
    pub(crate) run_status: &'a str,
    pub(crate) log_line: &'a str,
    pub(crate) run_reason: &'static str,
    pub(crate) work: Option<(Uuid, &'a str)>,
}

/// Closes the run and settles its work item in one transaction, retrying
/// transient lock errors. A `queued` work item is handed back to the
/// scheduler rather than completed.
pub(crate) async fn end_turn(pool: &SqlitePool, end: &TurnEnd<'_>) -> CommandResult<()> {
    retry_transient_sqlite(|| end_turn_once(pool, end)).await
}

async fn end_turn_once(pool: &SqlitePool, end: &TurnEnd<'_>) -> CommandResult<()> {
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(to_string)?;
    close_run_in_tx(
        &mut transaction,
        end.run_id,
        end.run_status,
        None,
        end.log_line,
    )
    .await?;
    set_agent_status_in_tx(&mut transaction, end.agent_id, end.agent_status).await?;
    enqueue_ui_agent_run_changed_in_tx(&mut transaction, end.run_id, end.run_reason).await?;
    if let Some((work_item_id, work_status)) = end.work {
        if work_status == "queued" {
            requeue_work_item_in_tx(&mut transaction, work_item_id, "work_item_requeued").await?;
        } else {
            finish_work_item_in_tx(
                &mut transaction,
                work_item_id,
                work_status,
                "work_item_finished",
            )
            .await?;
        }
    }
    transaction.commit().await.map_err(to_string)
}

/// The end of a cold (one process per run) run once its process has exited.
pub(crate) struct ColdRunEnd<'a> {
    pub(crate) agent_id: Uuid,
    pub(crate) agent_status: &'a str,
    pub(crate) run_id: Uuid,
    pub(crate) run_status: &'a str,
    pub(crate) exit_code: Option<i32>,
    pub(crate) log_line: &'a str,
    pub(crate) work: Option<(Uuid, &'a str)>,
}

pub(crate) async fn end_cold_run(pool: &SqlitePool, end: &ColdRunEnd<'_>) -> CommandResult<()> {
    let mut transaction = pool.begin().await.map_err(to_string)?;
    close_run_in_tx(
        &mut transaction,
        end.run_id,
        end.run_status,
        end.exit_code,
        end.log_line,
    )
    .await?;
    set_agent_status_in_tx(&mut transaction, end.agent_id, end.agent_status).await?;
    enqueue_ui_agent_run_changed_in_tx(&mut transaction, end.run_id, "run_finished").await?;
    if let Some((work_item_id, work_status)) = end.work {
        finish_work_item_in_tx(
            &mut transaction,
            work_item_id,
            work_status,
            "work_item_finished",
        )
        .await?;
    }
    transaction.commit().await.map_err(to_string)
}

/// A cold run whose process could not be spawned: the run and its work item
/// fail and the agent is flagged.
pub(crate) async fn fail_cold_launch(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
    work_item_id: Option<Uuid>,
    log_line: &str,
) -> CommandResult<()> {
    let mut transaction = pool.begin().await.map_err(to_string)?;
    close_run_in_tx(&mut transaction, run_id, "failed", None, log_line).await?;
    set_agent_status_in_tx(&mut transaction, agent_id, "error").await?;
    enqueue_ui_agent_run_changed_in_tx(&mut transaction, run_id, "run_failed").await?;
    if let Some(work_item_id) = work_item_id {
        finish_work_item_in_tx(&mut transaction, work_item_id, "failed", "work_item_failed")
            .await?;
    }
    transaction.commit().await.map_err(to_string)
}

/// What a failed warm start did with its work item.
pub(crate) struct WarmStartFailed {
    pub(crate) requeued: bool,
    /// Failed runs of the work item so far, the current one included; counted
    /// only for [`WarmStartFailure::Transient`] failures, otherwise 0.
    pub(crate) attempts: i64,
}

/// A warm turn that failed after its run and work item rows were created but
/// before the turn started. Without this the run stays `running` and the
/// agent looks busy until the next app restart.
pub(crate) async fn fail_warm_start(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
    work_item_id: Option<Uuid>,
    error_log: &str,
    failure: WarmStartFailure,
) -> CommandResult<WarmStartFailed> {
    let mut transaction = pool.begin().await.map_err(to_string)?;
    close_run_in_tx(&mut transaction, run_id, "failed", None, error_log).await?;

    // Every failed start of a request leaves a failed run behind, so the run
    // history doubles as the attempt counter (the current run included).
    let attempts = match (failure, work_item_id) {
        (WarmStartFailure::Transient, Some(work_item_id)) => sqlx::query_scalar::<_, i64>(
            "select count(*) from agent_runs where work_item_id = $1 and status = 'failed'",
        )
        .bind(work_item_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(to_string)?,
        _ => 0,
    };
    let requeued = match failure {
        WarmStartFailure::RuntimeBusy => true,
        WarmStartFailure::Transient => work_item_id.is_some() && attempts < MAX_WARM_START_ATTEMPTS,
        WarmStartFailure::Fatal => false,
    };

    match failure {
        WarmStartFailure::RuntimeBusy => {
            set_agent_status_in_tx(&mut transaction, agent_id, "running").await?;
        }
        WarmStartFailure::Transient => {
            // The runtime is fine, so the agent goes back to idle unless another
            // run of it is still live (codex can start turns concurrently).
            sqlx::query(
                r#"
                update agents
                set status = 'idle'
                where id = $1
                  and not exists (
                      select 1
                      from agent_runs
                      where agent_id = $1
                        and stopped_at is null
                        and status in ('starting', 'running', 'stopping')
                  )
                "#,
            )
            .bind(agent_id)
            .execute(&mut *transaction)
            .await
            .map_err(to_string)?;
        }
        WarmStartFailure::Fatal => {
            set_agent_status_in_tx(&mut transaction, agent_id, "error").await?;
        }
    }
    enqueue_ui_agent_run_changed_in_tx(&mut transaction, run_id, "run_failed").await?;

    if let Some(work_item_id) = work_item_id {
        if requeued {
            requeue_work_item_in_tx(&mut transaction, work_item_id, "work_item_queued").await?;
        } else {
            finish_work_item_in_tx(&mut transaction, work_item_id, "failed", "work_item_failed")
                .await?;
        }
    }
    transaction.commit().await.map_err(to_string)?;
    Ok(WarmStartFailed { requeued, attempts })
}

/// A work item that fails before any run started for it (its agent's budget
/// is spent, or its runtime could not be spawned).
pub(crate) async fn fail_work_item_before_run(
    pool: &SqlitePool,
    work_item_id: Uuid,
    reason: &'static str,
) -> CommandResult<()> {
    let mut transaction = pool.begin().await.map_err(to_string)?;
    finish_work_item_in_tx(&mut transaction, work_item_id, "failed", reason).await?;
    transaction.commit().await.map_err(to_string)
}

/// A warm runtime process that could not be spawned flags its agent.
pub(crate) async fn mark_agent_runtime_failed(
    pool: &SqlitePool,
    agent_id: Uuid,
) -> CommandResult<()> {
    sqlx::query("update agents set status = 'error' where id = $1")
        .bind(agent_id)
        .execute(pool)
        .await
        .map_err(to_string)?;
    Ok(())
}

/// A queued agent whose provider needs a login goes back to idle; its request
/// stays queued for when the login works again.
pub(crate) async fn idle_agent_waiting_for_login(
    pool: &SqlitePool,
    agent_id: Uuid,
) -> CommandResult<()> {
    sqlx::query("update agents set status = 'idle' where id = $1 and status = 'queued'")
        .bind(agent_id)
        .execute(pool)
        .await
        .map_err(to_string)?;
    Ok(())
}

/// At supervisor startup no run is live any more: close every run still
/// marked live, fail their running work items, finish cancels nothing can
/// complete now, and return busy agents to idle.
pub(crate) async fn close_orphaned_runs_at_startup(pool: &SqlitePool) -> CommandResult<()> {
    sqlx::query(
        r#"
        update agent_runs
        set status = 'unknown', stopped_at = coalesce(stopped_at, strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
        where stopped_at is null and status in ('starting', 'running', 'stopping')
        "#,
    )
    .execute(pool)
    .await
    .map_err(to_string)?;
    sqlx::query(
        "update agents set status = 'idle' where status in ('queued', 'running', 'stopping')",
    )
    .execute(pool)
    .await
    .map_err(to_string)?;
    sqlx::query(
        r#"
        update agent_work_items
        set status = 'failed',
            completed_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where status = 'running'
        "#,
    )
    .execute(pool)
    .await
    .map_err(to_string)?;
    // A `cancelling` item can only be completed by a live stop path; after a
    // restart none exists, so finalize it instead of leaving it stuck forever.
    sqlx::query(
        r#"
        update agent_work_items
        set status = 'cancelled',
            completed_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where status = 'cancelling'
        "#,
    )
    .execute(pool)
    .await
    .map_err(to_string)?;

    Ok(())
}

/// Closes a live run no runtime owns. Returns the work items it finished with
/// their new status, or `None` when the run had already stopped.
pub(crate) async fn close_orphaned_run(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
) -> CommandResult<Option<Vec<(Uuid, String)>>> {
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(to_string)?;
    let closed = sqlx::query(
        r#"
        update agent_runs
        set status = 'unknown',
            log = substr(log || $2, -20000),
            stopped_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1 and stopped_at is null
        "#,
    )
    .bind(run_id)
    .bind("[lantor] no live runtime owned this run; closed by the orphan sweep\n")
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?
    .rows_affected()
        > 0;
    if !closed {
        return Ok(None);
    }
    let work_items: Vec<(Uuid, String)> = sqlx::query(
        r#"
        update agent_work_items
        set status = case status when 'cancelling' then 'cancelled' else 'failed' end,
            completed_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where run_id = $1 and status in ('running', 'cancelling')
        returning id, status
        "#,
    )
    .bind(run_id)
    .fetch_all(&mut *transaction)
    .await
    .map_err(to_string)?
    .into_iter()
    .map(|row| (row.get("id"), row.get("status")))
    .collect();
    sqlx::query(
        "update agents set status = 'idle' where id = $1 and status in ('running', 'stopping')",
    )
    .bind(agent_id)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;
    enqueue_ui_agent_run_changed_in_tx(&mut transaction, run_id, "orphaned_run_closed").await?;
    for (work_item_id, _) in &work_items {
        enqueue_ui_work_item_changed_in_tx(&mut transaction, *work_item_id, "work_item_finished")
            .await?;
    }
    transaction.commit().await.map_err(to_string)?;
    Ok(Some(work_items))
}

/// Marks a live run and its agent as stopping, in the caller's transaction,
/// while the stop request reaches the process.
pub(crate) async fn mark_run_stopping_in_tx(
    transaction: &mut Transaction<'_, Sqlite>,
    agent_id: Uuid,
    run_id: Uuid,
) -> CommandResult<()> {
    sqlx::query("update agent_runs set status = 'stopping' where id = $1")
        .bind(run_id)
        .execute(&mut **transaction)
        .await
        .map_err(to_string)?;
    sqlx::query("update agents set status = 'stopping' where id = $1")
        .bind(agent_id)
        .execute(&mut **transaction)
        .await
        .map_err(to_string)?;
    enqueue_ui_agent_run_changed_in_tx(transaction, run_id, "run_stopping").await
}

/// A run the supervisor was asked to stop is marked stopping, and its work
/// item, if still queued or running, is marked cancelling until the run ends.
pub(crate) async fn mark_run_stopping(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
    work_item_id: Option<Uuid>,
) -> CommandResult<()> {
    let mut transaction = pool.begin().await.map_err(to_string)?;
    mark_run_stopping_in_tx(&mut transaction, agent_id, run_id).await?;
    if let Some(work_item_id) = work_item_id {
        sqlx::query(
            r#"
            update agent_work_items
            set status = 'cancelling',
                updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
            where id = $1 and status in ('queued', 'running')
            "#,
        )
        .bind(work_item_id)
        .execute(&mut *transaction)
        .await
        .map_err(to_string)?;
        enqueue_ui_work_item_changed_in_tx(&mut transaction, work_item_id, "work_item_cancelling")
            .await?;
    }
    transaction.commit().await.map_err(to_string)
}

/// A run asked to stop that never published a process: it ends as cancelled
/// and its agent returns to idle.
pub(crate) async fn cancel_run_without_process(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
) -> CommandResult<()> {
    let mut transaction = pool.begin().await.map_err(to_string)?;
    sqlx::query(
        r#"
        update agent_runs
        set status = 'cancelled',
            stopped_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1 and stopped_at is null
        "#,
    )
    .bind(run_id)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;
    sqlx::query(
        "update agents set status = 'idle' where id = $1 and status in ('queued', 'running', 'stopping')",
    )
    .bind(agent_id)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;
    enqueue_ui_agent_run_changed_in_tx(&mut transaction, run_id, "run_finished").await?;
    transaction.commit().await.map_err(to_string)
}

/// Cancels a work item whose run can no longer finish it, if it is still
/// queued, running or cancelling.
pub(crate) async fn cancel_unfinished_work_item(
    pool: &SqlitePool,
    work_item_id: Uuid,
) -> CommandResult<()> {
    let mut transaction = pool.begin().await.map_err(to_string)?;
    sqlx::query(
        r#"
        update agent_work_items
        set status = 'cancelled',
            completed_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1 and status in ('queued', 'running', 'cancelling')
        "#,
    )
    .bind(work_item_id)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;
    enqueue_ui_work_item_changed_in_tx(&mut transaction, work_item_id, "work_item_cancelled")
        .await?;
    transaction.commit().await.map_err(to_string)
}

/// Cancels a work item the owner cancelled while it was queued, in the
/// caller's transaction. Returns `false`, changing nothing, when the item is
/// no longer queued: its turn started or it finished after the owner's read.
pub(crate) async fn cancel_queued_work_item_in_tx(
    transaction: &mut Transaction<'_, Sqlite>,
    work_item_id: Uuid,
) -> CommandResult<bool> {
    let cancelled = sqlx::query(
        r#"
        update agent_work_items
        set status = 'cancelled',
            completed_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1 and status = 'queued'
        "#,
    )
    .bind(work_item_id)
    .execute(&mut **transaction)
    .await
    .map_err(to_string)?
    .rows_affected()
        > 0;
    if cancelled {
        enqueue_ui_work_item_changed_in_tx(transaction, work_item_id, "work_item_cancelled")
            .await?;
    }
    Ok(cancelled)
}

/// Marks a running work item the owner cancelled as cancelling until its run
/// stops. Returns `false`, changing nothing, when the item is no longer
/// running on `run_id`.
pub(crate) async fn mark_work_item_cancelling(
    pool: &SqlitePool,
    work_item_id: Uuid,
    run_id: Uuid,
) -> CommandResult<bool> {
    let mut transaction = pool.begin().await.map_err(to_string)?;
    let marked = sqlx::query(
        r#"
        update agent_work_items
        set status = 'cancelling',
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1 and status = 'running' and run_id = $2
        "#,
    )
    .bind(work_item_id)
    .bind(run_id)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?
    .rows_affected()
        > 0;
    if !marked {
        return Ok(false);
    }
    enqueue_ui_work_item_changed_in_tx(&mut transaction, work_item_id, "work_item_cancelling")
        .await?;
    transaction.commit().await.map_err(to_string)?;
    Ok(true)
}

/// Finishes a work item left `cancelling` after its run ended.
pub(crate) async fn finish_cancelling_work_item(
    pool: &SqlitePool,
    work_item_id: Uuid,
) -> CommandResult<()> {
    let mut transaction = pool.begin().await.map_err(to_string)?;
    sqlx::query(
        r#"
        update agent_work_items
        set status = 'cancelled',
            completed_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1 and status = 'cancelling'
        "#,
    )
    .bind(work_item_id)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;
    enqueue_ui_work_item_changed_in_tx(&mut transaction, work_item_id, "work_item_cancelled")
        .await?;
    transaction.commit().await.map_err(to_string)
}

/// Queues a start for an agent, in the caller's transaction. An agent whose
/// work joins a turn that is already running stays running.
pub(crate) async fn mark_agent_queued_in_tx(
    transaction: &mut Transaction<'_, Sqlite>,
    agent_id: Uuid,
    joins_active_turn: bool,
) -> CommandResult<()> {
    sqlx::query("update agents set status = 'queued' where id = $1")
        .bind(agent_id)
        .execute(&mut **transaction)
        .await
        .map_err(to_string)?;
    if joins_active_turn {
        set_agent_status_in_tx(transaction, agent_id, "running").await?;
    }
    Ok(())
}

/// A run about to start: what it runs, where, and the request it carries.
pub(crate) struct NewRun<'a> {
    pub(crate) agent_id: Uuid,
    pub(crate) work_item_id: Option<Uuid>,
    pub(crate) command: &'a str,
    pub(crate) working_directory: &'a str,
    pub(crate) log: &'a str,
}

/// Creates a `starting` run in the caller's transaction and moves its work
/// item, if any, to running on it.
pub(crate) async fn create_run_in_tx(
    transaction: &mut Transaction<'_, Sqlite>,
    run: &NewRun<'_>,
) -> CommandResult<Uuid> {
    let run_id: Uuid = sqlx::query_scalar(
        r#"
        insert into agent_runs (agent_id, work_item_id, command, working_directory, status, log)
        values ($1, $2, $3, $4, 'starting', $5)
        returning id
        "#,
    )
    .bind(run.agent_id)
    .bind(run.work_item_id)
    .bind(run.command)
    .bind(run.working_directory)
    .bind(run.log)
    .fetch_one(&mut **transaction)
    .await
    .map_err(to_string)?;
    enqueue_ui_agent_run_changed_in_tx(transaction, run_id, "run_created").await?;
    if let Some(work_item_id) = run.work_item_id {
        mark_work_item_running_in_tx(transaction, work_item_id, run_id).await?;
    }
    Ok(run_id)
}

/// A run whose process is up: the run is running with its pid and so is the
/// agent. Runs in the caller's transaction.
pub(crate) async fn mark_run_running_in_tx(
    transaction: &mut Transaction<'_, Sqlite>,
    agent_id: Uuid,
    run_id: Uuid,
    pid: Option<i32>,
) -> CommandResult<()> {
    sqlx::query("update agent_runs set status = 'running', pid = $2 where id = $1")
        .bind(run_id)
        .bind(pid)
        .execute(&mut **transaction)
        .await
        .map_err(to_string)?;
    set_agent_status_in_tx(transaction, agent_id, "running").await?;
    enqueue_ui_agent_run_changed_in_tx(transaction, run_id, "run_running").await
}

/// Starts a run for a Claude follow-up that the CLI took as its own turn: the
/// run is created already running on the warm process.
pub(crate) async fn start_follow_up_run(
    pool: &SqlitePool,
    run: &NewRun<'_>,
    pid: Option<i32>,
) -> CommandResult<Uuid> {
    let mut transaction = pool.begin().await.map_err(to_string)?;
    let run_id: Uuid = sqlx::query_scalar(
        r#"
        insert into agent_runs (agent_id, work_item_id, command, working_directory, status, log, pid)
        values ($1, $2, $3, $4, 'running', $5, $6)
        returning id
        "#,
    )
    .bind(run.agent_id)
    .bind(run.work_item_id)
    .bind(run.command)
    .bind(run.working_directory)
    .bind(run.log)
    .bind(pid)
    .fetch_one(&mut *transaction)
    .await
    .map_err(to_string)?;
    enqueue_ui_agent_run_changed_in_tx(&mut transaction, run_id, "run_created").await?;
    if let Some(work_item_id) = run.work_item_id {
        mark_work_item_running_in_tx(&mut transaction, work_item_id, run_id).await?;
    }
    set_agent_status_in_tx(&mut transaction, run.agent_id, "running").await?;
    enqueue_ui_agent_run_changed_in_tx(&mut transaction, run_id, "run_running").await?;
    transaction.commit().await.map_err(to_string)?;
    Ok(run_id)
}

/// Joins a work item to a running Codex turn as a steer.
pub(crate) async fn join_work_item_to_run(
    pool: &SqlitePool,
    work_item_id: Uuid,
    run_id: Uuid,
) -> CommandResult<()> {
    let mut transaction = pool.begin().await.map_err(to_string)?;
    mark_work_item_running_in_tx(&mut transaction, work_item_id, run_id).await?;
    transaction.commit().await.map_err(to_string)
}

/// A Codex steer that could not be sent goes back to the queue. Unlike a
/// requeue after a turn, this leaves `completed_at` as it was.
pub(crate) async fn return_steer_to_queue(
    pool: &SqlitePool,
    work_item_id: Uuid,
) -> CommandResult<()> {
    let mut transaction = pool.begin().await.map_err(to_string)?;
    sqlx::query(
        r#"
        update agent_work_items
        set status = 'queued',
            run_id = null,
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1
        "#,
    )
    .bind(work_item_id)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;
    enqueue_ui_work_item_changed_in_tx(&mut transaction, work_item_id, "work_item_queued").await?;
    transaction.commit().await.map_err(to_string)
}

/// Settles a Codex steer when the turn it joined ends: `done` or `cancelled`
/// with that turn's run, or back to `queued` without one.
pub(crate) async fn settle_codex_steer(
    pool: &SqlitePool,
    work_item_id: Uuid,
    status: &str,
    run_id: Option<Uuid>,
    completed: bool,
) -> CommandResult<()> {
    let completed_at = if completed {
        "strftime('%Y-%m-%dT%H:%M:%f+00:00','now')"
    } else {
        "null"
    };
    let mut transaction = pool.begin().await.map_err(to_string)?;
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
    .bind(work_item_id)
    .bind(status)
    .bind(run_id)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;
    enqueue_ui_work_item_changed_in_tx(&mut transaction, work_item_id, "codex_turn_steer_result")
        .await?;
    transaction.commit().await.map_err(to_string)
}

/// A Claude follow-up the running turn consumed is done with that turn's run.
pub(crate) async fn finish_merged_follow_up(
    pool: &SqlitePool,
    work_item_id: Uuid,
    run_id: Uuid,
) -> CommandResult<()> {
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
    .bind(work_item_id)
    .bind(run_id)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;
    enqueue_ui_work_item_changed_in_tx(&mut transaction, work_item_id, "claude_follow_up_merged")
        .await?;
    transaction.commit().await.map_err(to_string)
}

/// Moves a Claude follow-up's work item from `from` to `to`; false if it was
/// not in `from`.
pub(crate) async fn move_follow_up_status(
    pool: &SqlitePool,
    work_item_id: Uuid,
    from: &str,
    to: &str,
    run_id: Option<Uuid>,
    reason: &str,
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
    Ok(true)
}

/// A work item whose agent chose not to reply visibly, unless it already
/// failed or was cancelled.
pub(crate) async fn mark_work_item_silent(
    pool: &SqlitePool,
    work_item_id: Uuid,
) -> CommandResult<()> {
    let mut transaction = pool.begin().await.map_err(to_string)?;
    sqlx::query(
        r#"
        update agent_work_items
        set status = 'silent',
            completed_at = coalesce(completed_at, strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1
          and status not in ('cancelled', 'failed')
        "#,
    )
    .bind(work_item_id)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;
    enqueue_ui_work_item_changed_in_tx(&mut transaction, work_item_id, "work_item_silent").await?;
    transaction.commit().await.map_err(to_string)
}

/// A work item whose reply was held back because newer messages arrived,
/// unless it already failed, was cancelled or went silent. Runs in the
/// caller's transaction, which also records the held reply.
pub(crate) async fn mark_work_item_held_in_tx(
    transaction: &mut Transaction<'_, Sqlite>,
    work_item_id: Uuid,
) -> CommandResult<()> {
    sqlx::query(
        r#"
        update agent_work_items
        set status = 'held',
            completed_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1
          and status not in ('cancelled', 'failed', 'silent')
        "#,
    )
    .bind(work_item_id)
    .execute(&mut **transaction)
    .await
    .map_err(to_string)?;
    enqueue_ui_work_item_changed_in_tx(transaction, work_item_id, "work_item_held").await
}

async fn close_run_in_tx(
    transaction: &mut Transaction<'_, Sqlite>,
    run_id: Uuid,
    status: &str,
    exit_code: Option<i32>,
    log_line: &str,
) -> CommandResult<()> {
    sqlx::query(
        r#"
        update agent_runs
        set status = $2,
            exit_code = $3,
            log = substr(log || $4, -20000),
            stopped_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1
        "#,
    )
    .bind(run_id)
    .bind(status)
    .bind(exit_code)
    .bind(log_line)
    .execute(&mut **transaction)
    .await
    .map_err(to_string)?;
    Ok(())
}

async fn set_agent_status_in_tx(
    transaction: &mut Transaction<'_, Sqlite>,
    agent_id: Uuid,
    status: &str,
) -> CommandResult<()> {
    sqlx::query("update agents set status = $2 where id = $1")
        .bind(agent_id)
        .bind(status)
        .execute(&mut **transaction)
        .await
        .map_err(to_string)?;
    Ok(())
}

async fn finish_work_item_in_tx(
    transaction: &mut Transaction<'_, Sqlite>,
    work_item_id: Uuid,
    status: &str,
    reason: &str,
) -> CommandResult<()> {
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
    .bind(status)
    .execute(&mut **transaction)
    .await
    .map_err(to_string)?;
    enqueue_ui_work_item_changed_in_tx(transaction, work_item_id, reason).await
}

async fn requeue_work_item_in_tx(
    transaction: &mut Transaction<'_, Sqlite>,
    work_item_id: Uuid,
    reason: &str,
) -> CommandResult<()> {
    sqlx::query(
        r#"
        update agent_work_items
        set status = 'queued',
            run_id = null,
            completed_at = null,
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1
        "#,
    )
    .bind(work_item_id)
    .execute(&mut **transaction)
    .await
    .map_err(to_string)?;
    enqueue_ui_work_item_changed_in_tx(transaction, work_item_id, reason).await
}

async fn mark_work_item_running_in_tx(
    transaction: &mut Transaction<'_, Sqlite>,
    work_item_id: Uuid,
    run_id: Uuid,
) -> CommandResult<()> {
    sqlx::query(
        r#"
        update agent_work_items
        set status = 'running',
            run_id = $2,
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1
        "#,
    )
    .bind(work_item_id)
    .bind(run_id)
    .execute(&mut **transaction)
    .await
    .map_err(to_string)?;
    enqueue_ui_work_item_changed_in_tx(transaction, work_item_id, "work_item_running").await
}

#[cfg(test)]
#[path = "../tests/runtime_lifecycle.rs"]
mod tests;
