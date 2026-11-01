//! Lifecycle writes for agent runs, the work items they carry, and the agents
//! running them.
//!
//! Every path that ends a run or a work item, or stops one, writes the new
//! states here: the run, its work item and its agent move together in one
//! transaction with their UI events. Callers decide the outcome; what follows
//! a committed transition (task updates, reconciliation, the activity log)
//! stays with them. See docs/agent-runtime.md#run-lifecycle.

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
/// caller's transaction. Unlike the other cancels this does not recheck the
/// status it read before the transaction.
pub(crate) async fn cancel_queued_work_item_in_tx(
    transaction: &mut Transaction<'_, Sqlite>,
    work_item_id: Uuid,
) -> CommandResult<()> {
    finish_work_item_in_tx(
        transaction,
        work_item_id,
        "cancelled",
        "work_item_cancelled",
    )
    .await
}

/// Marks a running work item the owner cancelled as cancelling until its run
/// stops. Unlike [`mark_run_stopping`] this does not recheck the status it
/// read before the transaction.
pub(crate) async fn mark_work_item_cancelling(
    pool: &SqlitePool,
    work_item_id: Uuid,
) -> CommandResult<()> {
    let mut transaction = pool.begin().await.map_err(to_string)?;
    sqlx::query(
        r#"
        update agent_work_items
        set status = 'cancelling',
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1
        "#,
    )
    .bind(work_item_id)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;
    enqueue_ui_work_item_changed_in_tx(&mut transaction, work_item_id, "work_item_cancelling")
        .await?;
    transaction.commit().await.map_err(to_string)
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
