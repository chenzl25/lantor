use std::{
    future::Future,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use chrono::{DateTime, Utc};
use sqlx::{Row, SqlitePool};
use tokio::{sync::Semaphore, time::sleep};
use uuid::Uuid;

use super::{
    claude::{self, WarmClaudeRegistry},
    codex::{self, WarmCodexRegistry},
    lifecycle::{
        cancel_run_without_process, cancel_unfinished_work_item, close_orphaned_run,
        close_orphaned_runs_at_startup, fail_work_item_before_run,
        idle_agent_after_start_command_in_tx, idle_agent_waiting_for_login, mark_run_stopping,
    },
    network_retry::{system_in_dark_wake, with_network_retry_note},
    process::{
        effective_launch_command, start_process_agent, terminate_process_group, ProcessAgentLaunch,
    },
    self_update::SelfUpdate,
    streaming::settle_orphaned_streaming_messages,
    surface::{append_surface_context, same_codex_surface, CodexActiveTurnScheduleState},
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
    ui_notifications::{enqueue_ui_event, reconcile_work_item_change, UiEvent},
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
/// How long a supervisor about to replace itself waits for its stopped warm
/// processes to exit. Their SIGKILL escalation comes after 10 seconds.
const SELF_UPDATE_PROCESS_EXIT_WAIT: Duration = Duration::from_secs(15);

pub(crate) async fn load_supervisor_status(pool: &SqlitePool) -> CommandResult<SupervisorStatus> {
    let row = sqlx::query(
        r#"
        select pid, status, updated_at, started_at, build_modified_at,
               update_build_modified_at, update_waiting_since, update_error
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
            started_at: None,
            build_modified_at: None,
            update_build_modified_at: None,
            update_waiting_since: None,
            update_error: String::new(),
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
        started_at: row.get("started_at"),
        build_modified_at: row.get("build_modified_at"),
        update_build_modified_at: row.get("update_build_modified_at"),
        update_waiting_since: row.get("update_waiting_since"),
        update_error: row.get("update_error"),
    })
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
            claude_registry.owns_turn(agent_id).await
        } else {
            codex_registry.owns_turn(agent_id).await
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
    let Some(work_items) = close_orphaned_run(pool, agent_id, run_id).await? else {
        return Ok(());
    };
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

/// Closes everything a previous supervisor left live, then updates the tasks,
/// threads and activity log of what it closed, as the periodic sweep does.
async fn close_runs_left_by_restart(pool: &SqlitePool) -> CommandResult<()> {
    let sweep = close_orphaned_runs_at_startup(pool).await?;
    for work_item in &sweep.work_items {
        if let Some(run_id) = work_item.run_id {
            let _ = crate::mark_task_after_work_item_finished(
                pool,
                work_item.id,
                work_item.agent_id,
                run_id,
                &work_item.status,
            )
            .await;
        }
        reconcile_work_item_change(pool, work_item.id, "work_item_finished").await?;
    }
    for (run_id, agent_id) in &sweep.runs {
        record_agent_activity(
            pool,
            Some(*agent_id),
            Some(*run_id),
            "run_error",
            "Closed a run the supervisor restart interrupted",
            format!("run={run_id}; its request was closed so the queue can move on"),
        )
        .await?;
    }
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

fn timestamp(time: SystemTime) -> DateTime<Utc> {
    DateTime::<Utc>::from(time)
}

/// Records which build this supervisor runs, clearing what an earlier one
/// left about newer builds.
async fn record_supervisor_start(
    pool: &SqlitePool,
    self_update: Option<&SelfUpdate>,
) -> CommandResult<()> {
    write_supervisor_heartbeat(pool).await?;
    sqlx::query(
        r#"
        update supervisor_state
        set started_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
            build_modified_at = $1,
            update_build_modified_at = null,
            update_waiting_since = null,
            update_error = ''
        where id = 1
        "#,
    )
    .bind(self_update.map(|update| timestamp(update.running().modified())))
    .execute(pool)
    .await
    .map_err(to_string)?;
    enqueue_ui_event(
        pool,
        &UiEvent::Refresh {
            reason: "supervisor_update",
        },
    )
    .await
}

/// Records the newer build that waits to take over, or that none does.
async fn record_pending_update(pool: &SqlitePool, self_update: &SelfUpdate) -> CommandResult<()> {
    let pending = self_update.pending().map(|file| timestamp(file.modified()));
    sqlx::query(
        r#"
        update supervisor_state
        set update_build_modified_at = $1,
            update_waiting_since = case
                when $1 is null then null
                else strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
            end
        where id = 1
        "#,
    )
    .bind(pending)
    .execute(pool)
    .await
    .map_err(to_string)?;
    enqueue_ui_event(
        pool,
        &UiEvent::Refresh {
            reason: "supervisor_update",
        },
    )
    .await
}

async fn record_failed_update(pool: &SqlitePool, error: &str) -> CommandResult<()> {
    sqlx::query(
        r#"
        update supervisor_state
        set status = 'running',
            update_build_modified_at = null,
            update_waiting_since = null,
            update_error = $1
        where id = 1
        "#,
    )
    .bind(error)
    .execute(pool)
    .await
    .map_err(to_string)?;
    enqueue_ui_event(
        pool,
        &UiEvent::Refresh {
            reason: "supervisor_update",
        },
    )
    .await
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

    let mut transaction = pool.begin().await.map_err(to_string)?;
    let finished = sqlx::query(
        r#"
        update supervisor_commands
        set status = $2, error = $3, updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1
        returning command_type, agent_id
        "#,
    )
    .bind(command_id)
    .bind(status)
    .bind(error)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(to_string)?;
    // A start can end without a run (its request already finished, the budget
    // is spent, the runtime would not start); its agent must not stay queued.
    if let Some(row) = finished {
        let command_type: String = row.get("command_type");
        let agent_id: Option<Uuid> = row.get("agent_id");
        if let (Some(agent_id), "start_agent") = (agent_id, command_type.as_str()) {
            idle_agent_after_start_command_in_tx(&mut transaction, agent_id).await?;
        }
    }
    transaction.commit().await.map_err(to_string)
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

    close_runs_left_by_restart(&pool).await?;
    settle_orphaned_streaming_messages(&pool).await?;
    recover_supervisor_commands_at_startup(&pool).await?;
    let mut self_update = SelfUpdate::for_current_executable();
    record_supervisor_start(&pool, self_update.as_ref()).await?;
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
            self_update.as_mut(),
            &mut last_command_cleanup,
            &mut last_maintenance,
        )
        .await
        {
            Ok(SupervisorIteration::ClaimedCommands) => {
                error_backoff = SUPERVISOR_ERROR_BACKOFF_INITIAL;
            }
            Ok(SupervisorIteration::Waiting) => {
                error_backoff = SUPERVISOR_ERROR_BACKOFF_INITIAL;
                sleep(SUPERVISOR_IDLE_SLEEP).await;
            }
            Ok(SupervisorIteration::NewerBuildCanTakeOver) => {
                error_backoff = SUPERVISOR_ERROR_BACKOFF_INITIAL;
                if let Some(self_update) = self_update.as_mut() {
                    if let Err(err) = hand_over_to_newer_build(
                        &pool,
                        &codex_registry,
                        &claude_registry,
                        &command_semaphore,
                        self_update,
                    )
                    .await
                    {
                        eprintln!("Lantor supervisor could not hand over to a newer build: {err}");
                    }
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

enum SupervisorIteration {
    /// Commands were claimed; look for more right away.
    ClaimedCommands,
    /// Nothing more to start until the next pass.
    Waiting,
    /// Nothing is working and a newer build waits to take over. No work was
    /// scheduled in this pass.
    NewerBuildCanTakeOver,
}

async fn run_supervisor_iteration(
    pool: &SqlitePool,
    codex_registry: &WarmCodexRegistry,
    claude_registry: &WarmClaudeRegistry,
    command_semaphore: &Arc<Semaphore>,
    mut self_update: Option<&mut SelfUpdate>,
    last_command_cleanup: &mut Instant,
    last_maintenance: &mut Instant,
) -> CommandResult<SupervisorIteration> {
    write_supervisor_heartbeat(pool).await?;
    if last_command_cleanup.elapsed() >= Duration::from_secs(3600) {
        cleanup_supervisor_commands(pool).await?;
        *last_command_cleanup = Instant::now();
    }
    if last_maintenance.elapsed() >= SUPERVISOR_MAINTENANCE_INTERVAL {
        *last_maintenance = Instant::now();
        start_due_provider_probes(pool).await?;
        close_orphaned_agent_runs(pool, codex_registry, claude_registry).await?;
        settle_orphaned_streaming_messages(pool).await?;
        if let Some(self_update) = self_update.as_deref_mut() {
            if self_update.check() {
                record_pending_update(pool, self_update).await?;
            }
        }
    }
    // Checked before scheduling, so that nothing starts between the check and
    // the handover: only this loop starts work.
    if self_update
        .as_deref()
        .is_some_and(|self_update| self_update.pending().is_some())
        && supervisor_is_idle(pool, codex_registry, claude_registry, command_semaphore).await?
    {
        return Ok(SupervisorIteration::NewerBuildCanTakeOver);
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
        let pool = pool.clone();
        let codex_registry = codex_registry.clone();
        let claude_registry = claude_registry.clone();
        tokio::spawn(async move {
            let _permit = permit;
            run_supervisor_command(&pool, &codex_registry, &claude_registry, command).await;
        });
    }

    Ok(if processed_command {
        SupervisorIteration::ClaimedCommands
    } else {
        SupervisorIteration::Waiting
    })
}

/// Whether nothing is working: no run or request in progress, no command
/// being carried out, and no warm process in a turn. Queued requests do not
/// count; the next supervisor starts them.
async fn supervisor_is_idle(
    pool: &SqlitePool,
    codex_registry: &WarmCodexRegistry,
    claude_registry: &WarmClaudeRegistry,
    command_semaphore: &Semaphore,
) -> CommandResult<bool> {
    if command_semaphore.available_permits() < SUPERVISOR_COMMAND_CONCURRENCY
        || codex_registry.any_busy().await
        || claude_registry.any_busy().await
    {
        return Ok(false);
    }
    let working: bool = sqlx::query_scalar(
        r#"
        select exists (
                   select 1 from agent_work_items where status in ('running', 'cancelling')
               )
            or exists (
                   select 1 from supervisor_commands where status = 'running'
               )
            or exists (
                   select 1 from agent_runs
                   where stopped_at is null and status in ('starting', 'running', 'stopping')
               )
        "#,
    )
    .fetch_one(pool)
    .await
    .map_err(to_string)?;
    Ok(!working)
}

/// Replaces this process with the newer build of its executable, which then
/// starts as a new supervisor with the same process id. Warm processes are
/// stopped first, as the idle reaper would; their next turn resumes the same
/// session in a new process. Returns only when the handover did not happen:
/// something started working, the file changed again, or the exec failed.
async fn hand_over_to_newer_build(
    pool: &SqlitePool,
    codex_registry: &WarmCodexRegistry,
    claude_registry: &WarmClaudeRegistry,
    command_semaphore: &Semaphore,
    self_update: &mut SelfUpdate,
) -> CommandResult<()> {
    if !self_update.pending_is_current() {
        if self_update.check() {
            record_pending_update(pool, self_update).await?;
        }
        return Ok(());
    }
    sqlx::query("update supervisor_state set status = 'restarting' where id = 1")
        .execute(pool)
        .await
        .map_err(to_string)?;
    codex_registry
        .stop_idle_processes(pool, "stopped for a newer Lantor build")
        .await;
    claude_registry
        .stop_idle_processes(pool, "stopped for a newer Lantor build")
        .await;
    let deadline = Instant::now() + SELF_UPDATE_PROCESS_EXIT_WAIT;
    while !(codex_registry.is_empty().await && claude_registry.is_empty().await)
        && Instant::now() < deadline
    {
        sleep(Duration::from_millis(100)).await;
    }
    // The next heartbeat sets the status back if this stops here.
    if !supervisor_is_idle(pool, codex_registry, claude_registry, command_semaphore).await? {
        return Ok(());
    }
    let error = self_update.exec_pending();
    record_failed_update(pool, &error.to_string()).await
}

/// Runs a claimed command and records how it ended.
async fn run_supervisor_command(
    pool: &SqlitePool,
    codex_registry: &WarmCodexRegistry,
    claude_registry: &WarmClaudeRegistry,
    command: SupervisorCommand,
) {
    let command_id = command.id;
    let result = process_supervisor_command(pool, codex_registry, claude_registry, command).await;
    if let Err(err) = finish_supervisor_command(pool, command_id, result.err()).await {
        eprintln!("failed to finish supervisor command {command_id}: {err}");
    }
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
    schedule_queued_work_items_when(pool, registry, claude_registry, system_in_dark_wake).await
}

async fn schedule_queued_work_items_when<F, Fut>(
    pool: &SqlitePool,
    registry: &WarmCodexRegistry,
    claude_registry: &WarmClaudeRegistry,
    in_dark_wake: F,
) -> CommandResult<()>
where
    F: Fn() -> Fut,
    Fut: Future<Output = bool>,
{
    let rows = sqlx::query(
        r#"
        select w.id, w.agent_id, w.channel_id, w.thread_root_id, w.network_requeued_at
        from agent_work_items w
        where w.status = 'queued'
          and (
              w.retry_not_before is null
              or w.retry_not_before <= strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
          )
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

    let mut dark_wake = None;
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
        // A retry in a dark wake would only be cut off again by the next sleep.
        if row
            .get::<Option<String>, _>("network_requeued_at")
            .is_some()
        {
            let dark = match dark_wake {
                Some(dark) => dark,
                None => *dark_wake.insert(in_dark_wake().await),
            };
            if dark {
                continue;
            }
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
            fail_work_item_before_run(pool, work_item_id, "budget_exhausted").await?;
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
            idle_agent_waiting_for_login(pool, agent_id).await?;
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
                    w.network_requeued_at,
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
            let rendered = append_surface_context(
                pool,
                &context,
                channel_id,
                channel_name.as_deref(),
                thread_root_id,
            )
            .await?;
            rendered_context_max_seq = rendered.max_seq;
            let context = rendered.context;
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
            let prompt = if is_warm_streaming_runtime {
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
            };
            if row
                .get::<Option<String>, _>("network_requeued_at")
                .is_some()
            {
                with_network_retry_note(prompt)
            } else {
                prompt
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
            rendered_context_max_seq,
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
            rendered_context_max_seq,
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
    cancel_unfinished_work_item(pool, work_item_id).await?;
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
        cancel_run_without_process(pool, agent_id, run_id).await?;
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

    mark_run_stopping(pool, agent_id, run_id, work_item_id).await?;
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
#[path = "../tests/supervisor_self_update.rs"]
mod self_update_tests;

#[cfg(test)]
mod tests {
    use std::fs as std_fs;

    use sqlx::SqlitePool;
    use uuid::Uuid;

    use super::{
        claim_next_supervisor_command, close_orphaned_agent_runs, load_channel_agent_roster,
        recover_supervisor_commands_at_startup, run_supervisor_command, supervisor_stop_run,
    };
    use crate::db::{db_connect_with_url, migrate};
    use crate::runtime::lifecycle::close_orphaned_runs_at_startup;
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

            close_orphaned_runs_at_startup(&pool).await?;

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
    async fn startup_sweep_reports_what_it_closed() {
        use crate::test_support::run_and_work_item_events;
        let Some((pool, database_path)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "restarted").await?;
            let channel_id = insert_test_channel(&pool, "restarted").await?;
            sqlx::query("update agents set runtime = 'claude', status = 'running' where id = $1")
                .bind(agent_id)
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            let run_id: Uuid = sqlx::query_scalar(
                "insert into agent_runs (agent_id, command, status) values ($1, 'claude', 'running') returning id",
            )
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let insert_work_item = |status: &'static str, run_id: Option<Uuid>| {
                let pool = pool.clone();
                async move {
                    sqlx::query_scalar::<_, Uuid>(
                        r#"
                        insert into agent_work_items (agent_id, channel_id, title, context, status, run_id)
                        values ($1, $2, 'left behind', 'context', $3, $4)
                        returning id
                        "#,
                    )
                    .bind(agent_id)
                    .bind(channel_id)
                    .bind(status)
                    .bind(run_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())
                }
            };
            let interrupted = insert_work_item("running", Some(run_id)).await?;
            insert_work_item("cancelling", None).await?;
            let inbox_item_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_inbox_items (
                    agent_id, channel_id, kind, state, title, body_preview, work_item_id
                )
                values ($1, $2, 'dm', 'processing', 'hello', 'hi', $3)
                returning id
                "#,
            )
            .bind(agent_id)
            .bind(channel_id)
            .bind(interrupted)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let cursor: i64 = sqlx::query_scalar("select coalesce(max(id), 0) from ui_events")
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;

            super::close_runs_left_by_restart(&pool).await?;

            // The UI hears about every run, request and agent it closed, as
            // with the periodic sweep.
            let mut events = run_and_work_item_events(&pool, cursor).await?;
            events.sort();
            assert_eq!(
                events,
                [
                    "agent_run_upsert:orphaned_run_closed:unknown",
                    "work_item_upsert:work_item_finished:cancelled",
                    "work_item_upsert:work_item_finished:failed",
                ]
            );
            let agent_refreshes: i64 = sqlx::query_scalar(
                r#"
                select count(*) from ui_events
                where id > $1
                  and json_extract(event_json, '$.type') = 'refresh'
                  and json_extract(event_json, '$.reason') = 'agent_updated'
                "#,
            )
            .bind(cursor)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(agent_refreshes, 1);
            // The interrupted request's inbox item is archived, and the
            // activity log says why its run ended.
            let inbox_state: String =
                sqlx::query_scalar("select state from agent_inbox_items where id = $1")
                    .bind(inbox_item_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(inbox_state, "archived");
            let run_errors: i64 = sqlx::query_scalar(
                "select count(*) from agent_activities where run_id = $1 and kind = 'run_error'",
            )
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(run_errors, 1);
            Ok(())
        }
        .await;
        drop_test_schema(pool, database_path).await;
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

    /// Inserts a live run of `agent_id` and its running work item.
    async fn insert_live_run(
        pool: &SqlitePool,
        agent_id: Uuid,
        pid: Option<i32>,
    ) -> Result<(Uuid, Uuid), String> {
        let work_item_id: Uuid = sqlx::query_scalar(
            "insert into agent_work_items (agent_id, title, status) values ($1, 'stop me', 'running') returning id",
        )
        .bind(agent_id)
        .fetch_one(pool)
        .await
        .map_err(|err| err.to_string())?;
        let run_id: Uuid = sqlx::query_scalar(
            "insert into agent_runs (agent_id, work_item_id, command, status, pid) values ($1, $2, 'sleep', 'running', $3) returning id",
        )
        .bind(agent_id)
        .bind(work_item_id)
        .bind(pid)
        .fetch_one(pool)
        .await
        .map_err(|err| err.to_string())?;
        sqlx::query("update agent_work_items set run_id = $2 where id = $1")
            .bind(work_item_id)
            .bind(run_id)
            .execute(pool)
            .await
            .map_err(|err| err.to_string())?;
        sqlx::query("update agents set runtime = 'custom', status = 'running' where id = $1")
            .bind(agent_id)
            .execute(pool)
            .await
            .map_err(|err| err.to_string())?;
        Ok((run_id, work_item_id))
    }

    /// Run status, agent status, work item status, and whether the run and
    /// the work item recorded when they stopped.
    async fn stop_state(
        pool: &SqlitePool,
        agent_id: Uuid,
        run_id: Uuid,
        work_item_id: Uuid,
    ) -> Result<(String, String, String, bool, bool), String> {
        let (run_status, run_stopped): (String, bool) =
            sqlx::query_as("select status, stopped_at is not null from agent_runs where id = $1")
                .bind(run_id)
                .fetch_one(pool)
                .await
                .map_err(|err| err.to_string())?;
        let agent_status: String = sqlx::query_scalar("select status from agents where id = $1")
            .bind(agent_id)
            .fetch_one(pool)
            .await
            .map_err(|err| err.to_string())?;
        let (work_status, work_completed): (String, bool) = sqlx::query_as(
            "select status, completed_at is not null from agent_work_items where id = $1",
        )
        .bind(work_item_id)
        .fetch_one(pool)
        .await
        .map_err(|err| err.to_string())?;
        Ok((
            run_status,
            agent_status,
            work_status,
            run_stopped,
            work_completed,
        ))
    }

    #[tokio::test]
    async fn stopping_a_run_without_a_process_cancels_it_and_its_request() {
        use crate::test_support::{run_and_work_item_events, ui_event_cursor};
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "stop-no-pid").await?;
            let (run_id, work_item_id) = insert_live_run(&pool, agent_id, None).await?;
            let cursor = ui_event_cursor(&pool).await?;

            supervisor_stop_run(
                &pool,
                &WarmCodexRegistry::default(),
                &WarmClaudeRegistry::default(),
                run_id,
                None,
            )
            .await?;

            assert_eq!(
                stop_state(&pool, agent_id, run_id, work_item_id).await?,
                (
                    "cancelled".to_owned(),
                    "idle".to_owned(),
                    "cancelled".to_owned(),
                    true,
                    true
                )
            );
            assert_eq!(
                run_and_work_item_events(&pool, cursor).await?,
                [
                    "agent_run_upsert:run_finished:cancelled",
                    "work_item_upsert:work_item_cancelled:cancelled",
                ]
            );
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn stopping_a_finished_run_cancels_the_request_left_cancelling() {
        use crate::test_support::{run_and_work_item_events, ui_event_cursor};
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "stop-finished").await?;
            let (run_id, work_item_id) = insert_live_run(&pool, agent_id, None).await?;
            sqlx::query(
                "update agent_runs set status = 'exited', stopped_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') where id = $1",
            )
            .bind(run_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
            sqlx::query("update agent_work_items set status = 'cancelling' where id = $1")
                .bind(work_item_id)
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            let cursor = ui_event_cursor(&pool).await?;

            supervisor_stop_run(
                &pool,
                &WarmCodexRegistry::default(),
                &WarmClaudeRegistry::default(),
                run_id,
                Some(work_item_id),
            )
            .await?;

            assert_eq!(
                stop_state(&pool, agent_id, run_id, work_item_id).await?,
                (
                    "exited".to_owned(),
                    "running".to_owned(),
                    "cancelled".to_owned(),
                    true,
                    true
                )
            );
            assert_eq!(
                run_and_work_item_events(&pool, cursor).await?,
                ["work_item_upsert:work_item_cancelled:cancelled"]
            );
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stopping_a_run_with_a_process_marks_it_stopping_and_signals_it() {
        use crate::test_support::{run_and_work_item_events, ui_event_cursor};
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let mut child = tokio::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .expect("spawn sleep");
        let pid = child.id().map(|pid| pid as i32);
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "stop-with-pid").await?;
            let (run_id, work_item_id) = insert_live_run(&pool, agent_id, pid).await?;
            let cursor = ui_event_cursor(&pool).await?;

            supervisor_stop_run(
                &pool,
                &WarmCodexRegistry::default(),
                &WarmClaudeRegistry::default(),
                run_id,
                Some(work_item_id),
            )
            .await?;

            // The run's own exit path finishes it once the process is gone.
            assert_eq!(
                stop_state(&pool, agent_id, run_id, work_item_id).await?,
                (
                    "stopping".to_owned(),
                    "stopping".to_owned(),
                    "cancelling".to_owned(),
                    false,
                    false
                )
            );
            assert_eq!(
                run_and_work_item_events(&pool, cursor).await?,
                [
                    "agent_run_upsert:run_stopping:stopping",
                    "work_item_upsert:work_item_cancelling:cancelling",
                ]
            );
            Ok(())
        }
        .await;
        let exit = tokio::time::timeout(std::time::Duration::from_secs(10), child.wait()).await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
        assert!(matches!(exit, Ok(Ok(status)) if !status.success()));
    }

    #[tokio::test]
    async fn a_warm_runtime_that_cannot_spawn_flags_its_agent_in_the_ui() {
        let Some((pool, database_path)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            // No process can start in a directory that does not exist.
            let missing_directory = std::env::temp_dir()
                .join(format!("lantor-missing-{}", Uuid::new_v4().simple()))
                .to_string_lossy()
                .into_owned();
            for runtime in ["claude", "codex"] {
                let agent_id = insert_test_agent(&pool, &format!("{runtime}-spawn")).await?;
                sqlx::query("update agents set runtime = $2 where id = $1")
                    .bind(agent_id)
                    .bind(runtime)
                    .execute(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
                let cursor: i64 = sqlx::query_scalar("select coalesce(max(id), 0) from ui_events")
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
                let started = if runtime == "claude" {
                    crate::runtime::claude::supervisor_start_claude_streaming_agent(
                        &pool,
                        &WarmClaudeRegistry::default(),
                        agent_id,
                        None,
                        runtime.to_owned(),
                        String::new(),
                        String::new(),
                        missing_directory.clone(),
                        String::new(),
                        String::new(),
                        0,
                        None,
                    )
                    .await
                } else {
                    crate::runtime::codex::supervisor_start_codex_streaming_agent(
                        &pool,
                        &WarmCodexRegistry::default(),
                        agent_id,
                        None,
                        runtime.to_owned(),
                        String::new(),
                        String::new(),
                        String::new(),
                        missing_directory.clone(),
                        String::new(),
                        String::new(),
                        0,
                        None,
                    )
                    .await
                };
                assert!(started.is_err(), "{runtime} started in a missing directory");

                let status: String = sqlx::query_scalar("select status from agents where id = $1")
                    .bind(agent_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
                assert_eq!(status, "error", "{runtime}");
                let refreshes: Vec<String> = sqlx::query_scalar(
                    r#"
                    select json_extract(event_json, '$.reason')
                    from ui_events
                    where id > $1 and json_extract(event_json, '$.type') = 'refresh'
                    "#,
                )
                .bind(cursor)
                .fetch_all(&pool)
                .await
                .map_err(|err| err.to_string())?;
                assert_eq!(refreshes, ["agent_updated"], "{runtime}");
            }
            Ok(())
        }
        .await;
        drop_test_schema(pool, database_path).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn a_start_that_ends_without_a_run_returns_its_queued_agent_to_idle() {
        let Some((pool, database_path)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            // A request cancelled after its start was queued is skipped.
            let skipped = insert_test_agent(&pool, "skipped").await?;
            queue_test_start(&pool, skipped, "cancelled").await?;
            // A request whose agent spent its daily budget fails before a run.
            let spent = insert_test_agent(&pool, "spent").await?;
            sqlx::query("update agents set daily_budget_micros = 1 where id = $1")
                .bind(spent)
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            sqlx::query(
                r#"
                insert into agent_runs (agent_id, command, status, cost_micros, stopped_at)
                values ($1, 'codex', 'exited', 1, strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
                "#,
            )
            .bind(spent)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let over_budget = queue_test_start(&pool, spent, "queued").await?;
            let cursor = latest_ui_event(&pool).await?;

            run_pending_commands(&pool).await?;

            for agent_id in [skipped, spent] {
                assert_eq!(agent_status(&pool, agent_id).await?, "idle");
            }
            let work_status: String =
                sqlx::query_scalar("select status from agent_work_items where id = $1")
                    .bind(over_budget)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(work_status, "failed");
            assert_eq!(
                refresh_reasons_since(&pool, cursor).await?,
                ["agent_updated", "agent_updated"]
            );
            Ok(())
        }
        .await;
        drop_test_schema(pool, database_path).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn network_retries_wait_out_their_delay_and_dark_wakes() {
        let Some((pool, database_path)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let channel_id = insert_test_channel(&pool, "network-retry-schedule").await?;
            let mut items = Vec::new();
            for (handle, requeued_at, retry_not_before) in [
                ("fresh", None, None),
                ("due", Some("-5 minutes"), Some("-1 seconds")),
                ("backing-off", Some("-5 minutes"), Some("+1 hours")),
            ] {
                let agent_id = insert_test_agent(&pool, handle).await?;
                sqlx::query("update agents set runtime = 'claude' where id = $1")
                    .bind(agent_id)
                    .execute(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
                let work_item_id: Uuid = sqlx::query_scalar(
                    r#"
                    insert into agent_work_items (
                        agent_id, channel_id, title, status, network_requeued_at, retry_not_before
                    )
                    values (
                        $1, $2, 'request', 'queued',
                        strftime('%Y-%m-%dT%H:%M:%f+00:00','now', $3),
                        strftime('%Y-%m-%dT%H:%M:%f+00:00','now', $4)
                    )
                    returning id
                    "#,
                )
                .bind(agent_id)
                .bind(channel_id)
                .bind(requeued_at)
                .bind(retry_not_before)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
                items.push(work_item_id);
            }
            let (fresh, due, backing_off) = (items[0], items[1], items[2]);
            let started = |work_item_id: Uuid| {
                let pool = pool.clone();
                async move {
                    sqlx::query_scalar::<_, i64>(
                        "select count(*) from supervisor_commands where work_item_id = $1",
                    )
                    .bind(work_item_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())
                }
            };
            let codex_registry = WarmCodexRegistry::default();
            let claude_registry = WarmClaudeRegistry::default();

            // A dark wake: new requests may still try, retries wait.
            super::schedule_queued_work_items_when(
                &pool,
                &codex_registry,
                &claude_registry,
                || async { true },
            )
            .await?;
            assert_eq!(started(fresh).await?, 1);
            assert_eq!(started(due).await?, 0);
            assert_eq!(started(backing_off).await?, 0);

            // The Mac is really awake: the retry that is due starts.
            super::schedule_queued_work_items_when(
                &pool,
                &codex_registry,
                &claude_registry,
                || async { false },
            )
            .await?;
            assert_eq!(started(due).await?, 1);
            assert_eq!(started(backing_off).await?, 0);
            Ok(())
        }
        .await;
        drop_test_schema(pool, database_path).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    /// The whole path with a stand-in `claude`: the first process loses its
    /// connection, the second resumes its session and finishes the request.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_lost_connection_continues_in_the_same_session_after_its_delay() {
        use std::os::unix::fs::PermissionsExt;

        let Some((pool, database_path)) = test_pool().await else {
            return;
        };
        let directory =
            std::env::temp_dir().join(format!("lantor-network-retry-{}", Uuid::new_v4()));
        let result: Result<(), String> = async {
            let bin = directory.join("bin");
            std_fs::create_dir_all(&bin).map_err(|err| err.to_string())?;
            let script = r#"#!/bin/sh
dir="$(dirname "$0")/.."
count=$(cat "$dir/count" 2>/dev/null || echo 0)
count=$((count + 1))
echo "$count" > "$dir/count"
printf '%s\n' "$*" > "$dir/args-$count"
printf '%s\n' '{"type":"system","subtype":"init","session_id":"11111111-1111-4111-8111-111111111111"}'
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$dir/input-$count"
  if [ "$count" = 1 ]; then
    printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Half an answer\n"}},"session_id":"11111111-1111-4111-8111-111111111111"}'
    printf '%s\n' '{"type":"result","subtype":"success","is_error":true,"api_error_status":null,"terminal_reason":"api_error","result":"API Error: Connection dropped (ECONNRESET)","session_id":"11111111-1111-4111-8111-111111111111"}'
  else
    printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"The whole answer"}},"session_id":"11111111-1111-4111-8111-111111111111"}'
    printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"The whole answer","session_id":"11111111-1111-4111-8111-111111111111"}'
  fi
done
"#;
            let claude = bin.join("claude");
            std_fs::write(&claude, script).map_err(|err| err.to_string())?;
            std_fs::set_permissions(&claude, std_fs::Permissions::from_mode(0o755))
                .map_err(|err| err.to_string())?;

            let agent_id = insert_test_agent(&pool, "network-retry-end-to-end").await?;
            sqlx::query(
                r#"
                update agents
                set runtime = 'claude', model = 'sonnet', working_directory = $2,
                    environment_variables = $3
                where id = $1
                "#,
            )
            .bind(agent_id)
            .bind(directory.to_string_lossy().into_owned())
            .bind(format!("PATH={}:/usr/bin:/bin", bin.display()))
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let channel_id = insert_test_channel(&pool, "network-retry-end-to-end").await?;
            let work_item_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_work_items (agent_id, channel_id, title, context, status)
                values ($1, $2, 'look at the logs', 'context', 'queued')
                returning id
                "#,
            )
            .bind(agent_id)
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let codex_registry = WarmCodexRegistry::default();
            let claude_registry = WarmClaudeRegistry::default();
            let wait_for_status = |wanted: &'static str| {
                let pool = pool.clone();
                async move {
                    for _ in 0..200 {
                        let status: String =
                            sqlx::query_scalar("select status from agent_work_items where id = $1")
                                .bind(work_item_id)
                                .fetch_one(&pool)
                                .await
                                .map_err(|err| err.to_string())?;
                        if status == wanted {
                            return Ok(());
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    }
                    Err(format!("the request never became {wanted}"))
                }
            };

            super::supervisor_start_agent(
                &pool,
                &codex_registry,
                &claude_registry,
                agent_id,
                Some(work_item_id),
            )
            .await?;
            wait_for_status("queued").await?;
            let replies = |pool: SqlitePool| async move {
                sqlx::query_as::<_, (String, String)>(
                    "select body, delivery_state from messages where sender_agent_id = $1",
                )
                .bind(agent_id)
                .fetch_all(&pool)
                .await
                .map_err(|err| err.to_string())
            };
            assert_eq!(replies(pool.clone()).await?, []);
            for _ in 0..200 {
                if claude_registry.get(agent_id).await.is_none() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }

            // Still inside its delay: nothing starts.
            super::schedule_queued_work_items_when(&pool, &codex_registry, &claude_registry, || async {
                false
            })
            .await?;
            let delayed = claim_next_supervisor_command(&pool).await?;
            assert!(delayed.is_none(), "the retry must wait for its delay");

            sqlx::query(
                r#"
                update agent_work_items
                set retry_not_before = strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-1 seconds')
                where id = $1
                "#,
            )
            .bind(work_item_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
            super::schedule_queued_work_items_when(&pool, &codex_registry, &claude_registry, || async {
                false
            })
            .await?;
            let command = claim_next_supervisor_command(&pool)
                .await?
                .ok_or("the retry must start once its delay passed")?;
            run_supervisor_command(&pool, &codex_registry, &claude_registry, command).await;
            wait_for_status("done").await?;

            assert_eq!(
                replies(pool.clone()).await?,
                [("The whole answer".to_owned(), "complete".to_owned())]
            );
            let read = |name: &str| {
                std_fs::read_to_string(directory.join(name)).map_err(|err| format!("{name}: {err}"))
            };
            assert!(!read("args-1")?.contains("--resume"));
            assert!(read("args-2")?.contains("--resume 11111111-1111-4111-8111-111111111111"));
            const NOTE: &str = "the connection to the model dropped";
            assert!(!read("input-1")?.contains(NOTE));
            assert!(read("input-2")?.contains(NOTE));
            Ok(())
        }
        .await;
        let _ = std_fs::remove_dir_all(&directory);
        drop_test_schema(pool, database_path).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn a_start_that_ends_without_a_run_keeps_an_agent_another_start_or_run_needs() {
        let Some((pool, database_path)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let codex_registry = WarmCodexRegistry::default();
            let claude_registry = WarmClaudeRegistry::default();
            // Two starts are queued; the first ending leaves the second.
            let twice = insert_test_agent(&pool, "twice").await?;
            queue_test_start(&pool, twice, "cancelled").await?;
            queue_test_start(&pool, twice, "cancelled").await?;
            let first = claim_next_supervisor_command(&pool)
                .await?
                .ok_or("no first command")?;
            run_supervisor_command(&pool, &codex_registry, &claude_registry, first).await;
            assert_eq!(agent_status(&pool, twice).await?, "queued");
            let second = claim_next_supervisor_command(&pool)
                .await?
                .ok_or("no second command")?;
            run_supervisor_command(&pool, &codex_registry, &claude_registry, second).await;
            assert_eq!(agent_status(&pool, twice).await?, "idle");

            // A live run settles its agent's status itself when it ends.
            let busy = insert_test_agent(&pool, "busy").await?;
            sqlx::query("insert into agent_runs (agent_id, command, status) values ($1, 'codex', 'running')")
                .bind(busy)
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            queue_test_start(&pool, busy, "cancelled").await?;
            run_pending_commands(&pool).await?;
            assert_eq!(agent_status(&pool, busy).await?, "queued");
            Ok(())
        }
        .await;
        drop_test_schema(pool, database_path).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    /// Queues a start for a work item with `work_status`, as an inbox wake does.
    async fn queue_test_start(
        pool: &SqlitePool,
        agent_id: Uuid,
        work_status: &str,
    ) -> Result<Uuid, String> {
        let work_item_id: Uuid = sqlx::query_scalar(
            "insert into agent_work_items (agent_id, title, status) values ($1, 'request', $2) returning id",
        )
        .bind(agent_id)
        .bind(work_status)
        .fetch_one(pool)
        .await
        .map_err(|err| err.to_string())?;
        sqlx::query(
            "insert into supervisor_commands (command_type, agent_id, work_item_id) values ('start_agent', $1, $2)",
        )
        .bind(agent_id)
        .bind(work_item_id)
        .execute(pool)
        .await
        .map_err(|err| err.to_string())?;
        sqlx::query("update agents set status = 'queued' where id = $1")
            .bind(agent_id)
            .execute(pool)
            .await
            .map_err(|err| err.to_string())?;
        Ok(work_item_id)
    }

    async fn run_pending_commands(pool: &SqlitePool) -> Result<(), String> {
        let codex_registry = WarmCodexRegistry::default();
        let claude_registry = WarmClaudeRegistry::default();
        while let Some(command) = claim_next_supervisor_command(pool).await? {
            run_supervisor_command(pool, &codex_registry, &claude_registry, command).await;
        }
        Ok(())
    }

    async fn agent_status(pool: &SqlitePool, agent_id: Uuid) -> Result<String, String> {
        sqlx::query_scalar("select status from agents where id = $1")
            .bind(agent_id)
            .fetch_one(pool)
            .await
            .map_err(|err| err.to_string())
    }

    async fn latest_ui_event(pool: &SqlitePool) -> Result<i64, String> {
        sqlx::query_scalar("select coalesce(max(id), 0) from ui_events")
            .fetch_one(pool)
            .await
            .map_err(|err| err.to_string())
    }

    async fn refresh_reasons_since(pool: &SqlitePool, cursor: i64) -> Result<Vec<String>, String> {
        sqlx::query_scalar(
            r#"
            select json_extract(event_json, '$.reason')
            from ui_events
            where id > $1 and json_extract(event_json, '$.type') = 'refresh'
            order by id
            "#,
        )
        .bind(cursor)
        .fetch_all(pool)
        .await
        .map_err(|err| err.to_string())
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
