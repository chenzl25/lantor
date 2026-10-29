use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use sqlx::{Row, SqlitePool};
use tokio::{sync::Semaphore, time::sleep};
use uuid::Uuid;

use super::{
    claude::{self, WarmClaudeRegistry},
    codex::{self, WarmCodexRegistry},
    process::{
        effective_launch_command, start_process_agent, terminate_process_group, ProcessAgentLaunch,
    },
    surface::{
        append_claude_thread_context_with_seq, same_codex_surface, CodexActiveTurnScheduleState,
    },
};
use crate::{
    agent_inbox_wake::{
        agent_has_active_or_pending_start, agent_runtime, enqueue_agent_work_if_available,
    },
    agent_memory::append_run_log,
    app::{to_string, CommandResult},
    db::{acquire_supervisor_lock, db_connect_with_url, db_url, migrate},
    events::activity::record_agent_activity,
    models::{SupervisorCommand, SupervisorStatus},
    prompts::{
        build_streaming_work_item_prompt, build_work_item_prompt, load_agent_memory_context,
        prepend_memory_context,
    },
    provider_auth::{provider_needs_login, start_due_provider_probes},
    ui_notifications::{
        enqueue_ui_agent_run_changed_in_tx, enqueue_ui_work_item_changed_in_tx,
        reconcile_work_item_change,
    },
    usage::agent_budget_exhausted,
};

const SUPERVISOR_COMMAND_CONCURRENCY: usize = 4;
const SUPERVISOR_IDLE_SLEEP: Duration = Duration::from_secs(2);
const SUPERVISOR_ERROR_BACKOFF_INITIAL: Duration = Duration::from_secs(1);
const SUPERVISOR_ERROR_BACKOFF_MAX: Duration = Duration::from_secs(10);
const SUPERVISOR_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(30);
/// A run younger than this may still be starting its process; the orphan
/// sweep leaves it alone.
const ORPHAN_RUN_GRACE_SECS: i64 = 120;

pub(crate) async fn load_supervisor_status(pool: &SqlitePool) -> CommandResult<SupervisorStatus> {
    let row = sqlx::query(
        r#"
        select pid, status, updated_at
        from supervisor_state
        where id = 1
        "#,
    )
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;

    let Some(row) = row else {
        return Ok(SupervisorStatus {
            pid: None,
            status: "offline".to_owned(),
            updated_at: None,
        });
    };

    let updated_at: DateTime<Utc> = row.get("updated_at");
    let status = if Utc::now().signed_duration_since(updated_at).num_seconds() > 10 {
        "stale".to_owned()
    } else {
        row.get("status")
    };

    Ok(SupervisorStatus {
        pid: row.get("pid"),
        status,
        updated_at: Some(updated_at),
    })
}

pub(crate) async fn mark_orphaned_agent_runs(pool: &SqlitePool) -> CommandResult<()> {
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

/// The startup sweep, per run and while running: a run still marked live
/// whose agent has no warm process owning a turn will never be finished by
/// anyone (its finalization failed, or its process vanished). Close it so the
/// agent's queue moves again instead of waiting for a supervisor restart.
pub(crate) async fn close_orphaned_agent_runs(
    pool: &SqlitePool,
    codex_registry: &WarmCodexRegistry,
    claude_registry: &WarmClaudeRegistry,
) -> CommandResult<()> {
    let rows = sqlx::query(
        r#"
        select r.id, r.agent_id, lower(a.runtime) as runtime
        from agent_runs r
        join agents a on a.id = r.agent_id
        where r.stopped_at is null
          and r.status in ('starting', 'running', 'stopping')
          and lower(a.runtime) in ('claude', 'codex')
          and r.started_at < strftime('%Y-%m-%dT%H:%M:%f+00:00','now', $1)
          and not exists (
              select 1 from supervisor_commands c
              where c.agent_id = r.agent_id and c.status = 'running'
          )
        "#,
    )
    .bind(format!("-{ORPHAN_RUN_GRACE_SECS} seconds"))
    .fetch_all(pool)
    .await
    .map_err(to_string)?;
    for row in rows {
        let run_id: Uuid = row.get("id");
        let agent_id: Uuid = row.get("agent_id");
        let runtime: String = row.get("runtime");
        let owned = if runtime == "claude" {
            claude::claude_runtime_owns_turn(claude_registry, agent_id).await
        } else {
            codex::codex_runtime_owns_turn(codex_registry, agent_id).await
        };
        if !owned {
            close_orphaned_agent_run(pool, agent_id, run_id).await?;
        }
    }
    Ok(())
}

async fn close_orphaned_agent_run(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
) -> CommandResult<()> {
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
        return Ok(());
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
    for (work_item_id, status) in &work_items {
        let _ = crate::mark_task_after_work_item_finished(
            pool,
            *work_item_id,
            agent_id,
            run_id,
            status,
        )
        .await;
        reconcile_work_item_change(pool, *work_item_id, "work_item_finished").await?;
    }
    record_agent_activity(
        pool,
        Some(agent_id),
        Some(run_id),
        "run_error",
        "Closed a run no process owned",
        format!("run={run_id}; its request was marked failed so the queue can move on"),
    )
    .await?;
    Ok(())
}

pub(crate) async fn recover_supervisor_commands_at_startup(pool: &SqlitePool) -> CommandResult<()> {
    sqlx::query(
        r#"
        update supervisor_commands
        set status = 'done',
            error = 'skipped stale command for terminal work item',
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where status in ('pending', 'running')
          and exists (
              select 1
              from agent_work_items w
              where w.id = supervisor_commands.work_item_id
          and w.status in ('cancelled', 'failed', 'done', 'silent', 'held')
          )
        "#,
    )
    .execute(pool)
    .await
    .map_err(to_string)?;

    sqlx::query(
        r#"
        update supervisor_commands
        set status = 'pending',
            error = 'requeued after supervisor restart',
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where status = 'running'
        "#,
    )
    .execute(pool)
    .await
    .map_err(to_string)?;

    Ok(())
}

pub(crate) async fn write_supervisor_heartbeat(pool: &SqlitePool) -> CommandResult<()> {
    sqlx::query(
        r#"
        insert into supervisor_state (id, pid, status, updated_at)
        values (1, $1, 'running', strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
        on conflict (id) do update set
            pid = excluded.pid,
            status = excluded.status,
            updated_at = excluded.updated_at
        "#,
    )
    .bind(std::process::id() as i32)
    .execute(pool)
    .await
    .map_err(to_string)?;

    Ok(())
}

pub(crate) async fn claim_next_supervisor_command(
    pool: &SqlitePool,
) -> CommandResult<Option<SupervisorCommand>> {
    let row = sqlx::query(
        r#"
        update supervisor_commands
        set status = 'running',
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = (
            select id
            from supervisor_commands
            where status = 'pending'
            order by created_at asc
            limit 1
        )
        returning id, command_type, agent_id, run_id, work_item_id
        "#,
    )
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;

    let Some(row) = row else {
        return Ok(None);
    };

    let command = SupervisorCommand {
        id: row.get("id"),
        command_type: row.get("command_type"),
        agent_id: row.get("agent_id"),
        run_id: row.get("run_id"),
        work_item_id: row.get("work_item_id"),
    };

    Ok(Some(command))
}

pub(crate) async fn finish_supervisor_command(
    pool: &SqlitePool,
    command_id: Uuid,
    error: Option<String>,
) -> CommandResult<()> {
    let (status, error) = match error {
        Some(error) => ("failed", error),
        None => ("done", String::new()),
    };

    sqlx::query(
        r#"
        update supervisor_commands
        set status = $2, error = $3, updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1
        "#,
    )
    .bind(command_id)
    .bind(status)
    .bind(error)
    .execute(pool)
    .await
    .map_err(to_string)?;

    Ok(())
}

pub(crate) async fn cleanup_supervisor_commands(pool: &SqlitePool) -> CommandResult<()> {
    sqlx::query(
        r#"
        delete from supervisor_commands
        where status in ('done', 'failed')
          and updated_at < strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-7 days')
        "#,
    )
    .execute(pool)
    .await
    .map_err(to_string)?;
    // Event receipts only need to cover a run's replay window for idempotent
    // ingestion; without a retention window the table grows forever.
    sqlx::query(
        r#"
        delete from agent_event_receipts
        where created_at < strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-30 days')
        "#,
    )
    .execute(pool)
    .await
    .map_err(to_string)?;
    Ok(())
}

pub(crate) async fn run_supervisor() -> CommandResult<()> {
    let database_url = db_url();
    let _supervisor_lock = acquire_supervisor_lock(&database_url)?;
    let pool = db_connect_with_url(&database_url, 5)
        .await
        .map_err(to_string)?;
    migrate(&pool).await.map_err(to_string)?;

    mark_orphaned_agent_runs(&pool).await?;
    recover_supervisor_commands_at_startup(&pool).await?;
    let codex_registry = WarmCodexRegistry::default();
    let claude_registry = WarmClaudeRegistry::default();
    let command_semaphore = Arc::new(Semaphore::new(SUPERVISOR_COMMAND_CONCURRENCY));
    let mut last_command_cleanup = Instant::now() - Duration::from_secs(3600);
    let mut last_maintenance = Instant::now();
    let mut error_backoff = SUPERVISOR_ERROR_BACKOFF_INITIAL;

    loop {
        match run_supervisor_iteration(
            &pool,
            &codex_registry,
            &claude_registry,
            &command_semaphore,
            &mut last_command_cleanup,
            &mut last_maintenance,
        )
        .await
        {
            Ok(processed_command) => {
                error_backoff = SUPERVISOR_ERROR_BACKOFF_INITIAL;
                if processed_command {
                    continue;
                }
                sleep(SUPERVISOR_IDLE_SLEEP).await;
            }
            Err(err) => {
                eprintln!(
                    "Lantor supervisor loop failed; retrying in {:?}: {err}",
                    error_backoff
                );
                sleep(error_backoff).await;
                error_backoff = error_backoff
                    .saturating_mul(2)
                    .min(SUPERVISOR_ERROR_BACKOFF_MAX);
            }
        }
    }
}

async fn run_supervisor_iteration(
    pool: &SqlitePool,
    codex_registry: &WarmCodexRegistry,
    claude_registry: &WarmClaudeRegistry,
    command_semaphore: &Arc<Semaphore>,
    last_command_cleanup: &mut Instant,
    last_maintenance: &mut Instant,
) -> CommandResult<bool> {
    write_supervisor_heartbeat(pool).await?;
    if last_command_cleanup.elapsed() >= Duration::from_secs(3600) {
        cleanup_supervisor_commands(pool).await?;
        *last_command_cleanup = Instant::now();
    }
    if last_maintenance.elapsed() >= SUPERVISOR_MAINTENANCE_INTERVAL {
        *last_maintenance = Instant::now();
        start_due_provider_probes(pool).await?;
        close_orphaned_agent_runs(pool, codex_registry, claude_registry).await?;
    }
    schedule_queued_work_items(pool, codex_registry, claude_registry).await?;
    let mut processed_command = false;
    loop {
        let Ok(permit) = command_semaphore.clone().try_acquire_owned() else {
            break;
        };
        let Some(command) = claim_next_supervisor_command(pool).await? else {
            drop(permit);
            break;
        };
        processed_command = true;
        let command_id = command.id;
        let pool = pool.clone();
        let codex_registry = codex_registry.clone();
        let claude_registry = claude_registry.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let result =
                process_supervisor_command(&pool, &codex_registry, &claude_registry, command).await;
            if let Err(err) = finish_supervisor_command(&pool, command_id, result.err()).await {
                eprintln!("failed to finish supervisor command {command_id}: {err}");
            }
        });
    }

    Ok(processed_command)
}

async fn should_schedule_queued_work_item(
    pool: &SqlitePool,
    registry: &WarmCodexRegistry,
    claude_registry: &WarmClaudeRegistry,
    agent_id: Uuid,
    channel_id: Option<Uuid>,
    thread_root_id: Option<Uuid>,
) -> CommandResult<bool> {
    let runtime = agent_runtime(pool, agent_id).await?;
    if runtime
        .as_deref()
        .is_some_and(|runtime| runtime.eq_ignore_ascii_case("claude"))
    {
        // A follow-up for the running turn's surface joins it; others wait.
        if let Some((active_channel_id, active_thread_root_id)) =
            claude::steerable_claude_turn_surface(claude_registry, agent_id).await
        {
            if same_codex_surface(
                pool,
                channel_id,
                thread_root_id,
                active_channel_id,
                active_thread_root_id,
            )
            .await?
            {
                return Ok(true);
            }
        }
        return Ok(!agent_has_active_or_pending_start(pool, agent_id).await?);
    }
    if !runtime
        .as_deref()
        .is_some_and(|runtime| runtime.eq_ignore_ascii_case("codex"))
    {
        return Ok(!agent_has_active_or_pending_start(pool, agent_id).await?);
    }

    let Some((active_channel_id, active_thread_root_id, schedule_state)) =
        codex::active_codex_turn_surface(registry, agent_id).await
    else {
        return Ok(true);
    };
    match schedule_state {
        CodexActiveTurnScheduleState::ReadyForSteer => {}
        CodexActiveTurnScheduleState::WaitingForTurnId => return Ok(false),
        CodexActiveTurnScheduleState::StuckBeforeTurnId => {
            codex::reap_stuck_codex_turn(pool, registry, agent_id, "scheduler").await?;
            return Ok(true);
        }
    }
    same_codex_surface(
        pool,
        channel_id,
        thread_root_id,
        active_channel_id,
        active_thread_root_id,
    )
    .await
}

async fn schedule_queued_work_items(
    pool: &SqlitePool,
    registry: &WarmCodexRegistry,
    claude_registry: &WarmClaudeRegistry,
) -> CommandResult<()> {
    let rows = sqlx::query(
        r#"
        select w.id, w.agent_id, w.channel_id, w.thread_root_id
        from agent_work_items w
        where w.status = 'queued'
          and not exists (
              select 1
              from supervisor_commands c
              where c.command_type = 'start_agent'
                and c.work_item_id = w.id
                and c.status in ('pending', 'running')
          )
          -- A long login outage must not starve other runtimes of the batch.
          and not exists (
              select 1
              from agents a
              join provider_auth_state p on p.runtime = lower(a.runtime)
              where a.id = w.agent_id and p.status = 'needs_login'
          )
        order by w.created_at asc
        limit 16
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    for row in rows {
        let work_item_id: Uuid = row.get("id");
        let agent_id: Uuid = row.get("agent_id");
        let channel_id: Option<Uuid> = row.get("channel_id");
        let thread_root_id: Option<Uuid> = row.get("thread_root_id");
        if !should_schedule_queued_work_item(
            pool,
            registry,
            claude_registry,
            agent_id,
            channel_id,
            thread_root_id,
        )
        .await?
        {
            continue;
        }
        if enqueue_agent_work_if_available(pool, agent_id, work_item_id).await? {
            record_agent_activity(
                pool,
                Some(agent_id),
                None,
                "dispatch",
                "Backlog agent request scheduled",
                work_item_id.to_string(),
            )
            .await?;
        }
    }

    Ok(())
}

async fn process_supervisor_command(
    pool: &SqlitePool,
    codex_registry: &WarmCodexRegistry,
    claude_registry: &WarmClaudeRegistry,
    command: SupervisorCommand,
) -> CommandResult<()> {
    match command.command_type.as_str() {
        "start_agent" => {
            let Some(agent_id) = command.agent_id else {
                return Err("start_agent command missing agent_id".to_owned());
            };
            supervisor_start_agent(
                pool,
                codex_registry,
                claude_registry,
                agent_id,
                command.work_item_id,
            )
            .await
        }
        "stop_run" => {
            let Some(run_id) = command.run_id else {
                return Err("stop_run command missing run_id".to_owned());
            };
            supervisor_stop_run(
                pool,
                codex_registry,
                claude_registry,
                run_id,
                command.work_item_id,
            )
            .await
        }
        other => Err(format!("unknown supervisor command: {other}")),
    }
}

async fn supervisor_start_agent(
    pool: &SqlitePool,
    codex_registry: &WarmCodexRegistry,
    claude_registry: &WarmClaudeRegistry,
    agent_id: Uuid,
    work_item_id: Option<Uuid>,
) -> CommandResult<()> {
    if let Some(work_item_id) = work_item_id {
        let status: Option<String> =
            sqlx::query_scalar("select status from agent_work_items where id = $1")
                .bind(work_item_id)
                .fetch_optional(pool)
                .await
                .map_err(to_string)?;
        if matches!(
            status.as_deref(),
            Some("cancelled" | "failed" | "done" | "silent" | "held")
        ) {
            record_agent_activity(
                pool,
                Some(agent_id),
                None,
                "dispatch",
                "Terminal agent request skipped",
                work_item_id.to_string(),
            )
            .await?;
            return Ok(());
        }
    }

    if let Some(reason) = agent_budget_exhausted(pool, agent_id).await? {
        if let Some(work_item_id) = work_item_id {
            let mut transaction = pool.begin().await.map_err(to_string)?;
            sqlx::query(
                r#"
                update agent_work_items
                set status = 'failed',
                    completed_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
                    updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
                where id = $1
                "#,
            )
            .bind(work_item_id)
            .execute(&mut *transaction)
            .await
            .map_err(to_string)?;
            enqueue_ui_work_item_changed_in_tx(&mut transaction, work_item_id, "budget_exhausted")
                .await?;
            transaction.commit().await.map_err(to_string)?;
            reconcile_work_item_change(pool, work_item_id, "budget_exhausted").await?;
        }
        record_agent_activity(
            pool,
            Some(agent_id),
            None,
            "usage",
            "Budget reached",
            reason,
        )
        .await?;
        return Ok(());
    }

    let row = sqlx::query(
        r#"
        select handle, runtime, model, reasoning_effort, service_tier, launch_command,
               environment_variables, working_directory, avatar
        from agents
        where id = $1
        "#,
    )
    .bind(agent_id)
    .fetch_one(pool)
    .await
    .map_err(to_string)?;

    let handle: String = row.get("handle");
    let runtime: String = row.get("runtime");
    let model: String = row.get("model");
    let reasoning_effort: String = row.get("reasoning_effort");
    let service_tier: String = row.get("service_tier");
    let launch_command: String = row.get("launch_command");
    let environment_variables: String = row.get("environment_variables");
    let working_directory: String = row.get::<String, _>("working_directory").trim().to_owned();
    let avatar: Option<String> = row.get("avatar");
    // Queued before the login broke: leave the request queued for the resume.
    if let Some(work_item_id) = work_item_id {
        if provider_needs_login(pool, Some(&runtime)).await? {
            sqlx::query("update agents set status = 'idle' where id = $1 and status = 'queued'")
                .bind(agent_id)
                .execute(pool)
                .await
                .map_err(to_string)?;
            record_agent_activity(
                pool,
                Some(agent_id),
                None,
                "dispatch",
                "Waiting for provider login",
                work_item_id.to_string(),
            )
            .await?;
            return Ok(());
        }
    }
    let is_warm_streaming_runtime =
        runtime.eq_ignore_ascii_case("codex") || runtime.eq_ignore_ascii_case("claude");
    let memory_context = match load_agent_memory_context(&working_directory) {
        Ok(memory_context) => memory_context,
        Err(err) => {
            record_agent_activity(
                pool,
                Some(agent_id),
                None,
                "profile",
                "Memory context skipped",
                err,
            )
            .await?;
            None
        }
    };
    let mut rendered_context_max_seq = 0;
    let work_item_prompt = match work_item_id {
        Some(work_item_id) => {
            let row = sqlx::query(
                r#"
                select
                    w.channel_id,
                    w.title,
                    w.context,
                    c.name as channel_name,
                    t.number as task_number,
                    w.thread_root_id
                from agent_work_items w
                left join channels c on c.id = w.channel_id
                left join tasks t on t.id = w.task_id
                where w.id = $1 and w.agent_id = $2
                "#,
            )
            .bind(work_item_id)
            .bind(agent_id)
            .fetch_one(pool)
            .await
            .map_err(to_string)?;
            let title: String = row.get("title");
            let context: String = row.get("context");
            let channel_id: Option<Uuid> = row.get("channel_id");
            let channel_name: Option<String> = row.get("channel_name");
            let task_number: Option<i64> = row.get("task_number");
            let thread_root_id: Option<Uuid> = row.get("thread_root_id");
            let context = if runtime.eq_ignore_ascii_case("claude") {
                let rendered = append_claude_thread_context_with_seq(
                    pool,
                    &context,
                    channel_id,
                    channel_name.as_deref(),
                    thread_root_id,
                )
                .await?;
                rendered_context_max_seq = rendered.max_seq;
                rendered.context
            } else {
                context
            };
            let available_agents = load_channel_agent_roster(pool, channel_id, agent_id).await?;
            let github_reference_lines =
                crate::github::load_agent_github_reference_lines(pool, channel_id).await?;
            let agent_profile_hint = avatar
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_none()
                .then(|| {
                    format!(
                        "Your agent profile currently has no avatar. If your handle or MEMORY.md gives you a stable identity, you may emit one standalone LANTOR_EVENT profile_update with an avatar like `dicebear:dylan:{handle}`. Keep handling the user's request normally and do not send visible chat only for avatar setup."
                    )
                });
            if is_warm_streaming_runtime {
                build_streaming_work_item_prompt(
                    work_item_id,
                    &title,
                    &context,
                    channel_name.as_deref(),
                    &github_reference_lines,
                    task_number,
                    thread_root_id,
                    &available_agents,
                    agent_profile_hint.as_deref(),
                )
            } else {
                build_work_item_prompt(
                    work_item_id,
                    &title,
                    &context,
                    channel_name.as_deref(),
                    &github_reference_lines,
                    task_number,
                    thread_root_id,
                    &available_agents,
                    agent_profile_hint.as_deref(),
                )
            }
        }
        None => String::new(),
    };
    if runtime.eq_ignore_ascii_case("codex") {
        return codex::supervisor_start_codex_streaming_agent(
            pool,
            codex_registry,
            agent_id,
            work_item_id,
            handle,
            model,
            reasoning_effort,
            service_tier,
            working_directory,
            environment_variables,
            work_item_prompt,
            memory_context,
        )
        .await;
    }
    if runtime.eq_ignore_ascii_case("claude") {
        return claude::supervisor_start_claude_streaming_agent(
            pool,
            claude_registry,
            agent_id,
            work_item_id,
            handle,
            model,
            reasoning_effort,
            working_directory,
            environment_variables,
            work_item_prompt,
            rendered_context_max_seq,
            memory_context,
        )
        .await;
    }
    let work_item_prompt = prepend_memory_context(work_item_prompt, memory_context.as_deref());
    let command_text = effective_launch_command(launch_command, runtime, model, handle.clone());
    start_process_agent(
        pool,
        ProcessAgentLaunch {
            agent_id,
            work_item_id,
            handle,
            working_directory,
            command_text,
            environment_variables,
            work_item_prompt,
        },
    )
    .await
}

/// Finalize a work item whose cancel request can no longer be completed by a
/// live stop path (run already gone or never got a pid). Without this the item
/// would stay in `cancelling` forever, blocking retry and task redispatch.
async fn finalize_unstoppable_work_item(
    pool: &SqlitePool,
    work_item_id: Option<Uuid>,
    run_id: Uuid,
    reason: &str,
) -> CommandResult<()> {
    let Some(work_item_id) = work_item_id else {
        return Ok(());
    };
    let row = sqlx::query("select agent_id, status from agent_work_items where id = $1")
        .bind(work_item_id)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?;
    let Some(row) = row else {
        return Ok(());
    };
    let status: String = row.get("status");
    if !matches!(status.as_str(), "queued" | "running" | "cancelling") {
        return Ok(());
    }
    let agent_id: Uuid = row.get("agent_id");
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
    transaction.commit().await.map_err(to_string)?;
    let _ = crate::mark_task_after_work_item_finished(
        pool,
        work_item_id,
        agent_id,
        run_id,
        "cancelled",
    )
    .await;
    reconcile_work_item_change(pool, work_item_id, "work_item_cancelled").await?;
    record_agent_activity(
        pool,
        Some(agent_id),
        Some(run_id),
        "dispatch",
        "Agent request cancelled",
        format!("{reason}: {work_item_id}"),
    )
    .await?;
    Ok(())
}

async fn supervisor_stop_run(
    pool: &SqlitePool,
    codex_registry: &WarmCodexRegistry,
    claude_registry: &WarmClaudeRegistry,
    run_id: Uuid,
    command_work_item_id: Option<Uuid>,
) -> CommandResult<()> {
    let row = sqlx::query(
        r#"
        select r.agent_id, r.pid, r.work_item_id, a.runtime
        from agent_runs r
        join agents a on a.id = r.agent_id
        where r.id = $1 and r.stopped_at is null
        "#,
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;

    let Some(row) = row else {
        // The run already finished or never existed; make sure the cancel
        // request does not leave its work item stuck in `cancelling`.
        finalize_unstoppable_work_item(pool, command_work_item_id, run_id, "run already stopped")
            .await?;
        return Ok(());
    };

    let agent_id: Uuid = row.get("agent_id");
    let mut pid: Option<i32> = row.get("pid");
    let work_item_id: Option<Uuid> = row
        .get::<Option<Uuid>, _>("work_item_id")
        .or(command_work_item_id);
    let runtime: String = row.get("runtime");
    if pid.is_none() {
        // A run briefly has no pid while its start path is publishing it.
        // Give that window a moment before treating the run as unstoppable.
        sleep(Duration::from_millis(300)).await;
        pid = sqlx::query_scalar::<_, Option<i32>>(
            "select pid from agent_runs where id = $1 and stopped_at is null",
        )
        .bind(run_id)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?
        .flatten();
    }
    let Some(pid) = pid else {
        // No process to signal: finalize run + work item instead of failing,
        // so cancel never leaves them stuck.
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
        transaction.commit().await.map_err(to_string)?;
        finalize_unstoppable_work_item(pool, work_item_id, run_id, "run had no process").await?;
        record_agent_activity(
            pool,
            Some(agent_id),
            Some(run_id),
            "run",
            "Stop finalized without process",
            "agent run never published a pid",
        )
        .await?;
        return Ok(());
    };

    let mut transaction = pool.begin().await.map_err(to_string)?;
    sqlx::query("update agent_runs set status = 'stopping' where id = $1")
        .bind(run_id)
        .execute(&mut *transaction)
        .await
        .map_err(to_string)?;
    sqlx::query("update agents set status = 'stopping' where id = $1")
        .bind(agent_id)
        .execute(&mut *transaction)
        .await
        .map_err(to_string)?;
    enqueue_ui_agent_run_changed_in_tx(&mut transaction, run_id, "run_stopping").await?;
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
    transaction.commit().await.map_err(to_string)?;
    if let Some(work_item_id) = work_item_id {
        reconcile_work_item_change(pool, work_item_id, "work_item_cancelling").await?;
    }

    if runtime.eq_ignore_ascii_case("codex")
        && codex::interrupt_warm_codex_run(pool, codex_registry, agent_id, run_id).await?
    {
        record_agent_activity(
            pool,
            Some(agent_id),
            Some(run_id),
            "run",
            "Stop requested",
            "Codex accepted stop request",
        )
        .await?;
        return Ok(());
    }

    if runtime.eq_ignore_ascii_case("claude")
        && claude::interrupt_warm_claude_run(pool, claude_registry, agent_id, run_id).await?
    {
        record_agent_activity(
            pool,
            Some(agent_id),
            Some(run_id),
            "run",
            "Stop requested",
            "Claude accepted interrupt request",
        )
        .await?;
        return Ok(());
    }

    terminate_process_group(pid).await?;

    append_run_log(
        pool,
        run_id,
        format!("sent SIGTERM to process group {pid}\n"),
    )
    .await?;
    record_agent_activity(
        pool,
        Some(agent_id),
        Some(run_id),
        "run",
        "Stop requested",
        format!("process_group={pid}"),
    )
    .await?;
    Ok(())
}

async fn load_channel_agent_roster(
    pool: &SqlitePool,
    channel_id: Option<Uuid>,
    current_agent_id: Uuid,
) -> CommandResult<Vec<String>> {
    let Some(channel_id) = channel_id else {
        return Ok(vec![]);
    };
    let rows = sqlx::query(
        r#"
        select a.handle
        from channels c
        join channel_members cm on cm.channel_id = c.id
        join agents a on a.id = cm.agent_id
        where c.id = $1
          and c.kind = 'channel'
          and a.id <> $2
        order by lower(a.handle)
        "#,
    )
    .bind(channel_id)
    .bind(current_agent_id)
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    Ok(rows
        .into_iter()
        .map(|row| {
            let handle: String = row.get("handle");
            format!("@{handle}")
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use std::fs as std_fs;

    use sqlx::SqlitePool;
    use uuid::Uuid;

    use super::{
        claim_next_supervisor_command, close_orphaned_agent_runs, load_channel_agent_roster,
        mark_orphaned_agent_runs, recover_supervisor_commands_at_startup,
    };
    use crate::db::{db_connect_with_url, migrate};
    use crate::runtime::{claude::WarmClaudeRegistry, codex::WarmCodexRegistry};

    #[tokio::test]
    async fn orphan_sweep_closes_live_runs_no_process_owns() {
        let Some((pool, database_path)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "orphaned").await?;
            sqlx::query("update agents set runtime = 'claude', status = 'running' where id = $1")
                .bind(agent_id)
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            let mut runs = Vec::new();
            for (started_at, work_status) in [
                ("2000-01-01T00:00:00.000+00:00", "running"),
                ("2000-01-01T00:00:01.000+00:00", "cancelling"),
                ("2999-01-01T00:00:00.000+00:00", "running"),
            ] {
                let run_id: Uuid = sqlx::query_scalar(
                    "insert into agent_runs (agent_id, command, status, started_at) values ($1, 'claude', 'running', $2) returning id",
                )
                .bind(agent_id)
                .bind(started_at)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
                let work_item_id: Uuid = sqlx::query_scalar(
                    "insert into agent_work_items (agent_id, title, status, run_id) values ($1, 'stuck', $2, $3) returning id",
                )
                .bind(agent_id)
                .bind(work_status)
                .bind(run_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
                runs.push((run_id, work_item_id));
            }

            close_orphaned_agent_runs(
                &pool,
                &WarmCodexRegistry::default(),
                &WarmClaudeRegistry::default(),
            )
            .await?;

            let mut states = Vec::new();
            for (run_id, work_item_id) in &runs {
                let run_status: String =
                    sqlx::query_scalar("select status from agent_runs where id = $1")
                        .bind(run_id)
                        .fetch_one(&pool)
                        .await
                        .map_err(|err| err.to_string())?;
                let work_status: String =
                    sqlx::query_scalar("select status from agent_work_items where id = $1")
                        .bind(work_item_id)
                        .fetch_one(&pool)
                        .await
                        .map_err(|err| err.to_string())?;
                states.push((run_status, work_status));
            }
            assert_eq!(
                states,
                [
                    ("unknown".to_owned(), "failed".to_owned()),
                    ("unknown".to_owned(), "cancelled".to_owned()),
                    // Young runs may still be starting their process.
                    ("running".to_owned(), "running".to_owned()),
                ]
            );
            let agent_status: String = sqlx::query_scalar("select status from agents where id = $1")
                .bind(agent_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
            assert_eq!(agent_status, "idle");
            Ok(())
        }
        .await;
        drop_test_schema(pool, database_path).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn startup_sweep_finalizes_work_items_stuck_in_cancelling() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "cancelling-agent").await?;
            let channel_id = insert_test_channel(&pool, "cancelling").await?;
            let cancelling_work_item_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_work_items (agent_id, channel_id, title, context, status)
                values ($1, $2, 'stuck cancel', 'context', 'cancelling')
                returning id
                "#,
            )
            .bind(agent_id)
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            mark_orphaned_agent_runs(&pool).await?;

            let (status, completed): (String, Option<String>) =
                sqlx::query_as("select status, completed_at from agent_work_items where id = $1")
                    .bind(cancelling_work_item_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(status, "cancelled");
            assert!(completed.is_some());
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn startup_recovery_requeues_running_supervisor_commands_and_skips_terminal_work() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "recovery-agent").await?;
            let channel_id = insert_test_channel(&pool, "recovery").await?;
            let queued_work_item_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_work_items (agent_id, channel_id, title, context, status)
                values ($1, $2, 'queued request', 'context', 'queued')
                returning id
                "#,
            )
            .bind(agent_id)
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let cancelled_work_item_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_work_items (agent_id, channel_id, title, context, status)
                values ($1, $2, 'cancelled request', 'context', 'cancelled')
                returning id
                "#,
            )
            .bind(agent_id)
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let running_command_id: Uuid = sqlx::query_scalar(
                r#"
                insert into supervisor_commands (command_type, agent_id, work_item_id, status)
                values ('start_agent', $1, $2, 'running')
                returning id
                "#,
            )
            .bind(agent_id)
            .bind(queued_work_item_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let terminal_command_id: Uuid = sqlx::query_scalar(
                r#"
                insert into supervisor_commands (command_type, agent_id, work_item_id, status)
                values ('start_agent', $1, $2, 'pending')
                returning id
                "#,
            )
            .bind(agent_id)
            .bind(cancelled_work_item_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            recover_supervisor_commands_at_startup(&pool).await?;

            let running_status: String =
                sqlx::query_scalar("select status from supervisor_commands where id = $1")
                    .bind(running_command_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            let terminal_status: String =
                sqlx::query_scalar("select status from supervisor_commands where id = $1")
                    .bind(terminal_command_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;

            assert_eq!(running_status, "pending");
            assert_eq!(terminal_status, "done");
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn supervisor_command_claim_is_single_consumer() {
        let Some((pool, schema)) = test_pool_with_connections(8).await else {
            return;
        };
        let result: Result<(), String> = async {
            let command_id: Uuid = sqlx::query_scalar(
                r#"
                insert into supervisor_commands (command_type)
                values ('start_agent')
                returning id
                "#,
            )
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let (r1, r2, r3, r4) = tokio::join!(
                claim_next_supervisor_command(&pool),
                claim_next_supervisor_command(&pool),
                claim_next_supervisor_command(&pool),
                claim_next_supervisor_command(&pool)
            );
            let mut claimed = Vec::new();
            for result in [r1, r2, r3, r4] {
                if let Some(command) = result? {
                    claimed.push(command.id);
                }
            }

            assert_eq!(claimed, vec![command_id]);
            let status: String =
                sqlx::query_scalar("select status from supervisor_commands where id = $1")
                    .bind(command_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(status, "running");
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn channel_agent_roster_excludes_current_agent() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let current_id = insert_test_agent(&pool, "current").await?;
            let peer_id = insert_test_agent(&pool, "peer").await?;
            let channel_id = insert_test_channel(&pool, "roster").await?;
            sqlx::query(
                r#"
                insert into channel_members (channel_id, agent_id)
                values ($1, $2), ($1, $3)
                "#,
            )
            .bind(channel_id)
            .bind(current_id)
            .bind(peer_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let roster = load_channel_agent_roster(&pool, Some(channel_id), current_id).await?;
            assert_eq!(roster, vec!["@peer"]);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    async fn test_pool() -> Option<(SqlitePool, String)> {
        test_pool_with_connections(1).await
    }

    async fn test_pool_with_connections(max_connections: u32) -> Option<(SqlitePool, String)> {
        let database_path =
            std::env::temp_dir().join(format!("lantor-test-{}.sqlite", Uuid::new_v4().simple()));
        let database_path = database_path.to_string_lossy().into_owned();
        let database_url = format!("sqlite://{database_path}");
        let pool = match db_connect_with_url(&database_url, max_connections).await {
            Ok(pool) => pool,
            Err(err) => {
                eprintln!("skipping SQLite-backed Lantor test: {err}");
                return None;
            }
        };
        if let Err(err) = migrate(&pool).await {
            eprintln!("skipping SQLite-backed Lantor test: {err}");
            pool.close().await;
            drop_sqlite_test_files(&database_path);
            return None;
        }
        Some((pool, database_path))
    }

    fn drop_sqlite_test_files(database_path: &str) {
        let _ = std_fs::remove_file(database_path);
        let _ = std_fs::remove_file(format!("{database_path}-wal"));
        let _ = std_fs::remove_file(format!("{database_path}-shm"));
    }

    async fn drop_test_schema(pool: SqlitePool, database_path: String) {
        pool.close().await;
        drop_sqlite_test_files(&database_path);
    }

    async fn insert_test_agent(pool: &SqlitePool, handle: &str) -> Result<Uuid, String> {
        sqlx::query_scalar(
            r#"
            insert into agents (handle, display_name, role, status, runtime, model, avatar, description)
            values ($1, $2, 'agent', 'idle', 'codex', 'gpt-5.5', 'D', 'test agent')
            returning id
            "#,
        )
        .bind(handle)
        .bind(handle)
        .fetch_one(pool)
        .await
        .map_err(|err| err.to_string())
    }

    async fn insert_test_channel(pool: &SqlitePool, name: &str) -> Result<Uuid, String> {
        sqlx::query_scalar(
            r#"
            insert into channels (name, description, kind)
            values ($1, 'test channel', 'channel')
            returning id
            "#,
        )
        .bind(name)
        .fetch_one(pool)
        .await
        .map_err(|err| err.to_string())
    }
}
