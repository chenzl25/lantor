use sqlx::SqlitePool;
use uuid::Uuid;

use crate::{
    agent_work_dispatch::dispatch_agent_restart_backlog,
    app::{to_string, CommandResult},
    events::activity::record_agent_activity,
    runtime::{lifecycle::mark_agent_queued_in_tx, warm::request_restart_in_tx},
    ui_notifications::{enqueue_ui_event_in_tx, notify_supervisor_wake, UiEvent},
};

pub(crate) async fn start_agent_in_pool(pool: &SqlitePool, agent_id: Uuid) -> CommandResult<()> {
    let active_run: Option<Uuid> = sqlx::query_scalar(
        r#"
        select id
        from agent_runs
        where agent_id = $1
          and stopped_at is null
          and status in ('starting', 'running', 'stopping')
        limit 1
        "#,
    )
    .bind(agent_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;

    if active_run.is_some() {
        return Err("agent already has an active run".to_owned());
    }

    let pending_start: Option<Uuid> = sqlx::query_scalar(
        r#"
        select id
        from supervisor_commands
        where command_type = 'start_agent'
          and agent_id = $1
          and status in ('pending', 'running')
        limit 1
        "#,
    )
    .bind(agent_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;

    if pending_start.is_some() {
        return Err("agent already has a pending start command".to_owned());
    }

    let mut transaction = pool.begin().await.map_err(to_string)?;
    mark_agent_queued_in_tx(&mut transaction, agent_id, false).await?;
    request_restart_in_tx(&mut transaction, agent_id).await?;
    enqueue_ui_event_in_tx(
        &mut transaction,
        &UiEvent::Refresh {
            reason: "agent_queued",
        },
    )
    .await?;
    transaction.commit().await.map_err(to_string)?;
    let (redispatched_tasks, inbox_wake_available) =
        dispatch_agent_restart_backlog(pool, agent_id).await?;
    let pending_start_after_backlog: Option<Uuid> = sqlx::query_scalar(
        r#"
        select id
        from supervisor_commands
        where command_type = 'start_agent'
          and agent_id = $1
          and status in ('pending', 'running')
        limit 1
        "#,
    )
    .bind(agent_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;
    if pending_start_after_backlog.is_none() {
        let mut transaction = pool.begin().await.map_err(to_string)?;
        sqlx::query(
            r#"
            insert into supervisor_commands (command_type, agent_id)
            values ('start_agent', $1)
            "#,
        )
        .bind(agent_id)
        .execute(&mut *transaction)
        .await
        .map_err(to_string)?;
        enqueue_ui_event_in_tx(
            &mut transaction,
            &UiEvent::Refresh {
                reason: "supervisor_command",
            },
        )
        .await?;
        transaction.commit().await.map_err(to_string)?;
        let _ = notify_supervisor_wake(pool).await;
    }
    record_agent_activity(
        pool,
        Some(agent_id),
        None,
        "run",
        "Start queued",
        if redispatched_tasks > 0 || inbox_wake_available {
            format!(
                "Waiting for supervisor to launch the agent; redispatched {redispatched_tasks} unfinished task(s)"
            )
        } else {
            "Waiting for supervisor to launch the agent".to_owned()
        },
    )
    .await?;

    Ok(())
}
