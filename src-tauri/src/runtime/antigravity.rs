//! Google Antigravity's `agy` CLI as a warm runtime, through its headless
//! stream-json mode.
//!
//! One `agy` process per agent answers turn after turn in one conversation,
//! and a respawn resumes it with `--conversation`. The protocol has no way to
//! add input to a running turn or to interrupt one, so a follow-up waits for
//! the turn to end and a stop ends the process. Lantor's standing prompt is
//! the workspace's `GEMINI.md`, which `agy` loads on every start.

use std::{process::Stdio, sync::Arc, time::Duration, time::Instant};

use serde_json::Value;
use sqlx::{Row, SqlitePool};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    process::Command,
    sync::Mutex as AsyncMutex,
    time::sleep,
};
use uuid::Uuid;

use crate::agent_environment::apply_agent_environment_variables;
use crate::app::{to_string, CommandResult};
use crate::events::activity::{
    record_agent_activity, record_agent_activity_throttled, record_run_tool_call, RunToolCall,
};
use crate::freshness::advance_agent_target_watermark_for_work_item;
use crate::prompts::{
    antigravity_standing_prompt, build_antigravity_streaming_prompt, prepend_memory_context,
    write_antigravity_instructions,
};
use crate::run_log::append_run_log;
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
    session_context::{forget_session_context, load_injected_memory, SurfaceDelivery},
    streaming::{
        append_streaming_agent_message, ensure_streaming_agent_message,
        reconcile_streaming_agent_message, start_streaming_agent_text_block,
    },
    warm::{
        reusable_runtime, stop_idle_process, wait_for_warm_process, warm_stderr_reader,
        WarmProvider, WarmRegistry, WarmRuntime, WarmState,
    },
};
use crate::ui_notifications::reconcile_work_item_change;
use crate::usage::{record_turn_usage, RunUsage};

mod protocol;
mod turn;

use protocol::{
    agy_command_text, agy_init_conversation_id, agy_step_usage, agy_stream_key,
    agy_surface_boundary_marker, agy_text_delta, agy_tool_started, agy_turn_result, agy_user_input,
    agy_write_input, AGY_STREAM_ARGS,
};
use turn::finish_warm_antigravity_turn;

pub(crate) const ANTIGRAVITY_RUNTIME: &str = "antigravity";
const AGY_COMMAND: &str = "agy";
const ANTIGRAVITY_IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const ANTIGRAVITY_IDLE_REAPER_INTERVAL: Duration = Duration::from_secs(30);

pub(crate) struct AntigravityCli;

pub(crate) type WarmAntigravityRegistry = WarmRegistry<AntigravityCli>;
type WarmAntigravityRuntime = WarmRuntime<AntigravityCli>;

impl WarmProvider for AntigravityCli {
    type State = WarmAntigravityState;
    const RUNTIME: &'static str = ANTIGRAVITY_RUNTIME;
    const PROCESS_TITLE: &'static str = "Antigravity warm stream-json";
    const PROCESS_NAME: &'static str = "agy stream-json";

    fn session_thread_id(&self) -> Option<&str> {
        None
    }

    async fn finish_turn_after_exit(
        pool: &SqlitePool,
        agent_id: Uuid,
        runtime: &Arc<WarmAntigravityRuntime>,
        detail: String,
    ) {
        let _ = finish_warm_antigravity_turn(pool, agent_id, runtime, false, Some(detail)).await;
    }
}

pub(crate) struct WarmAntigravityState {
    alive: bool,
    active: Option<AntigravityActiveTurn>,
    conversation_id: Option<String>,
    /// The conversation this process was started to resume, until `init`
    /// confirms which one it runs: `agy` starts a new one, with only a
    /// warning on stderr, when it no longer has the old one.
    resuming_conversation_id: Option<String>,
    /// The standing prompt could not be written to `GEMINI.md`, so the first
    /// turn of a new conversation carries it instead.
    standing_prompt_in_first_turn: bool,
    last_surface: Option<AntigravitySurface>,
    injected_memory_context: Option<String>,
    last_activity: Instant,
}

impl WarmState for WarmAntigravityState {
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
        self.active.is_some()
    }
}

struct AntigravityActiveTurn {
    run_id: Uuid,
    started_at: Instant,
    first_delta_at: Option<Instant>,
    work_item_id: Option<Uuid>,
    channel_id: Option<Uuid>,
    thread_root_id: Option<Uuid>,
    stream_key: String,
    pending_memory_context: Option<Option<String>>,
    /// Highest surface message seq this turn's prompt delivers. The
    /// conversation holds it once the turn succeeds.
    surface_delivered_seq: Option<i64>,
    /// Everything streamed into the reply so far.
    streamed_text: String,
    /// The `agent_response` step the reply's last text came from.
    text_step: Option<i64>,
    /// Text of the current step, not yet followed by a tool call.
    step_text: String,
    /// Text was written before a tool call, so the reply holds progress notes
    /// that the turn's final answer replaces.
    has_interim_text: bool,
    usage: RunUsage,
    context_tokens: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AntigravitySurface {
    channel_id: Option<Uuid>,
    thread_root_id: Option<Uuid>,
}

#[derive(Clone, Copy)]
struct AntigravityRuntimeConfig<'a> {
    handle: &'a str,
    model: &'a str,
    working_directory: &'a str,
    environment_variables: &'a str,
}

async fn get_or_spawn_warm_antigravity_runtime(
    pool: &SqlitePool,
    registry: &WarmAntigravityRegistry,
    agent_id: Uuid,
    config: AntigravityRuntimeConfig<'_>,
) -> CommandResult<Arc<WarmAntigravityRuntime>> {
    if let Some(runtime) =
        reusable_runtime(pool, registry, agent_id, config.environment_variables).await?
    {
        return Ok(runtime);
    }
    let runtime = spawn_warm_antigravity_runtime(pool, registry.clone(), agent_id, config).await?;
    registry.insert(agent_id, runtime.clone()).await;
    Ok(runtime)
}

/// The conversation an earlier process left, unless that process never got
/// past its `pid:` placeholder.
async fn stored_conversation_id(
    pool: &SqlitePool,
    agent_id: Uuid,
) -> CommandResult<Option<String>> {
    let stored: Option<String> = sqlx::query_scalar(
        "select provider_thread_id from runtime_sessions where agent_id = $1 and runtime = $2",
    )
    .bind(agent_id)
    .bind(ANTIGRAVITY_RUNTIME)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;
    Ok(stored
        .map(|id| id.trim().to_owned())
        .filter(|id| Uuid::parse_str(id).is_ok()))
}

/// The surface the agent's most recent finished run answered, so a resumed
/// conversation still gets a thread-boundary marker when the surface moves.
async fn last_antigravity_surface(
    pool: &SqlitePool,
    agent_id: Uuid,
) -> CommandResult<Option<AntigravitySurface>> {
    let row = sqlx::query(
        r#"
        select w.channel_id, w.thread_root_id
        from agent_runs r
        left join agent_work_items w on w.id = r.work_item_id
        where r.agent_id = $1 and r.stopped_at is not null
        order by r.stopped_at desc
        limit 1
        "#,
    )
    .bind(agent_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;
    Ok(row.map(|row| AntigravitySurface {
        channel_id: row.get("channel_id"),
        thread_root_id: row.get("thread_root_id"),
    }))
}

async fn spawn_warm_antigravity_runtime(
    pool: &SqlitePool,
    registry: WarmAntigravityRegistry,
    agent_id: Uuid,
    config: AntigravityRuntimeConfig<'_>,
) -> CommandResult<Arc<WarmAntigravityRuntime>> {
    // Read before this spawn overwrites the stored id with a pid placeholder.
    let conversation_id = stored_conversation_id(pool, agent_id).await?;
    let standing_prompt_in_file =
        match write_antigravity_instructions(config.working_directory, config.handle) {
            Ok(written) => written,
            Err(err) => {
                record_agent_activity(
                    pool,
                    Some(agent_id),
                    None,
                    "warning",
                    "Could not write GEMINI.md",
                    format!("{err}; the standing prompt goes into the first turn instead"),
                )
                .await?;
                false
            }
        };

    let mut command = Command::new(AGY_COMMAND);
    command.args(AGY_STREAM_ARGS);
    if !config.model.is_empty() {
        command.arg("--model").arg(config.model);
    }
    if let Some(conversation_id) = &conversation_id {
        command.arg("--conversation").arg(conversation_id);
    }
    command.arg("-p=");
    apply_agent_environment_variables(&mut command, config.environment_variables)?;
    configure_agent_identity_env(&mut command, agent_id, config.handle);
    configure_agent_context_tool_env(&mut command);
    #[cfg(unix)]
    command.process_group(0);
    if !config.working_directory.is_empty() {
        command.current_dir(config.working_directory);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            mark_agent_runtime_failed(pool, agent_id).await?;
            record_agent_activity(
                pool,
                Some(agent_id),
                None,
                "run_error",
                "Antigravity warm stream-json failed to start",
                err.to_string(),
            )
            .await?;
            return Err(err.to_string());
        }
    };
    let pid = child.id().map(|id| id as i32);
    let Some(stdin) = child.stdin.take() else {
        return Err("agy stream-json stdin unavailable".to_owned());
    };
    let Some(stdout) = child.stdout.take() else {
        return Err("agy stream-json stdout unavailable".to_owned());
    };
    let stderr = child.stderr.take();

    // A resumed conversation still holds the memory snapshot an earlier
    // process gave it.
    let injected_memory_context = match &conversation_id {
        Some(id) => load_injected_memory(pool, agent_id, ANTIGRAVITY_RUNTIME, id).await?,
        None => None,
    };
    let last_surface = match &conversation_id {
        Some(_) => last_antigravity_surface(pool, agent_id).await?,
        None => None,
    };
    let runtime = Arc::new(WarmAntigravityRuntime {
        stdin: AsyncMutex::new(stdin),
        state: AsyncMutex::new(WarmAntigravityState {
            alive: true,
            active: None,
            conversation_id: conversation_id.clone(),
            resuming_conversation_id: conversation_id.clone(),
            standing_prompt_in_first_turn: !standing_prompt_in_file && conversation_id.is_none(),
            last_surface,
            injected_memory_context,
            last_activity: Instant::now(),
        }),
        pid,
        environment_variables: config.environment_variables.to_owned(),
        provider: AntigravityCli,
    });

    let provider_thread_id = conversation_id.clone().unwrap_or_else(|| {
        pid.map(|pid| format!("pid:{pid}"))
            .unwrap_or_else(|| "warming".to_owned())
    });
    upsert_runtime_thread_id(
        pool,
        agent_id,
        ANTIGRAVITY_RUNTIME,
        &provider_thread_id,
        "idle",
    )
    .await?;
    record_agent_activity(
        pool,
        Some(agent_id),
        None,
        "run",
        "Antigravity warm stream-json ready",
        format!(
            "{}, {}",
            pid.map(|pid| format!("pid={pid}"))
                .unwrap_or_else(|| "pid unavailable".to_owned()),
            conversation_id
                .as_deref()
                .map(|id| format!("resumed conversation {id}"))
                .unwrap_or_else(|| "new conversation".to_owned())
        ),
    )
    .await?;

    tokio::spawn(antigravity_stdout_reader(
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
    tokio::spawn(antigravity_idle_reaper(
        pool.clone(),
        agent_id,
        runtime.clone(),
    ));
    Ok(runtime)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn supervisor_start_antigravity_streaming_agent(
    pool: &SqlitePool,
    registry: &WarmAntigravityRegistry,
    agent_id: Uuid,
    work_item_id: Option<Uuid>,
    handle: String,
    model: String,
    working_directory: String,
    environment_variables: String,
    work_item_prompt: String,
    rendered_context_max_seq: i64,
    surface_delivery: SurfaceDelivery,
    memory_context: Option<String>,
) -> CommandResult<()> {
    let prompt = build_antigravity_streaming_prompt(&work_item_prompt);
    let model = model.trim().to_owned();
    let command_text = agy_command_text(&model);
    let runtime = match get_or_spawn_warm_antigravity_runtime(
        pool,
        registry,
        agent_id,
        AntigravityRuntimeConfig {
            handle: &handle,
            model: &model,
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
            return Err("Antigravity warm runtime is not alive".to_owned());
        }
        if state.active.is_some() {
            // agy takes a new prompt only after the turn's result; the request
            // stays queued and starts once the agent is idle.
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

    let (channel_id, thread_root_id) = match work_item_id {
        Some(work_item_id) => {
            let row = sqlx::query(
                "select channel_id, thread_root_id from agent_work_items where id = $1 and agent_id = $2",
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
        }
        None => (None, None),
    };
    let current_surface = AntigravitySurface {
        channel_id,
        thread_root_id,
    };
    let (surface_boundary, memory_update, conversation_id, standing_prompt) = {
        let mut state = runtime.state.lock().await;
        let standing_prompt = std::mem::take(&mut state.standing_prompt_in_first_turn)
            .then(|| antigravity_standing_prompt(&handle));
        (
            agy_surface_boundary_marker(state.last_surface, current_surface).unwrap_or_default(),
            memory_context_update(
                state.injected_memory_context.as_deref(),
                memory_context.as_deref(),
            ),
            state.conversation_id.clone(),
            standing_prompt,
        )
    };
    // The conversation this process resumed may already hold the surface's
    // earlier messages; only that conversation gets the prompt without them.
    let prompt = match surface_delivery.delta_prompt_for(conversation_id.as_deref()) {
        Some(delta_prompt) => build_antigravity_streaming_prompt(delta_prompt),
        None => prompt,
    };
    let surface_delivered_seq =
        (surface_delivery.delivered_seq > 0).then_some(surface_delivery.delivered_seq);
    let prompt = format!("{surface_boundary}{prompt}");
    let (turn_prompt, next_injected_memory_context) = match memory_update {
        Some((memory_prompt, next_memory_context)) => (
            prepend_memory_context(prompt.clone(), Some(&memory_prompt)),
            Some(next_memory_context),
        ),
        None => (prompt.clone(), None),
    };
    let turn_prompt = match standing_prompt {
        Some(standing_prompt) => format!("{standing_prompt}\n\n{turn_prompt}"),
        None => turn_prompt,
    };

    let initial_log =
        format!("$ {command_text}\n[warm process reused]\n\n[streaming agent request]\n{prompt}\n");
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

    let stream_key = agy_stream_key(run_id);
    if let Some(channel_id) = channel_id {
        if let Err(err) =
            ensure_streaming_agent_message(pool, agent_id, channel_id, thread_root_id, &stream_key)
                .await
        {
            cleanup_failed_warm_start(
                pool,
                ANTIGRAVITY_RUNTIME,
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
    const BUSY_BEFORE_TURN_START: &str = "Antigravity warm runtime became busy before turn start";
    let occupied = {
        let mut state = runtime.state.lock().await;
        if !state.alive {
            Err("Antigravity warm runtime exited before turn start".to_owned())
        } else if state.active.is_some() {
            Err(BUSY_BEFORE_TURN_START.to_owned())
        } else {
            state.last_activity = Instant::now();
            state.active = Some(AntigravityActiveTurn {
                run_id,
                started_at: Instant::now(),
                first_delta_at: None,
                work_item_id,
                channel_id,
                thread_root_id,
                stream_key,
                pending_memory_context: next_injected_memory_context,
                surface_delivered_seq,
                streamed_text: String::new(),
                text_step: None,
                step_text: String::new(),
                has_interim_text: false,
                usage: RunUsage::default(),
                context_tokens: 0,
            });
            Ok(())
        }
    };
    if let Err(err) = occupied {
        cleanup_failed_warm_start(
            pool,
            ANTIGRAVITY_RUNTIME,
            agent_id,
            run_id,
            work_item_id,
            &err,
            if err == BUSY_BEFORE_TURN_START {
                WarmStartFailure::RuntimeBusy
            } else {
                WarmStartFailure::Transient
            },
        )
        .await?;
        return Err(err);
    }

    let written = {
        let mut stdin = runtime.stdin.lock().await;
        agy_write_input(&mut stdin, agy_user_input(&turn_prompt)).await
    };
    if let Err(err) = written {
        finish_warm_antigravity_turn(pool, agent_id, &runtime, false, Some(err.clone())).await?;
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

async fn antigravity_stdout_reader<R>(
    pool: SqlitePool,
    registry: WarmAntigravityRegistry,
    agent_id: Uuid,
    runtime: Arc<WarmAntigravityRuntime>,
    stream: R,
) where
    R: AsyncRead + Unpin,
{
    let mut lines = BufReader::new(stream).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if let Err(err) =
                    handle_antigravity_stdout_line(&pool, agent_id, &runtime, &line).await
                {
                    let _ = record_agent_activity(
                        &pool,
                        Some(agent_id),
                        None,
                        "run_error",
                        "Antigravity stream event failed",
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
                    "agy stdout read failed",
                    err.to_string(),
                )
                .await;
                break;
            }
        }
    }
    runtime.state.lock().await.alive = false;
    let _ = finish_warm_antigravity_turn(
        &pool,
        agent_id,
        &runtime,
        false,
        Some("stdout closed".to_owned()),
    )
    .await;
    registry.remove_if_same(agent_id, &runtime).await;
}

async fn handle_antigravity_stdout_line(
    pool: &SqlitePool,
    agent_id: Uuid,
    runtime: &Arc<WarmAntigravityRuntime>,
    line: &str,
) -> CommandResult<()> {
    let active_run_id = runtime.state.lock().await.active_run_id();
    if let Some(run_id) = active_run_id {
        append_run_log(pool, run_id, format!("[agy] {line}\n")).await?;
    }
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return Ok(());
    };
    let applied = apply_antigravity_event(pool, agent_id, runtime, active_run_id, &value).await;
    // A `result` ends the turn even when recording its effects failed;
    // otherwise the turn would stay active forever and block the agent.
    let Some(result) = agy_turn_result(&value) else {
        return applied;
    };
    let finished = if result.success {
        let reconciled = if applied.is_ok() {
            settle_final_reply(pool, agent_id, runtime, &result.response).await
        } else {
            Ok(())
        };
        reconciled.and(finish_warm_antigravity_turn(pool, agent_id, runtime, true, None).await)
    } else {
        finish_warm_antigravity_turn(pool, agent_id, runtime, false, result.error).await
    };
    applied.and(finished)
}

async fn apply_antigravity_event(
    pool: &SqlitePool,
    agent_id: Uuid,
    runtime: &Arc<WarmAntigravityRuntime>,
    active_run_id: Option<Uuid>,
    value: &Value,
) -> CommandResult<()> {
    if let Some(conversation_id) = agy_init_conversation_id(value) {
        note_conversation(pool, agent_id, runtime, active_run_id, conversation_id).await?;
        return Ok(());
    }
    let Some(run_id) = active_run_id else {
        return Ok(());
    };
    if let Some((request, context_tokens)) = agy_step_usage(value) {
        record_usage(
            pool,
            agent_id,
            runtime,
            run_id,
            request,
            Some(context_tokens),
            false,
        )
        .await;
    }
    if let Some(result) = agy_turn_result(value) {
        if let Some(totals) = result.usage {
            record_usage(pool, agent_id, runtime, run_id, totals, None, true).await;
        }
        return Ok(());
    }
    if let Some((kind, title, detail)) = agy_tool_started(value) {
        let note = runtime
            .state
            .lock()
            .await
            .active
            .as_mut()
            .and_then(|active| {
                let note = std::mem::take(&mut active.step_text);
                (!note.trim().is_empty()).then(|| {
                    active.has_interim_text = true;
                    note
                })
            });
        if let Some(note) = note {
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
        let detail = truncate_activity_detail(&detail);
        match RunToolCall::from_activity_kind(kind) {
            Some(call) => record_run_tool_call(pool, agent_id, run_id, call, title, detail).await?,
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
        return Ok(());
    }
    if let Some((step_index, delta)) = agy_text_delta(value) {
        append_reply_text(pool, agent_id, runtime, step_index, delta).await?;
    }
    Ok(())
}

/// Records the conversation `init` names. When it is not the one this
/// process was started to resume, agy no longer had that one: the new
/// conversation holds nothing Lantor gave the old one.
async fn note_conversation(
    pool: &SqlitePool,
    agent_id: Uuid,
    runtime: &Arc<WarmAntigravityRuntime>,
    active_run_id: Option<Uuid>,
    conversation_id: &str,
) -> CommandResult<()> {
    let lost = {
        let mut state = runtime.state.lock().await;
        let resumed = state.resuming_conversation_id.take();
        let lost = resumed.filter(|resumed| resumed != conversation_id);
        if lost.is_some() {
            state.injected_memory_context = None;
            state.last_surface = None;
            if let Some(active) = state.active.as_mut() {
                active.pending_memory_context = None;
                active.surface_delivered_seq = None;
            }
        }
        state.conversation_id = Some(conversation_id.to_owned());
        state.last_activity = Instant::now();
        lost
    };
    if let Some(lost) = lost {
        forget_session_context(pool, agent_id, ANTIGRAVITY_RUNTIME).await?;
        record_agent_activity(
            pool,
            Some(agent_id),
            active_run_id,
            "warning",
            "Started a new Antigravity conversation",
            format!("agy no longer has conversation {lost}; now {conversation_id}"),
        )
        .await?;
    }
    let status = if active_run_id.is_some() {
        "running"
    } else {
        "idle"
    };
    upsert_runtime_thread_id(pool, agent_id, ANTIGRAVITY_RUNTIME, conversation_id, status).await
}

/// Keeps a run's token counts current. Each finished step adds its request;
/// the turn's `result` brings the turn's totals, which stand when they count
/// at least as much. agy reports no cost, so the run's cost stays zero rather
/// than a guess at a price.
async fn record_usage(
    pool: &SqlitePool,
    agent_id: Uuid,
    runtime: &Arc<WarmAntigravityRuntime>,
    run_id: Uuid,
    usage: RunUsage,
    context_tokens: Option<i64>,
    totals: bool,
) {
    let (usage, context_tokens) = {
        let mut state = runtime.state.lock().await;
        let Some(active) = state
            .active
            .as_mut()
            .filter(|active| active.run_id == run_id)
        else {
            return;
        };
        let counted = |usage: &RunUsage| usage.total_input_tokens() + usage.output_tokens;
        if totals {
            if counted(&usage) >= counted(&active.usage) {
                active.usage = usage;
            }
        } else {
            active.usage.add(&usage);
        }
        if let Some(context_tokens) = context_tokens {
            active.context_tokens = context_tokens;
        }
        (active.usage, active.context_tokens)
    };
    let _ = record_turn_usage(pool, agent_id, run_id, &usage, context_tokens, Some(0)).await;
}

async fn append_reply_text(
    pool: &SqlitePool,
    agent_id: Uuid,
    runtime: &Arc<WarmAntigravityRuntime>,
    step_index: i64,
    delta: &str,
) -> CommandResult<()> {
    let (run_id, channel_id, thread_root_id, stream_key, new_block, first_delta_ms) = {
        let mut state = runtime.state.lock().await;
        state.last_activity = Instant::now();
        let Some(active) = state.active.as_mut() else {
            return Ok(());
        };
        let new_block = active.text_step.is_some_and(|step| step != step_index);
        if active.text_step != Some(step_index) {
            active.text_step = Some(step_index);
            active.step_text.clear();
        }
        let first_delta_ms = active.first_delta_at.is_none().then(|| {
            active.first_delta_at = Some(Instant::now());
            active.started_at.elapsed().as_millis()
        });
        active.step_text.push_str(delta);
        active.streamed_text.push_str(delta);
        (
            active.run_id,
            active.channel_id,
            active.thread_root_id,
            active.stream_key.clone(),
            new_block,
            first_delta_ms,
        )
    };
    if let Some(elapsed) = first_delta_ms {
        record_agent_activity(
            pool,
            Some(agent_id),
            Some(run_id),
            "acting",
            "Responding",
            format!("first_token_ms={elapsed}"),
        )
        .await?;
    }
    let Some(channel_id) = channel_id else {
        return Ok(());
    };
    if new_block {
        start_streaming_agent_text_block(pool, agent_id, channel_id, thread_root_id, &stream_key)
            .await?;
    }
    append_streaming_agent_message(
        pool,
        agent_id,
        channel_id,
        thread_root_id,
        &stream_key,
        delta,
    )
    .await?;
    Ok(())
}

/// The turn's `result` carries the final answer alone. When the streamed
/// reply also holds notes written between tool calls, or misses part of the
/// answer, the reply becomes that answer. Controls in the notes were already
/// delivered live and replay idempotently.
async fn settle_final_reply(
    pool: &SqlitePool,
    agent_id: Uuid,
    runtime: &Arc<WarmAntigravityRuntime>,
    response: &str,
) -> CommandResult<()> {
    if response.trim().is_empty() {
        return Ok(());
    }
    let (channel_id, thread_root_id, stream_key) = {
        let state = runtime.state.lock().await;
        let Some(active) = state.active.as_ref() else {
            return Ok(());
        };
        if !active.has_interim_text && active.streamed_text.trim() == response.trim() {
            return Ok(());
        }
        (
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
        &[response.trim_end().to_owned()],
    )
    .await
}

async fn antigravity_idle_reaper(
    pool: SqlitePool,
    agent_id: Uuid,
    runtime: Arc<WarmAntigravityRuntime>,
) {
    loop {
        sleep(ANTIGRAVITY_IDLE_REAPER_INTERVAL).await;
        let should_stop = {
            let mut state = runtime.state.lock().await;
            let should_stop = state.alive
                && !state.is_busy()
                && state.last_activity.elapsed() >= ANTIGRAVITY_IDLE_TIMEOUT;
            if should_stop {
                state.alive = false;
            }
            should_stop
        };
        if !should_stop {
            if !runtime.state.lock().await.alive {
                return;
            }
            continue;
        }
        stop_idle_process(&pool, agent_id, &runtime, "idle timeout").await;
        return;
    }
}
