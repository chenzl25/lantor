use std::{process::Stdio, sync::Arc, time::Instant};

use serde_json::Value;
use sqlx::{Row, SqlitePool};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    process::Command,
    sync::Mutex as AsyncMutex,
};
use uuid::Uuid;

use crate::agent_environment::apply_agent_environment_variables;
use crate::agent_memory::append_run_log;
use crate::app::{to_string, CommandResult};
use crate::events::activity::{
    record_agent_activity, record_agent_activity_throttled, record_run_tool_call, RunToolCall,
};
use crate::freshness::advance_agent_target_watermark_for_work_item;
use crate::prompts::{build_claude_streaming_prompt, claude_system_prompt, prepend_memory_context};
use crate::runtime::{
    lifecycle::{
        create_run_in_tx, fail_work_item_before_run, mark_agent_runtime_failed,
        mark_run_running_in_tx, NewRun, WarmStartFailure,
    },
    memory_context_update,
    process::{
        cleanup_failed_warm_start, configure_agent_context_tool_env, configure_agent_identity_env,
        truncate_activity_detail, upsert_runtime_thread_id,
    },
    streaming::{
        append_streaming_agent_message, ensure_streaming_agent_message,
        reconcile_streaming_agent_message, start_streaming_agent_text_block,
    },
    warm::{
        reusable_runtime, wait_for_warm_process, warm_stderr_reader, WarmProvider, WarmRegistry,
        WarmRuntime, WarmState,
    },
};
use crate::subscription_status::{
    claude_subscription_status_from_event, persist_agent_subscription_status,
};
use crate::ui_notifications::reconcile_work_item_change;
use crate::usage::{record_run_usage, usage_from_runtime_event};

mod interrupt;
mod protocol;
mod reaper;
mod session;
mod steer;
mod text;
mod turn;

pub(crate) use interrupt::interrupt_warm_claude_run;
pub(crate) use steer::steerable_claude_turn_surface;
use steer::{
    note_claude_input_replayed, steer_warm_claude_turn_if_same_surface, ClaudePendingSteer,
    ClaudeSteerRequest,
};
use text::ClaudeTextState;

#[cfg(test)]
mod interrupt_tests;
#[cfg(test)]
mod network_retry_tests;
#[cfg(test)]
mod recovery_tests;
#[cfg(test)]
mod steer_tests;

use protocol::{
    claude_auth_failure, claude_context_tokens, claude_main_tool_use_started,
    claude_message_text_blocks, claude_network_failure, claude_replayed_input_uuid,
    claude_result_error, claude_result_text, claude_resume_session_missing, claude_session_id,
    claude_stream_event_activity, claude_stream_key, claude_streaming_command_text,
    claude_surface_boundary_marker, claude_text_delta, claude_user_input, claude_write_input,
    CLAUDE_DISABLE_BACKGROUND_TASKS_ENV, CLAUDE_DISABLE_CRON_ENV, CLAUDE_DISALLOWED_TOOLS,
    CLAUDE_MAX_RETRIES_ENV, DEFAULT_CLAUDE_MAX_RETRIES,
};
use reaper::claude_warm_idle_reaper;
use session::{
    claude_context_rotate_tokens, plan_claude_session_start, prepend_claude_rotation_marker,
    restart_after_missing_session, store_claude_context_tokens, ClaudeSessionStart,
};
use turn::finish_warm_claude_active_turn;

const CLAUDE_DISABLE_AUTO_MEMORY_ENV: &str = "CLAUDE_CODE_DISABLE_AUTO_MEMORY";
const CLAUDE_DISABLE_AUTO_MEMORY_VALUE: &str = "1";

fn configure_claude_process_environment(command: &mut Command) {
    // A provider result releases the Lantor run and its channel/thread ownership.
    // Native background tasks can start another provider turn after that boundary,
    // with no Lantor request to receive it (or while another target owns the session).
    // Apply these invariants after agent-configured environment overrides.
    command
        .env(CLAUDE_DISABLE_BACKGROUND_TASKS_ENV, "1")
        .env(CLAUDE_DISABLE_CRON_ENV, "1")
        .env(
            CLAUDE_DISABLE_AUTO_MEMORY_ENV,
            CLAUDE_DISABLE_AUTO_MEMORY_VALUE,
        )
        .env(CLAUDE_MAX_RETRIES_ENV, DEFAULT_CLAUDE_MAX_RETRIES);
}

/// The Claude CLI in stream-json mode as a warm runtime provider.
pub(crate) struct ClaudeCli;

pub(crate) type WarmClaudeRegistry = WarmRegistry<ClaudeCli>;
type WarmClaudeRuntime = WarmRuntime<ClaudeCli>;

impl WarmProvider for ClaudeCli {
    type State = WarmClaudeState;
    const RUNTIME: &'static str = "claude";
    const PROCESS_TITLE: &'static str = "Claude warm stream-json";
    const PROCESS_NAME: &'static str = "claude stream-json";

    fn session_thread_id(&self) -> Option<&str> {
        None
    }

    async fn finish_turn_after_exit(
        pool: &SqlitePool,
        agent_id: Uuid,
        runtime: &Arc<WarmClaudeRuntime>,
        detail: String,
    ) {
        let _ = finish_warm_claude_active_turn(pool, agent_id, runtime, false, Some(detail)).await;
    }
}

pub(crate) struct WarmClaudeState {
    alive: bool,
    active: Option<ClaudeActiveTurn>,
    session_id: Option<String>,
    /// Context size of the latest main-conversation request, persisted at turn
    /// end so a respawn can decide between resuming and rotating the session.
    context_tokens: i64,
    /// Set when this process rotated away from an oversized session; the first
    /// turn tells the model where the previous run can be read.
    pending_rotation_marker: Option<String>,
    last_surface: Option<ClaudeSurface>,
    injected_memory_context: Option<String>,
    last_activity: Instant,
    /// Follow-ups written to the CLI that it has not replayed yet.
    pending_steers: Vec<ClaudePendingSteer>,
}

impl WarmClaudeState {
    /// A turn is running, or the CLI still owes a turn for a follow-up. A new
    /// request written now would be answered after that turn, not as its own.
    fn is_busy(&self) -> bool {
        self.active.is_some() || !self.pending_steers.is_empty()
    }
}

impl WarmState for WarmClaudeState {
    fn alive(&self) -> bool {
        self.alive
    }

    fn set_alive(&mut self, alive: bool) {
        self.alive = alive;
    }

    fn touch(&mut self) {
        self.last_activity = Instant::now();
    }

    fn active_run_id(&self) -> Option<Uuid> {
        self.active.as_ref().map(|active| active.run_id)
    }

    fn is_busy(&self) -> bool {
        WarmClaudeState::is_busy(self)
    }
}

struct ClaudeActiveTurn {
    run_id: Uuid,
    started_at: Instant,
    first_delta_at: Option<Instant>,
    work_item_id: Option<Uuid>,
    channel_id: Option<Uuid>,
    thread_root_id: Option<Uuid>,
    stream_key: String,
    pending_memory_context: Option<Option<String>>,
    text: ClaudeTextState,
    /// Lantor asked the CLI to interrupt this turn, so its error result is a stop.
    interrupt_requested: bool,
    /// The turn's latest provider reply said the login is no longer valid.
    auth_required: bool,
    /// The turn's result says the connection to the API failed.
    network_failure: bool,
}

/// Reconcile complete provider text before finalization flushes the control gate.
async fn reconcile_final_text(
    pool: &SqlitePool,
    agent_id: Uuid,
    runtime: &Arc<WarmClaudeRuntime>,
    value: &Value,
) -> CommandResult<()> {
    let blocks = claude_message_text_blocks(value);
    let result = claude_result_text(value);
    if blocks.is_none() && result.is_none() {
        return Ok(());
    }
    let (mut text, run_id, channel_id, thread_root_id, stream_key) = {
        let state = runtime.state.lock().await;
        let Some(active) = state.active.as_ref() else {
            return Ok(());
        };
        (
            active.text.clone(),
            active.run_id,
            active.channel_id,
            active.thread_root_id,
            active.stream_key.clone(),
        )
    };
    let changed = if let Some(blocks) = blocks {
        text.assistant(
            value.get("uuid").and_then(Value::as_str),
            value.pointer("/message/id").and_then(Value::as_str),
            &blocks,
        )
    } else if let Some(result) = result {
        text.result(&result)
    } else {
        return Ok(());
    };
    if changed {
        if let Some(channel_id) = channel_id {
            reconcile_streaming_agent_message(
                pool,
                agent_id,
                channel_id,
                thread_root_id,
                &stream_key,
                &text.visible_blocks(),
            )
            .await?;
            record_agent_activity(
                pool,
                Some(agent_id),
                Some(run_id),
                "warning",
                "Recovered incomplete streamed reply",
                "Restored the reply from complete provider text; control events were deduplicated.",
            )
            .await?;
        }
    }
    if let Some(active) = runtime.state.lock().await.active.as_mut() {
        if active.run_id == run_id {
            active.text = text;
        }
    }
    Ok(())
}

/// Rewrite the finished reply without progress notes written between tool
/// calls. Their controls were already delivered live and replay idempotently.
async fn drop_interim_progress_text(
    pool: &SqlitePool,
    agent_id: Uuid,
    runtime: &Arc<WarmClaudeRuntime>,
) -> CommandResult<()> {
    let (blocks, channel_id, thread_root_id, stream_key) = {
        let state = runtime.state.lock().await;
        let Some(active) = state.active.as_ref() else {
            return Ok(());
        };
        if !active.text.has_interim_prose() {
            return Ok(());
        }
        (
            active.text.visible_blocks(),
            active.channel_id,
            active.thread_root_id,
            active.stream_key.clone(),
        )
    };
    let Some(channel_id) = channel_id else {
        return Ok(());
    };
    reconcile_streaming_agent_message(
        pool,
        agent_id,
        channel_id,
        thread_root_id,
        &stream_key,
        &blocks,
    )
    .await
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ClaudeSurface {
    channel_id: Option<Uuid>,
    thread_root_id: Option<Uuid>,
}

#[derive(Clone, Copy)]
struct ClaudeRuntimeConfig<'a> {
    handle: &'a str,
    model: &'a str,
    reasoning_effort: &'a str,
    working_directory: &'a str,
    environment_variables: &'a str,
}

async fn get_or_spawn_warm_claude_runtime(
    pool: &SqlitePool,
    registry: &WarmClaudeRegistry,
    agent_id: Uuid,
    config: ClaudeRuntimeConfig<'_>,
) -> CommandResult<Arc<WarmClaudeRuntime>> {
    if let Some(runtime) = reusable_runtime(
        pool,
        registry,
        agent_id,
        config.environment_variables,
        false,
    )
    .await?
    {
        return Ok(runtime);
    }

    let runtime = spawn_warm_claude_runtime(pool, registry.clone(), agent_id, config).await?;
    registry.insert(agent_id, runtime.clone()).await;
    Ok(runtime)
}

/// Common CLI flags/environment for warm turns and read-only tool discovery.
pub(super) fn configure_claude_command(
    command: &mut Command,
    agent_id: Uuid,
    handle: &str,
    working_directory: &str,
    environment_variables: &str,
) -> CommandResult<()> {
    apply_agent_environment_variables(command, environment_variables)?;
    command
        .arg("--output-format")
        .arg("stream-json")
        .arg("--input-format")
        .arg("stream-json")
        .arg("--include-partial-messages")
        // Echoes each input with its uuid when consumed; follow-ups need it.
        .arg("--replay-user-messages")
        .arg("--verbose")
        // Monitor has its own enablement gate in Claude Code, independent of
        // CLAUDE_CODE_DISABLE_BACKGROUND_TASKS. It can also emit unsolicited turns.
        // ScheduleWakeup stays visible with cron disabled and reports success
        // although the wakeup never fires; Lantor loops replace it.
        .arg("--disallowedTools")
        .arg(CLAUDE_DISALLOWED_TOOLS)
        .arg("--permission-mode")
        .arg("bypassPermissions");
    configure_claude_process_environment(command);
    configure_agent_identity_env(command, agent_id, handle);
    configure_agent_context_tool_env(command);
    #[cfg(unix)]
    command.process_group(0);
    if !working_directory.is_empty() {
        command.current_dir(working_directory);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    Ok(())
}

async fn spawn_warm_claude_runtime(
    pool: &SqlitePool,
    registry: WarmClaudeRegistry,
    agent_id: Uuid,
    config: ClaudeRuntimeConfig<'_>,
) -> CommandResult<Arc<WarmClaudeRuntime>> {
    let model = if config.model.trim().is_empty() {
        "sonnet".to_owned()
    } else {
        config.model.trim().to_owned()
    };
    let effort = config.reasoning_effort.trim();
    // Must run before this spawn overwrites the stored session with a pid placeholder.
    let session_start =
        plan_claude_session_start(pool, agent_id, claude_context_rotate_tokens()).await?;
    let mut command = Command::new("claude");
    command
        .arg("--system-prompt")
        .arg(claude_system_prompt(config.handle))
        .arg("--model")
        .arg(&model);
    if !effort.is_empty() {
        command.arg("--effort").arg(effort);
    }
    if let Some(session_id) = session_start.resume_session_id() {
        command.arg("--resume").arg(session_id);
    }
    configure_claude_command(
        &mut command,
        agent_id,
        config.handle,
        config.working_directory,
        config.environment_variables,
    )?;

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            mark_agent_runtime_failed(pool, agent_id).await?;
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

    let (session_id, context_tokens, last_surface) = match &session_start {
        ClaudeSessionStart::Resume {
            session_id,
            context_tokens,
            last_surface,
        } => (Some(session_id.clone()), *context_tokens, *last_surface),
        _ => (None, 0, None),
    };
    let runtime = Arc::new(WarmClaudeRuntime {
        stdin: AsyncMutex::new(stdin),
        state: AsyncMutex::new(WarmClaudeState {
            alive: true,
            active: None,
            session_id: session_id.clone(),
            context_tokens,
            pending_rotation_marker: session_start.rotation_marker(),
            last_surface,
            injected_memory_context: None,
            last_activity: Instant::now(),
            pending_steers: Vec::new(),
        }),
        pid,
        environment_variables: config.environment_variables.to_owned(),
        provider: ClaudeCli,
    });

    let provider_thread_id = session_id.unwrap_or_else(|| {
        pid.map(|pid| format!("pid:{pid}"))
            .unwrap_or_else(|| "warming".to_owned())
    });
    upsert_runtime_thread_id(pool, agent_id, "claude", &provider_thread_id, "idle").await?;
    store_claude_context_tokens(pool, agent_id, context_tokens).await?;
    record_agent_activity(
        pool,
        Some(agent_id),
        None,
        "run",
        "Claude warm stream-json ready",
        format!(
            "{}, {}",
            pid.map(|pid| format!("pid={pid}"))
                .unwrap_or_else(|| "pid unavailable".to_owned()),
            session_start.describe()
        ),
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
        tokio::spawn(warm_stderr_reader(
            pool.clone(),
            agent_id,
            runtime.clone(),
            stderr,
        ));
    }
    tokio::spawn(wait_for_warm_process(
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
    reasoning_effort: String,
    working_directory: String,
    environment_variables: String,
    work_item_prompt: String,
    rendered_context_max_seq: i64,
    memory_context: Option<String>,
) -> CommandResult<()> {
    let claude_prompt = build_claude_streaming_prompt(&work_item_prompt);
    let model = if model.trim().is_empty() {
        "sonnet".to_owned()
    } else {
        model.trim().to_owned()
    };
    let effort = reasoning_effort.trim().to_owned();
    let command_text = claude_streaming_command_text(&model, &effort);
    let runtime = match get_or_spawn_warm_claude_runtime(
        pool,
        claude_registry,
        agent_id,
        ClaudeRuntimeConfig {
            handle: &handle,
            model: &model,
            reasoning_effort: &effort,
            working_directory: &working_directory,
            environment_variables: &environment_variables,
        },
    )
    .await
    {
        Ok(runtime) => runtime,
        Err(err) => {
            if let Some(work_item_id) = work_item_id {
                fail_work_item_before_run(pool, work_item_id, "work_item_failed").await?;
                reconcile_work_item_change(pool, work_item_id, "work_item_failed").await?;
            }
            return Err(err);
        }
    };

    {
        let state = runtime.state.lock().await;
        if !state.alive {
            return Err("Claude warm runtime is not alive".to_owned());
        }
        if state.is_busy() {
            drop(state);
            if let Some(work_item_id) = work_item_id {
                let request = ClaudeSteerRequest {
                    work_item_id,
                    prompt: &claude_prompt,
                    rendered_context_max_seq,
                    command_text: &command_text,
                    working_directory: &working_directory,
                };
                if steer_warm_claude_turn_if_same_surface(pool, agent_id, &runtime, request).await?
                {
                    return Ok(());
                }
            }
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
    let (surface_boundary, rotation_marker, memory_update) = {
        let state = runtime.state.lock().await;
        (
            claude_surface_boundary_marker(state.last_surface, current_surface).unwrap_or_default(),
            state.pending_rotation_marker.clone(),
            memory_context_update(
                state.injected_memory_context.as_deref(),
                memory_context.as_deref(),
            ),
        )
    };
    let claude_prompt = if surface_boundary.is_empty() {
        claude_prompt
    } else {
        format!("{surface_boundary}{claude_prompt}")
    };
    let claude_prompt = match rotation_marker {
        Some(marker) => prepend_claude_rotation_marker(&claude_prompt, &marker),
        None => claude_prompt,
    };
    let (claude_turn_prompt, next_injected_memory_context) = match memory_update {
        Some((memory_prompt, next_memory_context)) => (
            prepend_memory_context(claude_prompt.clone(), Some(&memory_prompt)),
            Some(next_memory_context),
        ),
        None => (claude_prompt.clone(), None),
    };

    let initial_log = if claude_prompt.is_empty() {
        format!("$ {command_text}\n[warm process reused]\n")
    } else {
        format!(
            "$ {command_text}\n[warm process reused]\n\n[streaming agent request]\n{claude_prompt}\n"
        )
    };

    let mut transaction = pool.begin().await.map_err(to_string)?;
    let run_id = create_run_in_tx(
        &mut transaction,
        &NewRun {
            agent_id,
            work_item_id,
            command: &command_text,
            working_directory: &working_directory,
            log: &initial_log,
        },
    )
    .await?;
    mark_run_running_in_tx(&mut transaction, agent_id, run_id, runtime.pid).await?;
    transaction.commit().await.map_err(to_string)?;
    if let Some(work_item_id) = work_item_id {
        reconcile_work_item_change(pool, work_item_id, "work_item_running").await?;
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
        if let Err(err) =
            ensure_streaming_agent_message(pool, agent_id, channel_id, thread_root_id, &stream_key)
                .await
        {
            cleanup_failed_warm_start(
                pool,
                "claude",
                agent_id,
                run_id,
                work_item_id,
                &err,
                WarmStartFailure::Transient,
            )
            .await?;
            return Err(err);
        }
    }
    const CLAUDE_BUSY_BEFORE_TURN_START: &str = "Claude warm runtime became busy before turn start";
    let occupy_result = {
        let mut state = runtime.state.lock().await;
        if !state.alive {
            Err("Claude warm runtime exited before turn start".to_owned())
        } else if state.is_busy() {
            Err(CLAUDE_BUSY_BEFORE_TURN_START.to_owned())
        } else {
            state.last_activity = Instant::now();
            state.pending_rotation_marker = None;
            state.active = Some(ClaudeActiveTurn {
                run_id,
                started_at: Instant::now(),
                first_delta_at: None,
                work_item_id,
                channel_id,
                thread_root_id,
                stream_key,
                pending_memory_context: next_injected_memory_context,
                text: ClaudeTextState::default(),
                interrupt_requested: false,
                auth_required: false,
                network_failure: false,
            });
            Ok(())
        }
    };
    if let Err(err) = occupy_result {
        cleanup_failed_warm_start(
            pool,
            "claude",
            agent_id,
            run_id,
            work_item_id,
            &err,
            if err == CLAUDE_BUSY_BEFORE_TURN_START {
                WarmStartFailure::RuntimeBusy
            } else {
                WarmStartFailure::Transient
            },
        )
        .await?;
        return Err(err);
    }

    let write_result = {
        let mut stdin = runtime.stdin.lock().await;
        claude_write_input(&mut stdin, claude_user_input(&claude_turn_prompt)).await
    };

    if let Err(err) = write_result {
        finish_warm_claude_active_turn(pool, agent_id, &runtime, false, Some(err.clone())).await?;
        return Err(err);
    }
    if let Some(work_item_id) = work_item_id {
        advance_agent_target_watermark_for_work_item(
            pool,
            agent_id,
            work_item_id,
            rendered_context_max_seq,
        )
        .await?;
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
    registry.remove_if_same(agent_id, &runtime).await;
}

/// Whether the stdout line still needs the turn-ending check after its
/// effects were applied.
enum ClaudeLineFlow {
    Continue,
    /// A special line (input replay, missing session) fully handled the turn.
    Handled,
}

async fn handle_claude_warm_stdout_line(
    pool: &SqlitePool,
    agent_id: Uuid,
    runtime: &Arc<WarmClaudeRuntime>,
    line: &str,
) -> CommandResult<()> {
    let parsed = serde_json::from_str::<Value>(line).ok();
    if let Some(auth_failed) = parsed.as_ref().and_then(claude_auth_failure) {
        if let Some(active) = runtime.state.lock().await.active.as_mut() {
            active.auth_required = auth_failed;
        }
    }
    if parsed.as_ref().is_some_and(claude_network_failure) {
        if let Some(active) = runtime.state.lock().await.active.as_mut() {
            active.network_failure = true;
        }
    }
    let applied = apply_claude_warm_stdout_line(pool, agent_id, runtime, line).await;
    if matches!(applied, Ok(ClaudeLineFlow::Handled)) {
        return Ok(());
    }
    // A `result` line ends the turn even when recording its effects failed;
    // otherwise the turn would stay active forever and block the agent.
    let Some(value) =
        parsed.filter(|value| value.get("type").and_then(Value::as_str) == Some("result"))
    else {
        return applied.map(|_| ());
    };
    let finished = if let Some(error) = claude_result_error(&value) {
        finish_warm_claude_active_turn(pool, agent_id, runtime, false, Some(error)).await
    } else {
        let dropped = if applied.is_ok() {
            drop_interim_progress_text(pool, agent_id, runtime).await
        } else {
            Ok(())
        };
        let finished = finish_warm_claude_active_turn(pool, agent_id, runtime, true, None).await;
        dropped.and(finished)
    };
    applied.and(finished)
}

async fn apply_claude_warm_stdout_line(
    pool: &SqlitePool,
    agent_id: Uuid,
    runtime: &Arc<WarmClaudeRuntime>,
    line: &str,
) -> CommandResult<ClaudeLineFlow> {
    let active_run_id = {
        runtime
            .state
            .lock()
            .await
            .active
            .as_ref()
            .map(|active| active.run_id)
    };
    let value = serde_json::from_str::<Value>(line);
    let replayed_uuid = value
        .as_ref()
        .ok()
        .and_then(claude_replayed_input_uuid)
        .map(str::to_owned);
    if let Some(run_id) = active_run_id {
        // A replay repeats the whole prompt; the uuid is enough for the log.
        let entry = match &replayed_uuid {
            Some(uuid) => format!("[claude] input replayed uuid={uuid}\n"),
            None => format!("[claude] {line}\n"),
        };
        append_run_log(pool, run_id, entry).await?;
    }
    let value = value.map_err(to_string)?;
    if let Some(uuid) = replayed_uuid {
        return note_claude_input_replayed(pool, agent_id, runtime, &uuid)
            .await
            .map(|_| ClaudeLineFlow::Handled);
    }

    // Handle before the session-id capture below, which would store the missing ID again.
    if claude_resume_session_missing(&value) {
        return restart_after_missing_session(pool, agent_id, runtime, &value)
            .await
            .map(|_| ClaudeLineFlow::Handled);
    }

    if let Some(snapshot) = claude_subscription_status_from_event(&value) {
        persist_agent_subscription_status(pool, agent_id, &snapshot).await?;
    }

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

    if let Some(context_tokens) = claude_context_tokens(&value) {
        runtime.state.lock().await.context_tokens = context_tokens;
    }

    if let Some((kind, title, detail)) = claude_stream_event_activity(&value) {
        if let Some(run_id) = active_run_id {
            match RunToolCall::from_activity_kind(kind) {
                Some(call) => {
                    record_run_tool_call(pool, agent_id, run_id, call, title, detail).await?
                }
                None => {
                    record_agent_activity_throttled(
                        pool,
                        Some(agent_id),
                        Some(run_id),
                        kind,
                        title,
                        detail,
                    )
                    .await?
                }
            }
        }
    }

    if value.get("type").and_then(Value::as_str) == Some("stream_event")
        && value.pointer("/event/type").and_then(Value::as_str) == Some("message_start")
    {
        if let Some(active) = runtime.state.lock().await.active.as_mut() {
            active
                .text
                .start_message(value.pointer("/event/message/id").and_then(Value::as_str));
        }
    }

    if value.get("type").and_then(Value::as_str) == Some("stream_event")
        && value.pointer("/event/type").and_then(Value::as_str) == Some("content_block_start")
        && value
            .pointer("/event/content_block/type")
            .and_then(Value::as_str)
            == Some("text")
    {
        let active = runtime.state.lock().await.active.as_mut().map(|active| {
            active.text.start_block();
            (
                active.channel_id,
                active.thread_root_id,
                active.stream_key.clone(),
            )
        });
        if let Some((Some(channel_id), thread_root_id, stream_key)) = active {
            start_streaming_agent_text_block(
                pool,
                agent_id,
                channel_id,
                thread_root_id,
                &stream_key,
            )
            .await?;
        }
    }

    if let Some(delta) = claude_text_delta(&value) {
        let (active, first_delta_elapsed) = {
            let mut state = runtime.state.lock().await;
            let (active, first_delta_elapsed) = {
                let Some(active) = state.active.as_mut() else {
                    return Ok(ClaudeLineFlow::Continue);
                };
                let first_delta_elapsed = if active.first_delta_at.is_none() {
                    let elapsed = active.started_at.elapsed();
                    active.first_delta_at = Some(Instant::now());
                    Some(elapsed)
                } else {
                    None
                };
                active.text.push_delta(delta);
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
        return Ok(ClaudeLineFlow::Continue);
    }

    reconcile_final_text(pool, agent_id, runtime, &value).await?;

    // After reconciling, so text completed by this same `assistant` line counts
    // as preceding the tool call.
    if claude_main_tool_use_started(&value) {
        let notes = runtime
            .state
            .lock()
            .await
            .active
            .as_mut()
            .map(|active| active.text.mark_tool_use())
            .unwrap_or_default();
        if let Some(run_id) = active_run_id {
            for note in notes {
                let title: String = note
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(120)
                    .collect();
                record_agent_activity(
                    pool,
                    Some(agent_id),
                    Some(run_id),
                    "thinking",
                    title,
                    truncate_activity_detail(&note),
                )
                .await?;
            }
        }
    }

    Ok(ClaudeLineFlow::Continue)
}

#[cfg(test)]
mod tests {
    use std::{process::Stdio, sync::Arc, time::Instant};

    use sqlx::Row;
    use tokio::{process::Command, sync::Mutex as AsyncMutex};
    use uuid::Uuid;

    use crate::test_support::{
        drop_test_schema, insert_test_agent, insert_test_channel, test_pool,
    };

    use super::{
        configure_claude_process_environment, finish_warm_claude_active_turn, ClaudeActiveTurn,
        ClaudeTextState, WarmClaudeRuntime, CLAUDE_DISABLE_AUTO_MEMORY_ENV,
        CLAUDE_DISABLE_BACKGROUND_TASKS_ENV, CLAUDE_DISABLE_CRON_ENV, CLAUDE_MAX_RETRIES_ENV,
    };

    #[tokio::test]
    async fn claude_child_enforces_turn_boundaries_over_environment_overrides() {
        let mut command = Command::new("sh");
        command.arg("-c").arg(
            r#"printf '%s\n' "$CLAUDE_CODE_DISABLE_BACKGROUND_TASKS" "$CLAUDE_CODE_DISABLE_CRON" "$CLAUDE_CODE_DISABLE_AUTO_MEMORY" "$CLAUDE_CODE_MAX_RETRIES""#,
        );
        for key in [
            CLAUDE_DISABLE_BACKGROUND_TASKS_ENV,
            CLAUDE_DISABLE_CRON_ENV,
            CLAUDE_DISABLE_AUTO_MEMORY_ENV,
            CLAUDE_MAX_RETRIES_ENV,
        ] {
            command.env(key, "0");
        }
        configure_claude_process_environment(&mut command);
        let output = command.output().await.unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"1\n1\n1\n5\n");
    }

    pub(super) async fn test_runtime_with_active_turn(
        run_id: Uuid,
        work_item_id: Uuid,
        channel_id: Uuid,
        stream_key: String,
    ) -> Result<Arc<WarmClaudeRuntime>, String> {
        // This fixture exercises turn finalization, not process-group signaling. Keep the
        // placeholder process detached from the runtime pid so the test cannot signal its
        // surrounding test runner process group on CI.
        let mut command = Command::new("true");
        command.stdin(Stdio::piped());
        let mut child = command.spawn().map_err(|err| err.to_string())?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "test process stdin unavailable".to_owned())?;
        Ok(Arc::new(WarmClaudeRuntime {
            stdin: AsyncMutex::new(stdin),
            state: AsyncMutex::new(super::WarmClaudeState {
                alive: true,
                active: Some(ClaudeActiveTurn {
                    run_id,
                    started_at: Instant::now(),
                    first_delta_at: None,
                    work_item_id: Some(work_item_id),
                    channel_id: Some(channel_id),
                    thread_root_id: None,
                    stream_key,
                    pending_memory_context: None,
                    text: ClaudeTextState::default(),
                    interrupt_requested: false,
                    auth_required: false,
                    network_failure: false,
                }),
                session_id: Some("test-claude-session".to_owned()),
                context_tokens: 0,
                pending_rotation_marker: None,
                last_surface: None,
                injected_memory_context: None,
                last_activity: Instant::now(),
                pending_steers: Vec::new(),
            }),
            pid: None,
            environment_variables: String::new(),
            provider: super::ClaudeCli,
        }))
    }

    #[tokio::test]
    async fn failed_warm_claude_turn_keeps_agent_routable_and_stops_runtime() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "claude-failure-agent").await?;
            sqlx::query("update agents set status = 'running', runtime = 'claude' where id = $1")
                .bind(agent_id)
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            let channel_id = insert_test_channel(&pool, "claude-failure-channel").await?;
            let work_item_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_work_items (agent_id, channel_id, title, status)
                values ($1, $2, 'warm claude failure', 'running')
                returning id
                "#,
            )
            .bind(agent_id)
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let run_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_runs (agent_id, work_item_id, command, status)
                values ($1, $2, 'claude stream-json', 'running')
                returning id
                "#,
            )
            .bind(agent_id)
            .bind(work_item_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let stream_key = format!("{run_id}:warm-claude");
            sqlx::query(
                r#"
                insert into messages (
                    channel_id, sender_agent_id, sender_name, sender_role,
                    body, delivery_state, stream_key
                )
                values ($1, $2, 'claude-failure-agent', 'agent', 'partial', 'streaming', $3)
                "#,
            )
            .bind(channel_id)
            .bind(agent_id)
            .bind(&stream_key)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let runtime =
                test_runtime_with_active_turn(run_id, work_item_id, channel_id, stream_key.clone())
                    .await?;
            finish_warm_claude_active_turn(
                &pool,
                agent_id,
                &runtime,
                false,
                Some("Not logged in · Please run /login".to_owned()),
            )
            .await?;

            let agent_status: String =
                sqlx::query_scalar("select status from agents where id = $1")
                    .bind(agent_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(agent_status, "idle");

            let run = sqlx::query("select status, log, stopped_at from agent_runs where id = $1")
                .bind(run_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
            assert_eq!(run.get::<String, _>("status"), "failed");
            assert!(run
                .get::<String, _>("log")
                .contains("Not logged in · Please run /login"));
            assert!(run.get::<Option<String>, _>("stopped_at").is_some());

            let work_status: String =
                sqlx::query_scalar("select status from agent_work_items where id = $1")
                    .bind(work_item_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(work_status, "failed");

            let delivery_state: String =
                sqlx::query_scalar("select delivery_state from messages where stream_key = $1")
                    .bind(&stream_key)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(delivery_state, "error");

            let runtime_status: String = sqlx::query_scalar(
                "select status from runtime_sessions where agent_id = $1 and runtime = 'claude'",
            )
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(runtime_status, "stopped");
            assert!(!runtime.state.lock().await.alive);

            let run_error_count: i64 = sqlx::query_scalar(
                "select count(*) from agent_activities where agent_id = $1 and run_id = $2 and kind = 'run_error'",
            )
            .bind(agent_id)
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(run_error_count, 1);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }

    async fn insert_running_turn_fixture(
        pool: &sqlx::SqlitePool,
        handle: &str,
        runtime: &str,
    ) -> Result<(Uuid, Uuid, Uuid, Uuid, String), String> {
        let agent_id = insert_test_agent(pool, handle).await?;
        sqlx::query("update agents set status = 'running', runtime = $2 where id = $1")
            .bind(agent_id)
            .bind(runtime)
            .execute(pool)
            .await
            .map_err(|err| err.to_string())?;
        let channel_id = insert_test_channel(pool, &format!("{handle}-channel")).await?;
        let work_item_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_work_items (agent_id, channel_id, title, status)
            values ($1, $2, 'daily memory', 'running')
            returning id
            "#,
        )
        .bind(agent_id)
        .bind(channel_id)
        .fetch_one(pool)
        .await
        .map_err(|err| err.to_string())?;
        let run_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_runs (agent_id, work_item_id, command, status)
            values ($1, $2, 'warm runtime', 'running')
            returning id
            "#,
        )
        .bind(agent_id)
        .bind(work_item_id)
        .fetch_one(pool)
        .await
        .map_err(|err| err.to_string())?;
        sqlx::query("update agent_work_items set run_id = $2 where id = $1")
            .bind(work_item_id)
            .bind(run_id)
            .execute(pool)
            .await
            .map_err(|err| err.to_string())?;
        let stream_key = format!("{run_id}:warm");
        sqlx::query(
            r#"
            insert into messages (
                channel_id, sender_agent_id, sender_name, sender_role,
                body, delivery_state, stream_key
            )
            values ($1, $2, $3, 'agent', '', 'streaming', $4)
            "#,
        )
        .bind(channel_id)
        .bind(agent_id)
        .bind(handle)
        .bind(&stream_key)
        .execute(pool)
        .await
        .map_err(|err| err.to_string())?;
        Ok((agent_id, channel_id, work_item_id, run_id, stream_key))
    }

    /// Feeds `lines` through the stdout handler and reports whether the turn
    /// ended as a login failure: request requeued and Claude paused with a card.
    async fn replay_lines_and_read_login_state(
        pool: &sqlx::SqlitePool,
        handle: &str,
        lines: &[&str],
    ) -> Result<(String, Option<String>, i64), String> {
        let (agent_id, channel_id, work_item_id, run_id, stream_key) =
            insert_running_turn_fixture(pool, handle, "claude").await?;
        let runtime =
            test_runtime_with_active_turn(run_id, work_item_id, channel_id, stream_key).await?;
        for line in lines {
            super::handle_claude_warm_stdout_line(pool, agent_id, &runtime, line).await?;
        }
        assert!(runtime.state.lock().await.active.is_none());
        let work_status: String =
            sqlx::query_scalar("select status from agent_work_items where id = $1")
                .bind(work_item_id)
                .fetch_one(pool)
                .await
                .map_err(|err| err.to_string())?;
        let auth: Option<String> =
            sqlx::query_scalar("select status from provider_auth_state where runtime = 'claude'")
                .fetch_optional(pool)
                .await
                .map_err(|err| err.to_string())?;
        let cards: i64 = sqlx::query_scalar(
            "select count(*) from decisions where status = 'open' and requester_agent_id is null",
        )
        .fetch_one(pool)
        .await
        .map_err(|err| err.to_string())?;
        Ok((work_status, auth, cards))
    }

    const RETRY_401: &str = r#"{"type":"system","subtype":"api_retry","attempt":1,"max_retries":5,"retry_delay_ms":538,"error_status":401,"error":"authentication_failed","session_id":"s1","uuid":"u1"}"#;
    const RETRY_UNKNOWN: &str = r#"{"type":"system","subtype":"api_retry","attempt":2,"max_retries":5,"retry_delay_ms":1243,"error_status":null,"error":"unknown","session_id":"s1","uuid":"u2"}"#;
    const ASLEEP_REPLY: &str = r#"{"type":"assistant","message":{"id":"m1","model":"<synthetic>","role":"assistant","type":"message","content":[{"type":"text","text":"API Error: Connection lost while your computer was asleep"}]},"session_id":"s1","uuid":"u3","error":"server_error","is_api_error_message":true}"#;
    const ASLEEP_RESULT: &str = r#"{"type":"result","subtype":"success","is_error":true,"api_error_status":null,"terminal_reason":"api_error","result":"API Error: Connection lost while your computer was asleep","session_id":"s1","uuid":"u4"}"#;
    const LOGIN_REPLY: &str = r#"{"type":"assistant","message":{"id":"m2","model":"<synthetic>","role":"assistant","type":"message","content":[{"type":"text","text":"Failed to authenticate: OAuth session expired and could not be refreshed"}]},"session_id":"s1","uuid":"u5","error":"authentication_failed","is_api_error_message":true}"#;
    const LOGIN_RESULT: &str = r#"{"type":"result","subtype":"success","is_error":true,"api_error_status":null,"terminal_reason":"api_error","result":"Failed to authenticate: OAuth session expired and could not be refreshed","session_id":"s1","uuid":"u6"}"#;

    #[tokio::test]
    async fn a_401_retry_followed_by_a_lost_connection_is_not_a_login_failure() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            // The lines of run ef50ed4e: the laptop slept through the turn.
            let (work_status, auth, cards) = replay_lines_and_read_login_state(
                &pool,
                "claude-asleep-agent",
                &[RETRY_401, RETRY_UNKNOWN, ASLEEP_REPLY, ASLEEP_RESULT],
            )
            .await?;
            assert_eq!(auth, None, "a retry event must not pause Claude");
            assert_eq!(cards, 0, "a retry event must not raise the login card");
            assert_eq!(
                work_status, "queued",
                "it waits as a lost connection instead"
            );
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn a_401_retry_that_ends_in_a_real_login_failure_still_pauses_claude() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let (work_status, auth, cards) = replay_lines_and_read_login_state(
                &pool,
                "claude-real-login-agent",
                &[RETRY_401, LOGIN_REPLY, LOGIN_RESULT],
            )
            .await?;
            assert_eq!(auth.as_deref(), Some("needs_login"));
            assert_eq!(cards, 1);
            assert_eq!(work_status, "queued");
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn only_the_last_provider_error_of_a_turn_decides_a_login_failure() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let (work_status, auth, cards) = replay_lines_and_read_login_state(
                &pool,
                "claude-recovered-login-agent",
                &[LOGIN_REPLY, ASLEEP_REPLY, ASLEEP_RESULT],
            )
            .await?;
            assert_eq!(auth, None);
            assert_eq!(cards, 0);
            assert_eq!(work_status, "queued");
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn expired_claude_login_requeues_the_request_and_pauses_claude() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let (agent_id, channel_id, work_item_id, run_id, stream_key) =
                insert_running_turn_fixture(&pool, "claude-login-agent", "claude").await?;
            let runtime =
                test_runtime_with_active_turn(run_id, work_item_id, channel_id, stream_key.clone())
                    .await?;
            // The lines Claude Code 2.1 prints when its OAuth session expired.
            for line in [
                r#"{"type":"assistant","message":{"id":"m1","model":"<synthetic>","role":"assistant","type":"message","content":[{"type":"text","text":"Failed to authenticate: OAuth session expired and could not be refreshed"}]},"session_id":"s1","uuid":"u1","error":"authentication_failed","is_api_error_message":true}"#,
                r#"{"type":"result","subtype":"success","is_error":true,"api_error_status":null,"terminal_reason":"api_error","result":"Failed to authenticate: OAuth session expired and could not be refreshed","session_id":"s1","uuid":"u2"}"#,
            ] {
                super::handle_claude_warm_stdout_line(&pool, agent_id, &runtime, line).await?;
            }

            let work = sqlx::query("select status, run_id, completed_at from agent_work_items where id = $1")
                .bind(work_item_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
            assert_eq!(work.get::<String, _>("status"), "queued");
            assert!(work.get::<Option<Uuid>, _>("run_id").is_none());
            assert!(work.get::<Option<String>, _>("completed_at").is_none());
            let run_status: String = sqlx::query_scalar("select status from agent_runs where id = $1")
                .bind(run_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
            assert_eq!(run_status, "failed");
            let replies: i64 = sqlx::query_scalar("select count(*) from messages where stream_key = $1")
                .bind(&stream_key)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
            assert_eq!(replies, 0, "the CLI's error text is not posted as a reply");
            let auth: String =
                sqlx::query_scalar("select status from provider_auth_state where runtime = 'claude'")
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(auth, "needs_login");
            let cards: i64 = sqlx::query_scalar(
                "select count(*) from decisions where status = 'open' and requester_agent_id is null",
            )
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(cards, 1);
            assert!(runtime.state.lock().await.active.is_none());
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }
}
