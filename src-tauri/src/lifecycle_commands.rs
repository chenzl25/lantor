use sqlx::{Row, SqlitePool};
use tauri::State;
use uuid::Uuid;

use crate::{
    agent_work_dispatch::dispatch_agent_restart_backlog,
    app::{to_string, AppState, CommandResult},
    events::activity::record_agent_activity,
    launch_agent,
    models::LaunchAgentStatus,
    runtime::lifecycle::{mark_agent_queued_in_tx, mark_run_stopping_in_tx},
    ui_notifications::{enqueue_ui_event, enqueue_ui_event_in_tx, notify_supervisor_wake, UiEvent},
};

#[tauri::command]
pub(crate) async fn start_agent(agent_id: Uuid, state: State<'_, AppState>) -> CommandResult<()> {
    start_agent_in_pool(&state.pool, agent_id).await
}

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

#[tauri::command]
pub(crate) async fn stop_agent(run_id: Uuid, state: State<'_, AppState>) -> CommandResult<()> {
    let row = sqlx::query(
        r#"
        select agent_id, pid, work_item_id
        from agent_runs
        where id = $1 and stopped_at is null
        "#,
    )
    .bind(run_id)
    .fetch_one(&state.pool)
    .await
    .map_err(to_string)?;

    let agent_id: Uuid = row.get("agent_id");
    let mut transaction = state.pool.begin().await.map_err(to_string)?;
    sqlx::query(
        r#"
        insert into supervisor_commands (command_type, agent_id, run_id)
        values ('stop_run', $1, $2)
        "#,
    )
    .bind(agent_id)
    .bind(run_id)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;
    mark_run_stopping_in_tx(&mut transaction, agent_id, run_id).await?;
    enqueue_ui_event_in_tx(
        &mut transaction,
        &UiEvent::Refresh {
            reason: "supervisor_command",
        },
    )
    .await?;
    transaction.commit().await.map_err(to_string)?;
    let _ = notify_supervisor_wake(&state.pool).await;
    record_agent_activity(
        &state.pool,
        Some(agent_id),
        Some(run_id),
        "run",
        "Stop requested",
        "Stop command queued for supervisor",
    )
    .await?;

    Ok(())
}

#[tauri::command]
pub(crate) async fn install_supervisor_service(
    state: State<'_, AppState>,
) -> CommandResult<LaunchAgentStatus> {
    let status = launch_agent::install_supervisor_service(state.db_url())?;
    let _ = enqueue_ui_event(
        &state.pool,
        &UiEvent::Refresh {
            reason: "supervisor_service_installed",
        },
    )
    .await;
    Ok(status)
}

#[tauri::command]
pub(crate) async fn uninstall_supervisor_service(
    state: State<'_, AppState>,
) -> CommandResult<LaunchAgentStatus> {
    let status = launch_agent::uninstall_supervisor_service()?;

    let mut transaction = state.pool.begin().await.map_err(to_string)?;
    sqlx::query("update supervisor_state set status = 'offline', updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') where id = 1")
        .execute(&mut *transaction)
        .await
        .map_err(to_string)?;
    enqueue_ui_event_in_tx(
        &mut transaction,
        &UiEvent::Refresh {
            reason: "supervisor_service_uninstalled",
        },
    )
    .await?;
    transaction.commit().await.map_err(to_string)?;
    Ok(status)
}
