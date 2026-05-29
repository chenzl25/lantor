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
    surface::{append_claude_thread_context, same_codex_surface, CodexActiveTurnScheduleState},
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
    ui_notifications::{notify_ui_agent_run_changed, notify_ui_work_item_changed},
    usage::agent_budget_exhausted,
};

const SUPERVISOR_COMMAND_CONCURRENCY: usize = 4;
const SUPERVISOR_IDLE_SLEEP: Duration = Duration::from_secs(2);
const SUPERVISOR_ERROR_BACKOFF_INITIAL: Duration = Duration::from_secs(1);
const SUPERVISOR_ERROR_BACKOFF_MAX: Duration = Duration::from_secs(10);

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
          and w.status in ('cancelled', 'failed', 'done', 'silent')
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
    let mut error_backoff = SUPERVISOR_ERROR_BACKOFF_INITIAL;

    loop {
        match run_supervisor_iteration(
            &pool,
            &codex_registry,
            &claude_registry,
            &command_semaphore,
            &mut last_command_cleanup,
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
) -> CommandResult<bool> {
    write_supervisor_heartbeat(pool).await?;
    if last_command_cleanup.elapsed() >= Duration::from_secs(3600) {
        cleanup_supervisor_commands(pool).await?;
        *last_command_cleanup = Instant::now();
    }
    schedule_queued_work_items(pool, codex_registry).await?;
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
    agent_id: Uuid,
    channel_id: Option<Uuid>,
    thread_root_id: Option<Uuid>,
) -> CommandResult<bool> {
    let runtime = agent_runtime(pool, agent_id).await?;
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
        if !should_schedule_queued_work_item(pool, registry, agent_id, channel_id, thread_root_id)
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
            supervisor_stop_run(pool, codex_registry, run_id).await
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
        if status.as_deref() == Some("cancelled") {
            record_agent_activity(
                pool,
                Some(agent_id),
                None,
                "dispatch",
                "Cancelled agent request skipped",
                work_item_id.to_string(),
            )
            .await?;
            return Ok(());
        }
    }

    if let Some(reason) = agent_budget_exhausted(pool, agent_id).await? {
        if let Some(work_item_id) = work_item_id {
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
            .execute(pool)
            .await
            .map_err(to_string)?;
            notify_ui_work_item_changed(pool, work_item_id, "budget_exhausted").await;
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
        select handle, runtime, model, reasoning_effort, service_tier, launch_command, working_directory, avatar
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
    let working_directory: String = row.get::<String, _>("working_directory").trim().to_owned();
    let avatar: Option<String> = row.get("avatar");
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
                append_claude_thread_context(
                    pool,
                    &context,
                    channel_id,
                    channel_name.as_deref(),
                    thread_root_id,
                )
                .await?
            } else {
                context
            };
            let available_agents = load_channel_agent_roster(pool, channel_id, agent_id).await?;
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
            working_directory,
            work_item_prompt,
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
            work_item_prompt,
        },
    )
    .await
}

async fn supervisor_stop_run(
    pool: &SqlitePool,
    codex_registry: &WarmCodexRegistry,
    run_id: Uuid,
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
    .fetch_one(pool)
    .await
    .map_err(to_string)?;

    let agent_id: Uuid = row.get("agent_id");
    let pid: Option<i32> = row.get("pid");
    let work_item_id: Option<Uuid> = row.get("work_item_id");
    let runtime: String = row.get("runtime");
    let Some(pid) = pid else {
        return Err("agent run does not have a pid yet".to_owned());
    };

    sqlx::query("update agent_runs set status = 'stopping' where id = $1")
        .bind(run_id)
        .execute(pool)
        .await
        .map_err(to_string)?;
    notify_ui_agent_run_changed(pool, run_id, "run_stopping").await;
    sqlx::query("update agents set status = 'stopping' where id = $1")
        .bind(agent_id)
        .execute(pool)
        .await
        .map_err(to_string)?;
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
        .execute(pool)
        .await
        .map_err(to_string)?;
        notify_ui_work_item_changed(pool, work_item_id, "work_item_cancelling").await;
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
        select a.handle, a.display_name, a.runtime, a.model, a.status
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
            let display_name: String = row.get("display_name");
            let runtime: String = row.get("runtime");
            let model: String = row.get("model");
            let status: String = row.get("status");
            format!("@{handle} - {display_name} - {runtime}/{model} - {status}")
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use std::fs as std_fs;

    use sqlx::SqlitePool;
    use uuid::Uuid;

    use super::{
        claim_next_supervisor_command, load_channel_agent_roster,
        recover_supervisor_commands_at_startup,
    };
    use crate::db::{db_connect_with_url, migrate};

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
            assert_eq!(roster.len(), 1);
            assert!(roster[0].contains("@peer"));
            assert!(!roster[0].contains("@current"));
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
