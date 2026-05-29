use std::{collections::HashMap, process::Stdio, sync::Arc, time::Instant};

use serde_json::Value;
use sqlx::{Row, SqlitePool};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    process::Command,
    sync::Mutex as AsyncMutex,
};
use uuid::Uuid;

use crate::agent_memory::append_run_log;
use crate::app::{to_string, CommandResult};
use crate::events::activity::{record_agent_activity, record_agent_activity_throttled};
use crate::prompts::{build_claude_streaming_prompt, claude_system_prompt};
use crate::runtime::{
    process::{
        classify_agent_output_activity, configure_agent_context_tool_env,
        configure_agent_identity_env, upsert_runtime_thread_id,
    },
    streaming::{
        append_streaming_agent_message, ensure_streaming_agent_message, streaming_message_exists,
    },
};
use crate::ui_notifications::{
    notify_supervisor_wake, notify_ui_agent_run_changed, notify_ui_work_item_changed,
};
use crate::usage::{record_run_usage, usage_from_runtime_event};

mod protocol;
mod reaper;
mod turn;

use protocol::{
    claude_message_text, claude_result_error, claude_result_text, claude_session_id,
    claude_stream_event_activity, claude_stream_key, claude_streaming_command_text,
    claude_surface_boundary_marker, claude_text_delta, claude_user_input, claude_write_input,
    CLAUDE_MAX_RETRIES_ENV, DEFAULT_CLAUDE_MAX_RETRIES,
};
use reaper::claude_warm_idle_reaper;
use turn::finish_warm_claude_active_turn;

#[derive(Clone, Default)]
pub(crate) struct WarmClaudeRegistry {
    runtimes: Arc<AsyncMutex<HashMap<Uuid, Arc<WarmClaudeRuntime>>>>,
}

struct WarmClaudeRuntime {
    stdin: AsyncMutex<tokio::process::ChildStdin>,
    state: AsyncMutex<WarmClaudeState>,
    pid: Option<i32>,
}

struct WarmClaudeState {
    alive: bool,
    active: Option<ClaudeActiveTurn>,
    session_id: Option<String>,
    last_surface: Option<ClaudeSurface>,
    last_activity: Instant,
}

struct ClaudeActiveTurn {
    run_id: Uuid,
    started_at: Instant,
    first_delta_at: Option<Instant>,
    work_item_id: Option<Uuid>,
    channel_id: Option<Uuid>,
    thread_root_id: Option<Uuid>,
    stream_key: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ClaudeSurface {
    channel_id: Option<Uuid>,
    thread_root_id: Option<Uuid>,
}

async fn get_or_spawn_warm_claude_runtime(
    pool: &SqlitePool,
    registry: &WarmClaudeRegistry,
    agent_id: Uuid,
    handle: &str,
    model: &str,
    working_directory: &str,
    memory_context: Option<&str>,
) -> CommandResult<Arc<WarmClaudeRuntime>> {
    if let Some(runtime) = {
        let runtimes = registry.runtimes.lock().await;
        runtimes.get(&agent_id).cloned()
    } {
        if runtime.state.lock().await.alive {
            return Ok(runtime);
        }
        registry.runtimes.lock().await.remove(&agent_id);
    }

    let runtime = spawn_warm_claude_runtime(
        pool,
        registry.clone(),
        agent_id,
        handle,
        model,
        working_directory,
        memory_context,
    )
    .await?;
    registry
        .runtimes
        .lock()
        .await
        .insert(agent_id, runtime.clone());
    Ok(runtime)
}

async fn spawn_warm_claude_runtime(
    pool: &SqlitePool,
    registry: WarmClaudeRegistry,
    agent_id: Uuid,
    handle: &str,
    model: &str,
    working_directory: &str,
    memory_context: Option<&str>,
) -> CommandResult<Arc<WarmClaudeRuntime>> {
    let model = if model.trim().is_empty() {
        "sonnet".to_owned()
    } else {
        model.trim().to_owned()
    };
    let mut command = Command::new("claude");
    command
        .arg("-p")
        .arg("--system-prompt")
        .arg(claude_system_prompt(handle, memory_context))
        .arg("--model")
        .arg(&model)
        .arg("--output-format")
        .arg("stream-json")
        .arg("--input-format")
        .arg("stream-json")
        .arg("--include-partial-messages")
        .arg("--verbose")
        .arg("--permission-mode")
        .arg("bypassPermissions")
        .env(CLAUDE_MAX_RETRIES_ENV, DEFAULT_CLAUDE_MAX_RETRIES);
    configure_agent_identity_env(&mut command, agent_id, handle);
    configure_agent_context_tool_env(&mut command);
    #[cfg(unix)]
    command.process_group(0);
    if !working_directory.is_empty() {
        command.current_dir(working_directory);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            sqlx::query("update agents set status = 'error' where id = $1")
                .bind(agent_id)
                .execute(pool)
                .await
                .map_err(to_string)?;
            record_agent_activity(
                pool,
                Some(agent_id),
                None,
                "run_error",
                "Claude warm stream-json failed to start",
                err.to_string(),
            )
            .await?;
            return Err(err.to_string());
        }
    };

    let pid = child.id().map(|id| id as i32);
    let Some(stdin) = child.stdin.take() else {
        return Err("Claude stream-json stdin unavailable".to_owned());
    };
    let Some(stdout) = child.stdout.take() else {
        return Err("Claude stream-json stdout unavailable".to_owned());
    };
    let stderr = child.stderr.take();

    let runtime = Arc::new(WarmClaudeRuntime {
        stdin: AsyncMutex::new(stdin),
        state: AsyncMutex::new(WarmClaudeState {
            alive: true,
            active: None,
            session_id: None,
            last_surface: None,
            last_activity: Instant::now(),
        }),
        pid,
    });

    upsert_runtime_thread_id(
        pool,
        agent_id,
        "claude",
        &pid.map(|pid| format!("pid:{pid}"))
            .unwrap_or_else(|| "warming".to_owned()),
        "idle",
    )
    .await?;
    record_agent_activity(
        pool,
        Some(agent_id),
        None,
        "run",
        "Claude warm stream-json ready",
        pid.map(|pid| format!("pid={pid}"))
            .unwrap_or_else(|| "pid unavailable".to_owned()),
    )
    .await?;

    tokio::spawn(claude_warm_stdout_reader(
        pool.clone(),
        registry.clone(),
        agent_id,
        runtime.clone(),
        stdout,
    ));
    if let Some(stderr) = stderr {
        tokio::spawn(claude_warm_stderr_reader(
            pool.clone(),
            agent_id,
            runtime.clone(),
            stderr,
        ));
    }
    tokio::spawn(wait_for_warm_claude_process(
        pool.clone(),
        registry,
        agent_id,
        runtime.clone(),
        child,
    ));
    tokio::spawn(claude_warm_idle_reaper(
        pool.clone(),
        agent_id,
        runtime.clone(),
    ));

    Ok(runtime)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn supervisor_start_claude_streaming_agent(
    pool: &SqlitePool,
    claude_registry: &WarmClaudeRegistry,
    agent_id: Uuid,
    work_item_id: Option<Uuid>,
    handle: String,
    model: String,
    working_directory: String,
    work_item_prompt: String,
    memory_context: Option<String>,
) -> CommandResult<()> {
    let claude_prompt = build_claude_streaming_prompt(&work_item_prompt);
    let model = if model.trim().is_empty() {
        "sonnet".to_owned()
    } else {
        model.trim().to_owned()
    };
    let command_text = claude_streaming_command_text(&model);
    let runtime = match get_or_spawn_warm_claude_runtime(
        pool,
        claude_registry,
        agent_id,
        &handle,
        &model,
        &working_directory,
        memory_context.as_deref(),
    )
    .await
    {
        Ok(runtime) => runtime,
        Err(err) => {
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
                notify_ui_work_item_changed(pool, work_item_id, "work_item_failed").await;
            }
            return Err(err);
        }
    };

    {
        let state = runtime.state.lock().await;
        if !state.alive {
            return Err("Claude warm runtime is not alive".to_owned());
        }
        if state.active.is_some() {
            drop(state);
            record_agent_activity_throttled(
                pool,
                Some(agent_id),
                None,
                "dispatch",
                "Agent busy",
                work_item_id
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "no agent request".to_owned()),
            )
            .await?;
            return Ok(());
        }
    }
    let (channel_id, thread_root_id) = if let Some(work_item_id) = work_item_id {
        let row = sqlx::query(
            r#"
            select channel_id, thread_root_id
            from agent_work_items
            where id = $1 and agent_id = $2
            "#,
        )
        .bind(work_item_id)
        .bind(agent_id)
        .fetch_one(pool)
        .await
        .map_err(to_string)?;
        (
            row.get::<Option<Uuid>, _>("channel_id"),
            row.get::<Option<Uuid>, _>("thread_root_id"),
        )
    } else {
        (None, None)
    };
    let current_surface = ClaudeSurface {
        channel_id,
        thread_root_id,
    };
    let surface_boundary = {
        let state = runtime.state.lock().await;
        claude_surface_boundary_marker(state.last_surface, current_surface).unwrap_or_default()
    };
    let claude_prompt = if surface_boundary.is_empty() {
        claude_prompt
    } else {
        format!("{surface_boundary}{claude_prompt}")
    };

    let initial_log = if claude_prompt.is_empty() {
        format!("$ {command_text}\n[warm process reused]\n")
    } else {
        format!(
            "$ {command_text}\n[warm process reused]\n\n[streaming agent request]\n{claude_prompt}\n"
        )
    };

    let run_id: Uuid = sqlx::query_scalar(
        r#"
        insert into agent_runs (agent_id, work_item_id, command, working_directory, status, log)
        values ($1, $2, $3, $4, 'starting', $5)
        returning id
        "#,
    )
    .bind(agent_id)
    .bind(work_item_id)
    .bind(&command_text)
    .bind(&working_directory)
    .bind(initial_log)
    .fetch_one(pool)
    .await
    .map_err(to_string)?;
    notify_ui_agent_run_changed(pool, run_id, "run_created").await;

    if let Some(work_item_id) = work_item_id {
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
        .execute(pool)
        .await
        .map_err(to_string)?;
        notify_ui_work_item_changed(pool, work_item_id, "work_item_running").await;
        record_agent_activity(
            pool,
            Some(agent_id),
            Some(run_id),
            "dispatch",
            "Request started",
            work_item_id.to_string(),
        )
        .await?;
    }

    sqlx::query("update agent_runs set status = 'running', pid = $2 where id = $1")
        .bind(run_id)
        .bind(runtime.pid)
        .execute(pool)
        .await
        .map_err(to_string)?;
    notify_ui_agent_run_changed(pool, run_id, "run_running").await;
    sqlx::query("update agents set status = 'running' where id = $1")
        .bind(agent_id)
        .execute(pool)
        .await
        .map_err(to_string)?;
    record_agent_activity(
        pool,
        Some(agent_id),
        Some(run_id),
        "run",
        "Started working",
        runtime
            .pid
            .map(|pid| format!("pid={pid}"))
            .unwrap_or_else(|| "pid unavailable".to_owned()),
    )
    .await?;

    let stream_key = claude_stream_key(run_id);
    if let Some(channel_id) = channel_id {
        ensure_streaming_agent_message(pool, agent_id, channel_id, thread_root_id, &stream_key)
            .await?;
    }
    {
        let mut state = runtime.state.lock().await;
        if !state.alive {
            return Err("Claude warm runtime exited before turn start".to_owned());
        }
        if state.active.is_some() {
            return Err("Claude warm runtime became busy before turn start".to_owned());
        }
        state.last_activity = Instant::now();
        state.active = Some(ClaudeActiveTurn {
            run_id,
            started_at: Instant::now(),
            first_delta_at: None,
            work_item_id,
            channel_id,
            thread_root_id,
            stream_key,
        });
    }

    let write_result = {
        let mut stdin = runtime.stdin.lock().await;
        claude_write_input(&mut stdin, claude_user_input(&claude_prompt)).await
    };

    if let Err(err) = write_result {
        finish_warm_claude_active_turn(pool, agent_id, &runtime, false, Some(err.clone())).await?;
        return Err(err);
    }

    Ok(())
}

async fn claude_warm_stdout_reader<R>(
    pool: SqlitePool,
    registry: WarmClaudeRegistry,
    agent_id: Uuid,
    runtime: Arc<WarmClaudeRuntime>,
    stream: R,
) where
    R: AsyncRead + Unpin,
{
    let mut lines = BufReader::new(stream).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if let Err(err) =
                    handle_claude_warm_stdout_line(&pool, agent_id, &runtime, &line).await
                {
                    let _ = record_agent_activity(
                        &pool,
                        Some(agent_id),
                        None,
                        "run_error",
                        "Claude stream event failed",
                        err,
                    )
                    .await;
                }
            }
            Ok(None) => break,
            Err(err) => {
                let _ = record_agent_activity(
                    &pool,
                    Some(agent_id),
                    None,
                    "run_error",
                    "Claude stdout read failed",
                    err.to_string(),
                )
                .await;
                break;
            }
        }
    }

    {
        let mut state = runtime.state.lock().await;
        state.alive = false;
    }
    let _ = finish_warm_claude_active_turn(
        &pool,
        agent_id,
        &runtime,
        false,
        Some("stdout closed".to_owned()),
    )
    .await;
    remove_warm_claude_runtime_if_same(&registry, agent_id, &runtime).await;
}

async fn handle_claude_warm_stdout_line(
    pool: &SqlitePool,
    agent_id: Uuid,
    runtime: &Arc<WarmClaudeRuntime>,
    line: &str,
) -> CommandResult<()> {
    let active_run_id = {
        runtime
            .state
            .lock()
            .await
            .active
            .as_ref()
            .map(|active| active.run_id)
    };
    if let Some(run_id) = active_run_id {
        append_run_log(pool, run_id, format!("[claude] {line}\n")).await?;
    }
    let value: Value = serde_json::from_str(line).map_err(to_string)?;

    if let (Some(run_id), Some((input_tokens, output_tokens))) =
        (active_run_id, usage_from_runtime_event(&value))
    {
        let _ = record_run_usage(pool, agent_id, run_id, input_tokens, output_tokens, None).await;
    }

    if let Some(session_id) = claude_session_id(&value) {
        {
            let mut state = runtime.state.lock().await;
            state.session_id = Some(session_id.to_owned());
            state.last_activity = Instant::now();
        }
        let status = if active_run_id.is_some() {
            "running"
        } else {
            "idle"
        };
        let _ = upsert_runtime_thread_id(pool, agent_id, "claude", session_id, status).await;
    }

    if let Some((kind, title, detail)) = claude_stream_event_activity(&value) {
        if let Some(run_id) = active_run_id {
            record_agent_activity_throttled(
                pool,
                Some(agent_id),
                Some(run_id),
                kind,
                title,
                detail,
            )
            .await?;
        }
    }

    if let Some(delta) = claude_text_delta(&value) {
        let (active, first_delta_elapsed) = {
            let mut state = runtime.state.lock().await;
            let (active, first_delta_elapsed) = {
                let Some(active) = state.active.as_mut() else {
                    return Ok(());
                };
                let first_delta_elapsed = if active.first_delta_at.is_none() {
                    let elapsed = active.started_at.elapsed();
                    active.first_delta_at = Some(Instant::now());
                    Some(elapsed)
                } else {
                    None
                };
                let active = (
                    active.run_id,
                    active.channel_id,
                    active.thread_root_id,
                    active.stream_key.clone(),
                );
                (active, first_delta_elapsed)
            };
            state.last_activity = Instant::now();
            (active, first_delta_elapsed)
        };
        if let Some(elapsed) = first_delta_elapsed {
            record_agent_activity(
                pool,
                Some(agent_id),
                Some(active.0),
                "acting",
                "Responding",
                format!("first_token_ms={}", elapsed.as_millis()),
            )
            .await?;
        }
        if let Some(channel_id) = active.1 {
            append_streaming_agent_message(pool, agent_id, channel_id, active.2, &active.3, delta)
                .await?;
        }
        return Ok(());
    }

    if let Some(text) = claude_message_text(&value).or_else(|| claude_result_text(&value)) {
        let active = {
            let state = runtime.state.lock().await;
            state.active.as_ref().map(|active| {
                (
                    active.run_id,
                    active.channel_id,
                    active.thread_root_id,
                    active.stream_key.clone(),
                )
            })
        };
        if let Some((_, Some(channel_id), thread_root_id, stream_key)) = active {
            if !streaming_message_exists(pool, &stream_key).await? {
                append_streaming_agent_message(
                    pool,
                    agent_id,
                    channel_id,
                    thread_root_id,
                    &stream_key,
                    &text,
                )
                .await?;
            }
        }
    }

    if let Some(error) = claude_result_error(&value) {
        finish_warm_claude_active_turn(pool, agent_id, runtime, false, Some(error)).await?;
    } else if value.get("type").and_then(Value::as_str) == Some("result") {
        finish_warm_claude_active_turn(pool, agent_id, runtime, true, None).await?;
    }

    Ok(())
}

async fn claude_warm_stderr_reader<R>(
    pool: SqlitePool,
    agent_id: Uuid,
    runtime: Arc<WarmClaudeRuntime>,
    stream: R,
) where
    R: AsyncRead + Unpin,
{
    let mut lines = BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let active_run_id = {
            runtime
                .state
                .lock()
                .await
                .active
                .as_ref()
                .map(|active| active.run_id)
        };
        if let Some(run_id) = active_run_id {
            let _ = append_run_log(&pool, run_id, format!("[stderr] {line}\n")).await;
            if let Some((kind, title, detail)) = classify_agent_output_activity("stderr", &line) {
                let _ = record_agent_activity_throttled(
                    &pool,
                    Some(agent_id),
                    Some(run_id),
                    kind,
                    title,
                    detail,
                )
                .await;
            }
        }
    }
}

async fn wait_for_warm_claude_process(
    pool: SqlitePool,
    registry: WarmClaudeRegistry,
    agent_id: Uuid,
    runtime: Arc<WarmClaudeRuntime>,
    mut child: tokio::process::Child,
) {
    let wait_result = child.wait().await;
    {
        let mut state = runtime.state.lock().await;
        state.alive = false;
    }
    let detail = match wait_result {
        Ok(status) => format!("claude stream-json exited: {status}"),
        Err(err) => format!("claude stream-json wait failed: {err}"),
    };
    let _ = finish_warm_claude_active_turn(&pool, agent_id, &runtime, false, Some(detail.clone()))
        .await;
    let _ = sqlx::query(
        "update runtime_sessions set status = 'stopped', updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') where agent_id = $1 and runtime = 'claude'",
    )
    .bind(agent_id)
    .execute(&pool)
    .await;
    let _ = record_agent_activity(
        &pool,
        Some(agent_id),
        None,
        "run",
        "Claude warm stream-json exited",
        detail,
    )
    .await;
    remove_warm_claude_runtime_if_same(&registry, agent_id, &runtime).await;
    let _ = notify_supervisor_wake(&pool).await;
}

async fn remove_warm_claude_runtime_if_same(
    registry: &WarmClaudeRegistry,
    agent_id: Uuid,
    runtime: &Arc<WarmClaudeRuntime>,
) {
    let mut runtimes = registry.runtimes.lock().await;
    if runtimes
        .get(&agent_id)
        .map(|current| Arc::ptr_eq(current, runtime))
        .unwrap_or(false)
    {
        runtimes.remove(&agent_id);
    }
}
