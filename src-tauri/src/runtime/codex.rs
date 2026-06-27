use std::{
    collections::{HashMap, HashSet},
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};

use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    process::Command,
    sync::Mutex as AsyncMutex,
};
use uuid::Uuid;

use crate::agent_environment::apply_agent_environment_variables;
use crate::agent_memory::append_run_log;
use crate::events::activity::{record_agent_activity, record_agent_activity_throttled};
use crate::prompts::{build_codex_streaming_prompt, codex_developer_instructions};
use crate::runtime::{
    process::{
        classify_agent_output_activity, configure_agent_context_tool_env,
        configure_agent_identity_env, load_runtime_thread_id, terminate_process_group,
        upsert_runtime_thread_id,
    },
    streaming::{
        adopt_streaming_agent_message_key, append_streaming_agent_message_deferred_completion,
        delete_streaming_agent_message_by_key, ensure_streaming_agent_message,
        streaming_message_body_is_empty,
    },
    surface::{codex_active_turn_schedule_state, same_codex_surface, CodexActiveTurnScheduleState},
};
use crate::ui_notifications::{
    notify_supervisor_wake, notify_ui_agent_run_changed, notify_ui_work_item_changed,
};
use crate::usage::{record_run_usage, usage_from_runtime_event};
use crate::{
    app::{to_string, CommandResult},
    build_steer_followup_prompt, load_inbox_wake_items_for_work_item,
};

mod protocol;
mod reaper;
mod turn;

use protocol::{
    apply_codex_runtime_options, codex_context_rotate_env, codex_context_rotate_input_tokens,
    codex_error_notification_detail, codex_item_id, codex_item_started_activity, codex_item_type,
    codex_model_value, codex_pending_stream_key, codex_request_error, codex_stream_key,
    codex_thread_id_from_response, codex_tool_completion_activity, codex_turn_id_from_value,
    codex_write_json, effective_codex_cwd,
};
use reaper::codex_warm_idle_reaper;
pub(crate) use reaper::reap_stuck_codex_turn;
use turn::{finish_codex_steer_request, finish_warm_codex_active_turn};

const CODEX_TURN_START_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Clone, Default)]
pub(crate) struct WarmCodexRegistry {
    runtimes: Arc<AsyncMutex<HashMap<Uuid, Arc<WarmCodexRuntime>>>>,
}

struct WarmCodexRuntime {
    stdin: AsyncMutex<tokio::process::ChildStdin>,
    state: AsyncMutex<WarmCodexState>,
    thread_id: String,
    pid: Option<i32>,
    environment_variables: String,
}

struct WarmCodexState {
    alive: bool,
    active: Option<CodexActiveTurn>,
    next_request_id: i64,
    pending_rotation_marker: Option<String>,
    last_activity: Instant,
}

struct CodexActiveTurn {
    run_id: Uuid,
    turn_request_id: i64,
    turn_id: Option<String>,
    started_at: Instant,
    // Bumped on every codex stdout line while this turn is active. Used by the
    // idle reaper to detect an in-flight turn that started (got a turn_id) but
    // then went silent and never emitted `turn/completed`, which would
    // otherwise orphan its work_item in `running` forever.
    last_event_at: Instant,
    first_delta_at: Option<Instant>,
    work_item_id: Option<Uuid>,
    channel_id: Option<Uuid>,
    thread_root_id: Option<Uuid>,
    stream_keys: HashSet<String>,
    completed_agent_message_stream_keys: HashSet<String>,
    latest_agent_message_stream_key: Option<String>,
    steer_requests: HashMap<i64, CodexSteerRequest>,
    steer_disabled: bool,
    interrupt_request_id: Option<i64>,
}

struct CodexSteerRequest {
    work_item_id: Uuid,
    run_id: Uuid,
}

struct CodexAgentMessageStream {
    run_id: Uuid,
    channel_id: Option<Uuid>,
    thread_root_id: Option<Uuid>,
    pending_stream_key: String,
    stream_key: String,
    superseded_stream_key: Option<String>,
}

fn track_codex_agent_message_stream(
    active: &mut CodexActiveTurn,
    item_id: &str,
) -> CodexAgentMessageStream {
    let pending_stream_key = codex_pending_stream_key(active.run_id);
    let stream_key = codex_stream_key(active.run_id, item_id);
    let superseded_stream_key = active
        .latest_agent_message_stream_key
        .replace(stream_key.clone())
        .filter(|previous| previous != &stream_key);

    if let Some(previous) = &superseded_stream_key {
        active.stream_keys.remove(previous);
        active.completed_agent_message_stream_keys.remove(previous);
    }
    active.stream_keys.remove(&pending_stream_key);
    active.stream_keys.insert(stream_key.clone());

    CodexAgentMessageStream {
        run_id: active.run_id,
        channel_id: active.channel_id,
        thread_root_id: active.thread_root_id,
        pending_stream_key,
        stream_key,
        superseded_stream_key,
    }
}

async fn codex_context_rotation_candidate(
    pool: &SqlitePool,
    agent_id: Uuid,
    threshold: i64,
) -> CommandResult<Option<(Uuid, i64)>> {
    let row = sqlx::query(
        r#"
        select id, input_tokens
        from agent_runs
        where agent_id = $1
          and stopped_at is not null
        order by stopped_at desc
        limit 1
        "#,
    )
    .bind(agent_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;

    Ok(row.and_then(|row| {
        let input_tokens = row.get("input_tokens");
        (input_tokens >= threshold).then(|| (row.get("id"), input_tokens))
    }))
}

fn codex_rotation_marker(run_id: Uuid, input_tokens: i64, threshold: i64) -> String {
    format!(
        "Lantor rotated away from the previous Codex provider thread after run {run_id} reached {input_tokens} input tokens (threshold {threshold}). If continuity matters for this request, inspect the previous run on demand with:\n\"$LANTOR_CONTEXT_TOOL\" --agent-context-tool run-read --run-id {run_id}"
    )
}

fn prepend_codex_rotation_marker(prompt: &str, marker: &str) -> String {
    let marker = marker.trim();
    if marker.is_empty() {
        return prompt.to_owned();
    }
    if prompt.trim().is_empty() {
        return marker.to_owned();
    }
    format!("{marker}\n\nCurrent Lantor request after context rotation:\n{prompt}")
}

pub(crate) async fn cleanup_failed_warm_codex_start(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
    work_item_id: Option<Uuid>,
    error: &str,
    requeue_work_item: bool,
) -> CommandResult<()> {
    let error_log = format!("codex warm turn failed before start: {error}\n");
    sqlx::query(
        r#"
        update agent_runs
        set status = 'failed',
            exit_code = null,
            log = substr(log || $2, -20000),
            stopped_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1
        "#,
    )
    .bind(run_id)
    .bind(&error_log)
    .execute(pool)
    .await
    .map_err(to_string)?;
    notify_ui_agent_run_changed(pool, run_id, "run_failed").await;

    sqlx::query("update agents set status = $2 where id = $1")
        .bind(agent_id)
        .bind(if requeue_work_item {
            "running"
        } else {
            "error"
        })
        .execute(pool)
        .await
        .map_err(to_string)?;

    if let Some(work_item_id) = work_item_id {
        if requeue_work_item {
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
            .execute(pool)
            .await
            .map_err(to_string)?;
            notify_ui_work_item_changed(pool, work_item_id, "work_item_queued").await;
            let _ = notify_supervisor_wake(pool).await;
        } else {
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
    }

    record_agent_activity(
        pool,
        Some(agent_id),
        Some(run_id),
        if requeue_work_item {
            "dispatch"
        } else {
            "run_error"
        },
        if requeue_work_item {
            "Request requeued"
        } else {
            "Run failed to start"
        },
        error.to_owned(),
    )
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn get_or_spawn_warm_codex_runtime(
    pool: &SqlitePool,
    registry: &WarmCodexRegistry,
    agent_id: Uuid,
    handle: &str,
    model: &str,
    reasoning_effort: &str,
    service_tier: &str,
    working_directory: &str,
    environment_variables: &str,
    memory_context: Option<&str>,
) -> CommandResult<Arc<WarmCodexRuntime>> {
    let context_rotate_threshold = codex_context_rotate_input_tokens();
    let rotation_candidate =
        codex_context_rotation_candidate(pool, agent_id, context_rotate_threshold).await?;
    if let Some(runtime) = {
        let runtimes = registry.runtimes.lock().await;
        runtimes.get(&agent_id).cloned()
    } {
        let mut state = runtime.state.lock().await;
        let environment_changed = runtime.environment_variables != environment_variables;
        if state.alive
            && (rotation_candidate.is_some() || environment_changed)
            && state.active.is_none()
        {
            state.alive = false;
            drop(state);
            if let Some(pid) = runtime.pid {
                let _ = terminate_process_group(pid).await;
            }
            remove_warm_codex_runtime_if_same(registry, agent_id, &runtime).await;
        } else if state.alive {
            drop(state);
            return Ok(runtime);
        } else {
            drop(state);
            registry.runtimes.lock().await.remove(&agent_id);
        }
    }

    let runtime = spawn_warm_codex_runtime(
        pool,
        registry.clone(),
        agent_id,
        handle,
        model,
        reasoning_effort,
        service_tier,
        working_directory,
        environment_variables,
        memory_context,
        context_rotate_threshold,
    )
    .await?;
    registry
        .runtimes
        .lock()
        .await
        .insert(agent_id, runtime.clone());
    Ok(runtime)
}

#[allow(clippy::too_many_arguments)]
async fn spawn_warm_codex_runtime(
    pool: &SqlitePool,
    registry: WarmCodexRegistry,
    agent_id: Uuid,
    handle: &str,
    model: &str,
    reasoning_effort: &str,
    service_tier: &str,
    working_directory: &str,
    environment_variables: &str,
    memory_context: Option<&str>,
    context_rotate_threshold: i64,
) -> CommandResult<Arc<WarmCodexRuntime>> {
    let cwd = effective_codex_cwd(working_directory)?;
    let mut command = Command::new("/bin/zsh");
    command
        .arg("-lc")
        .arg("exec codex app-server --listen stdio:// -c 'notify=[]'");
    apply_agent_environment_variables(&mut command, environment_variables)?;
    configure_agent_identity_env(&mut command, agent_id, handle);
    configure_agent_context_tool_env(&mut command);
    #[cfg(unix)]
    command.process_group(0);
    command.current_dir(&cwd);
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
                "Codex warm app-server failed to start",
                err.to_string(),
            )
            .await?;
            return Err(err.to_string());
        }
    };

    let pid = child.id().map(|id| id as i32);
    let Some(mut stdin) = child.stdin.take() else {
        return Err("codex app-server stdin unavailable".to_owned());
    };
    let Some(stdout) = child.stdout.take() else {
        return Err("codex app-server stdout unavailable".to_owned());
    };
    let stderr = child.stderr.take();
    let mut reader = BufReader::new(stdout);
    let developer_instructions = codex_developer_instructions(handle, memory_context);
    let model_value = codex_model_value(model);
    let mut next_request_id = 1_i64;
    let initialize_id = next_request_id;
    next_request_id += 1;
    let mut thread_request_id = next_request_id;
    next_request_id += 1;

    codex_write_json(
        &mut stdin,
        json!({
            "method": "initialize",
            "id": initialize_id,
            "params": {
                "clientInfo": {
                    "name": "lantor",
                    "title": "Lantor",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": {
                    "experimentalApi": true
                }
            }
        }),
    )
    .await?;
    codex_write_json(&mut stdin, json!({ "method": "initialized" })).await?;

    let rotation_candidate =
        codex_context_rotation_candidate(pool, agent_id, context_rotate_threshold).await?;
    let rotation_marker = if let Some((run_id, input_tokens)) = rotation_candidate {
        Some(codex_rotation_marker(
            run_id,
            input_tokens,
            context_rotate_threshold,
        ))
    } else {
        None
    };
    let existing_thread_id = if rotation_candidate.is_some() {
        None
    } else {
        load_runtime_thread_id(pool, agent_id, "codex").await?
    };
    let mut attempted_resume = existing_thread_id.is_some();
    if let Some((run_id, input_tokens)) = rotation_candidate {
        record_agent_activity(
            pool,
            Some(agent_id),
            None,
            "run",
            "Codex context rotated",
            json!({
                "previous_run_id": run_id,
                "input_tokens": input_tokens,
                "threshold": context_rotate_threshold,
                "env": codex_context_rotate_env()
            })
            .to_string(),
        )
        .await?;
    }
    if let Some(thread_id) = existing_thread_id {
        let mut params = json!({
            "threadId": thread_id.clone(),
            "model": model_value.clone(),
            "cwd": cwd.clone(),
            "approvalPolicy": "never",
            "sandbox": "danger-full-access",
            "developerInstructions": developer_instructions.clone(),
            "persistExtendedHistory": true
        });
        apply_codex_runtime_options(&mut params, reasoning_effort, service_tier);
        codex_write_json(
            &mut stdin,
            json!({
                "method": "thread/resume",
                "id": thread_request_id,
                "params": params
            }),
        )
        .await?;
    } else {
        let mut params = json!({
            "model": model_value.clone(),
            "cwd": cwd.clone(),
            "approvalPolicy": "never",
            "sandbox": "danger-full-access",
            "developerInstructions": developer_instructions.clone(),
            "experimentalRawEvents": false,
            "persistExtendedHistory": true
        });
        apply_codex_runtime_options(&mut params, reasoning_effort, service_tier);
        codex_write_json(
            &mut stdin,
            json!({
                "method": "thread/start",
                "id": thread_request_id,
                "params": params
            }),
        )
        .await?;
    }

    let thread_id = loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line).await.map_err(to_string)?;
        if bytes == 0 {
            return Err("codex app-server closed during warm initialization".to_owned());
        }
        let line = line.trim_end_matches(['\r', '\n']);
        let value: Value = serde_json::from_str(line).map_err(to_string)?;
        if value.get("id").and_then(Value::as_i64) == Some(thread_request_id) {
            if let Some(error) = codex_request_error(&value) {
                if attempted_resume {
                    attempted_resume = false;
                    thread_request_id = next_request_id;
                    next_request_id += 1;
                    let mut params = json!({
                        "model": model_value.clone(),
                        "cwd": cwd.clone(),
                        "approvalPolicy": "never",
                        "sandbox": "danger-full-access",
                        "developerInstructions": developer_instructions.clone(),
                        "experimentalRawEvents": false,
                        "persistExtendedHistory": true
                    });
                    apply_codex_runtime_options(&mut params, reasoning_effort, service_tier);
                    codex_write_json(
                        &mut stdin,
                        json!({
                            "method": "thread/start",
                            "id": thread_request_id,
                            "params": params
                        }),
                    )
                    .await?;
                    record_agent_activity(
                        pool,
                        Some(agent_id),
                        None,
                        "run",
                        "Codex thread resume failed; starting new thread",
                        error,
                    )
                    .await?;
                    continue;
                }
                return Err(format!("codex thread request failed: {error}"));
            }
            let Some(thread_id) = codex_thread_id_from_response(&value) else {
                return Err("codex thread response missing thread id".to_owned());
            };
            break thread_id;
        }
    };

    upsert_runtime_thread_id(pool, agent_id, "codex", &thread_id, "idle").await?;
    record_agent_activity(
        pool,
        Some(agent_id),
        None,
        "run",
        "Codex warm app-server ready",
        pid.map(|pid| format!("pid={pid}, thread_id={thread_id}"))
            .unwrap_or_else(|| format!("thread_id={thread_id}")),
    )
    .await?;

    let runtime = Arc::new(WarmCodexRuntime {
        stdin: AsyncMutex::new(stdin),
        state: AsyncMutex::new(WarmCodexState {
            alive: true,
            active: None,
            next_request_id,
            pending_rotation_marker: rotation_marker,
            last_activity: Instant::now(),
        }),
        thread_id,
        pid,
        environment_variables: environment_variables.to_owned(),
    });

    tokio::spawn(codex_warm_stdout_reader(
        pool.clone(),
        registry.clone(),
        agent_id,
        runtime.clone(),
        reader,
    ));
    if let Some(stderr) = stderr {
        tokio::spawn(codex_warm_stderr_reader(
            pool.clone(),
            agent_id,
            runtime.clone(),
            stderr,
        ));
    }
    tokio::spawn(wait_for_warm_codex_process(
        pool.clone(),
        registry.clone(),
        agent_id,
        runtime.clone(),
        child,
    ));
    tokio::spawn(codex_warm_idle_reaper(
        pool.clone(),
        registry.clone(),
        agent_id,
        runtime.clone(),
    ));

    Ok(runtime)
}

pub(crate) async fn active_codex_turn_surface(
    registry: &WarmCodexRegistry,
    agent_id: Uuid,
) -> Option<(Option<Uuid>, Option<Uuid>, CodexActiveTurnScheduleState)> {
    let runtime = {
        let runtimes = registry.runtimes.lock().await;
        runtimes.get(&agent_id).cloned()
    }?;
    let state = runtime.state.lock().await;
    let active = state.active.as_ref()?;
    Some((
        active.channel_id,
        active.thread_root_id,
        codex_active_turn_schedule_state(
            active.turn_id.as_deref(),
            active.steer_disabled,
            active.started_at.elapsed(),
            CODEX_TURN_START_TIMEOUT,
        ),
    ))
}

pub(crate) async fn interrupt_warm_codex_run(
    pool: &SqlitePool,
    registry: &WarmCodexRegistry,
    agent_id: Uuid,
    run_id: Uuid,
) -> CommandResult<bool> {
    let runtime = {
        let runtimes = registry.runtimes.lock().await;
        runtimes.get(&agent_id).cloned()
    };
    let Some(runtime) = runtime else {
        return Ok(false);
    };
    interrupt_warm_codex_turn(pool, agent_id, &runtime, run_id).await
}

async fn interrupt_warm_codex_turn(
    pool: &SqlitePool,
    agent_id: Uuid,
    runtime: &Arc<WarmCodexRuntime>,
    run_id: Uuid,
) -> CommandResult<bool> {
    let (request_id, turn_id) = {
        let mut state = runtime.state.lock().await;
        let Some(active) = state.active.as_ref() else {
            return Ok(false);
        };
        if active.run_id != run_id {
            return Ok(false);
        }
        let Some(turn_id) = active.turn_id.clone() else {
            return Ok(false);
        };
        if active.interrupt_request_id.is_some() {
            return Ok(true);
        }
        let request_id = state.next_request_id;
        state.next_request_id += 1;
        state
            .active
            .as_mut()
            .expect("active turn checked")
            .interrupt_request_id = Some(request_id);
        state.last_activity = Instant::now();
        (request_id, turn_id)
    };

    {
        let mut stdin = runtime.stdin.lock().await;
        codex_write_json(
            &mut stdin,
            json!({
                "method": "turn/interrupt",
                "id": request_id,
                "params": {
                    "threadId": runtime.thread_id.clone(),
                    "turnId": turn_id
                }
            }),
        )
        .await?;
    }

    append_run_log(
        pool,
        run_id,
        format!("[codex] turn/interrupt requested id={request_id}\n"),
    )
    .await?;
    record_agent_activity(
        pool,
        Some(agent_id),
        Some(run_id),
        "run",
        "Stop requested",
        turn_id,
    )
    .await?;
    Ok(true)
}

async fn steer_warm_codex_turn_if_same_surface(
    pool: &SqlitePool,
    agent_id: Uuid,
    runtime: &Arc<WarmCodexRuntime>,
    work_item_id: Uuid,
    codex_prompt: &str,
) -> CommandResult<bool> {
    let row = sqlx::query(
        r#"
        select channel_id, thread_root_id, source_kind
        from agent_work_items
        where id = $1 and agent_id = $2 and status = 'queued'
        "#,
    )
    .bind(work_item_id)
    .bind(agent_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;
    let Some(row) = row else {
        return Ok(false);
    };
    let channel_id: Option<Uuid> = row.get("channel_id");
    let thread_root_id: Option<Uuid> = row.get("thread_root_id");
    let source_kind: String = row.get("source_kind");

    let (active_run_id, active_channel_id, active_thread_root_id, active_turn_id) = {
        let state = runtime.state.lock().await;
        let Some(active) = state.active.as_ref() else {
            return Ok(false);
        };
        if active.steer_disabled {
            return Ok(false);
        }
        (
            active.run_id,
            active.channel_id,
            active.thread_root_id,
            active.turn_id.clone(),
        )
    };
    if !same_codex_surface(
        pool,
        channel_id,
        thread_root_id,
        active_channel_id,
        active_thread_root_id,
    )
    .await?
    {
        return Ok(false);
    }
    let Some(turn_id) = active_turn_id else {
        return Ok(false);
    };
    let steer_prompt = if source_kind == "inbox_wake" {
        let items = load_inbox_wake_items_for_work_item(pool, work_item_id).await?;
        if items.is_empty() {
            codex_prompt.to_owned()
        } else {
            build_steer_followup_prompt(&items)
        }
    } else {
        codex_prompt.to_owned()
    };

    let request_id = {
        let mut state = runtime.state.lock().await;
        let Some(active) = state.active.as_mut() else {
            return Ok(false);
        };
        if active.steer_disabled {
            return Ok(false);
        }
        if active.run_id != active_run_id || active.turn_id.as_deref() != Some(turn_id.as_str()) {
            return Ok(false);
        }
        if active.channel_id != active_channel_id || active.thread_root_id != active_thread_root_id
        {
            return Ok(false);
        }
        let run_id = active.run_id;
        let request_id = state.next_request_id;
        state.next_request_id += 1;
        state.last_activity = Instant::now();
        state
            .active
            .as_mut()
            .expect("active turn checked")
            .steer_requests
            .insert(
                request_id,
                CodexSteerRequest {
                    work_item_id,
                    run_id,
                },
            );
        request_id
    };

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
    .bind(active_run_id)
    .execute(pool)
    .await
    .map_err(to_string)?;
    notify_ui_work_item_changed(pool, work_item_id, "work_item_running").await;

    let write_result = {
        let mut stdin = runtime.stdin.lock().await;
        codex_write_json(
            &mut stdin,
            json!({
                "method": "turn/steer",
                "id": request_id,
                "params": {
                    "threadId": runtime.thread_id.clone(),
                    "expectedTurnId": turn_id,
                    "input": [{
                        "type": "text",
                        "text": steer_prompt,
                        "text_elements": []
                    }]
                }
            }),
        )
        .await
    };

    if let Err(err) = write_result {
        {
            let mut state = runtime.state.lock().await;
            if let Some(active) = state.active.as_mut() {
                active.steer_requests.remove(&request_id);
            }
        }
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
        .execute(pool)
        .await
        .map_err(to_string)?;
        notify_ui_work_item_changed(pool, work_item_id, "work_item_queued").await;
        return Err(err);
    }

    append_run_log(
        pool,
        active_run_id,
        format!("[codex] turn/steer work_item={work_item_id}\n"),
    )
    .await?;
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn supervisor_start_codex_streaming_agent(
    pool: &SqlitePool,
    codex_registry: &WarmCodexRegistry,
    agent_id: Uuid,
    work_item_id: Option<Uuid>,
    handle: String,
    model: String,
    reasoning_effort: String,
    service_tier: String,
    working_directory: String,
    environment_variables: String,
    work_item_prompt: String,
    memory_context: Option<String>,
) -> CommandResult<()> {
    let command_text = "codex app-server --listen stdio://".to_owned();
    let codex_prompt = build_codex_streaming_prompt(&work_item_prompt);
    let runtime = get_or_spawn_warm_codex_runtime(
        pool,
        codex_registry,
        agent_id,
        &handle,
        &model,
        &reasoning_effort,
        &service_tier,
        &working_directory,
        &environment_variables,
        memory_context.as_deref(),
    )
    .await?;

    {
        let state = runtime.state.lock().await;
        if !state.alive {
            return Err("codex warm runtime is not alive".to_owned());
        }
        if state.active.is_some() {
            drop(state);
            if let Some(work_item_id) = work_item_id {
                if steer_warm_codex_turn_if_same_surface(
                    pool,
                    agent_id,
                    &runtime,
                    work_item_id,
                    &codex_prompt,
                )
                .await?
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

    let codex_prompt = {
        let mut state = runtime.state.lock().await;
        match state.pending_rotation_marker.take() {
            Some(marker) => prepend_codex_rotation_marker(&codex_prompt, &marker),
            None => codex_prompt,
        }
    };

    let initial_log = if codex_prompt.is_empty() {
        format!("$ {command_text}\n[warm process reused]\n")
    } else {
        format!(
            "$ {command_text}\n[warm process reused]\n\n[streaming agent request]\n{codex_prompt}\n"
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
        (
            row.get::<Option<Uuid>, _>("channel_id"),
            row.get::<Option<Uuid>, _>("thread_root_id"),
        )
    } else {
        (None, None)
    };

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
            .map(|pid| format!("pid={pid}, thread_id={}", runtime.thread_id))
            .unwrap_or_else(|| format!("thread_id={}", runtime.thread_id)),
    )
    .await?;

    let pending_stream_key = codex_pending_stream_key(run_id);
    if let Some(channel_id) = channel_id {
        if let Err(err) = ensure_streaming_agent_message(
            pool,
            agent_id,
            channel_id,
            thread_root_id,
            &pending_stream_key,
        )
        .await
        {
            cleanup_failed_warm_codex_start(pool, agent_id, run_id, work_item_id, &err, false)
                .await?;
            return Err(err);
        }
    }

    let cwd = match effective_codex_cwd(&working_directory) {
        Ok(cwd) => cwd,
        Err(err) => {
            cleanup_failed_warm_codex_start(pool, agent_id, run_id, work_item_id, &err, false)
                .await?;
            return Err(err);
        }
    };
    let model_value = codex_model_value(&model);
    let request_id_result = {
        let mut state = runtime.state.lock().await;
        if !state.alive {
            Err("codex warm runtime exited before turn start".to_owned())
        } else if state.active.is_some() {
            Err("codex warm runtime became busy before turn start".to_owned())
        } else {
            let request_id = state.next_request_id;
            state.next_request_id += 1;
            state.last_activity = Instant::now();
            let mut stream_keys = HashSet::new();
            if channel_id.is_some() {
                stream_keys.insert(pending_stream_key.clone());
            }
            state.active = Some(CodexActiveTurn {
                run_id,
                turn_request_id: request_id,
                turn_id: None,
                started_at: Instant::now(),
                last_event_at: Instant::now(),
                first_delta_at: None,
                work_item_id,
                channel_id,
                thread_root_id,
                stream_keys,
                completed_agent_message_stream_keys: HashSet::new(),
                latest_agent_message_stream_key: None,
                steer_requests: HashMap::new(),
                steer_disabled: false,
                interrupt_request_id: None,
            });
            Ok(request_id)
        }
    };
    let request_id = match request_id_result {
        Ok(request_id) => request_id,
        Err(err) => {
            cleanup_failed_warm_codex_start(
                pool,
                agent_id,
                run_id,
                work_item_id,
                &err,
                err == "codex warm runtime became busy before turn start",
            )
            .await?;
            return Err(err);
        }
    };
    let write_result = {
        let mut stdin = runtime.stdin.lock().await;
        let mut params = json!({
            "threadId": runtime.thread_id.clone(),
            "input": [{
                "type": "text",
                "text": codex_prompt,
                "text_elements": []
            }],
            "cwd": cwd,
            "approvalPolicy": "never",
            "model": model_value
        });
        apply_codex_runtime_options(&mut params, &reasoning_effort, &service_tier);
        codex_write_json(
            &mut stdin,
            json!({
                "method": "turn/start",
                "id": request_id,
                "params": params
            }),
        )
        .await
    };

    if let Err(err) = write_result {
        finish_warm_codex_active_turn(pool, agent_id, &runtime, false, Some(err.clone())).await?;
        return Err(err);
    }

    Ok(())
}

async fn codex_warm_stdout_reader(
    pool: SqlitePool,
    registry: WarmCodexRegistry,
    agent_id: Uuid,
    runtime: Arc<WarmCodexRuntime>,
    reader: BufReader<tokio::process::ChildStdout>,
) {
    let mut lines = reader.lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if let Err(err) =
                    handle_codex_warm_stdout_line(&pool, agent_id, &runtime, &line).await
                {
                    let _ = record_agent_activity(
                        &pool,
                        Some(agent_id),
                        None,
                        "run_error",
                        "Codex stream event failed",
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
                    "Codex stdout read failed",
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
    let _ = finish_warm_codex_active_turn(
        &pool,
        agent_id,
        &runtime,
        false,
        Some("stdout closed".into()),
    )
    .await;
    remove_warm_codex_runtime_if_same(&registry, agent_id, &runtime).await;
}

async fn handle_codex_warm_stdout_line(
    pool: &SqlitePool,
    agent_id: Uuid,
    runtime: &Arc<WarmCodexRuntime>,
    line: &str,
) -> CommandResult<()> {
    let value: Value = serde_json::from_str(line).map_err(to_string)?;
    let active_run_id = {
        let mut state = runtime.state.lock().await;
        match state.active.as_mut() {
            Some(active) => {
                // Any codex output means the in-flight turn is still alive; reset
                // the stall timer so the idle reaper only fires on real silence.
                active.last_event_at = Instant::now();
                Some(active.run_id)
            }
            None => None,
        }
    };
    if let Some(run_id) = active_run_id {
        append_run_log(pool, run_id, format!("[codex] {line}\n")).await?;
    }

    if let (Some(run_id), Some((input_tokens, output_tokens))) =
        (active_run_id, usage_from_runtime_event(&value))
    {
        let _ = record_run_usage(pool, agent_id, run_id, input_tokens, output_tokens, None).await;
    }

    if let Some(response_id) = value.get("id").and_then(Value::as_i64) {
        let matched = {
            let mut state = runtime.state.lock().await;
            let Some(active) = state.active.as_mut() else {
                return Ok(());
            };
            if active.turn_request_id == response_id {
                if let Some(turn_id) = codex_turn_id_from_value(&value) {
                    active.turn_id = Some(turn_id);
                    state.last_activity = Instant::now();
                }
                Some((true, None, false))
            } else if let Some(steer) = active.steer_requests.remove(&response_id) {
                if codex_request_error(&value).is_some() {
                    active.steer_disabled = true;
                }
                state.last_activity = Instant::now();
                Some((false, Some(steer), false))
            } else if active.interrupt_request_id == Some(response_id) {
                active.interrupt_request_id = None;
                state.last_activity = Instant::now();
                Some((false, None, true))
            } else {
                None
            }
        };
        if let Some((is_turn_start, steer, is_interrupt)) = matched {
            if is_turn_start {
                if let Some(error) = codex_request_error(&value) {
                    finish_warm_codex_active_turn(pool, agent_id, runtime, false, Some(error))
                        .await?;
                } else if let Some(run_id) = active_run_id {
                    record_agent_activity(
                        pool,
                        Some(agent_id),
                        Some(run_id),
                        "run",
                        "Request acknowledged",
                        response_id.to_string(),
                    )
                    .await?;
                }
                return Ok(());
            }
            if is_interrupt {
                if let Some(error) = codex_request_error(&value) {
                    finish_warm_codex_active_turn(pool, agent_id, runtime, false, Some(error))
                        .await?;
                } else if let Some(run_id) = active_run_id {
                    record_agent_activity(
                        pool,
                        Some(agent_id),
                        Some(run_id),
                        "run",
                        "Stop acknowledged",
                        response_id.to_string(),
                    )
                    .await?;
                }
                return Ok(());
            }
            if let Some(steer) = steer {
                if let Some(error) = codex_request_error(&value) {
                    finish_codex_steer_request(pool, agent_id, steer, false, Some(error)).await?;
                } else {
                    finish_codex_steer_request(pool, agent_id, steer, true, None).await?;
                }
                return Ok(());
            }
        }
    }

    match value.get("method").and_then(Value::as_str) {
        Some("turn/started") => {
            if value.pointer("/params/threadId").and_then(Value::as_str)
                != Some(runtime.thread_id.as_str())
            {
                return Ok(());
            }
            if let Some(turn_id) = codex_turn_id_from_value(&value) {
                let mut state = runtime.state.lock().await;
                if let Some(active) = state.active.as_mut() {
                    active.turn_id = Some(turn_id);
                    state.last_activity = Instant::now();
                }
            }
        }
        Some("item/agentMessage/delta") => {
            let item_id = value
                .pointer("/params/itemId")
                .and_then(Value::as_str)
                .unwrap_or("agent-message");
            let delta = value
                .pointer("/params/delta")
                .and_then(Value::as_str)
                .unwrap_or("");
            if delta.is_empty() {
                return Ok(());
            }
            let (stream, first_delta_elapsed) = {
                let mut state = runtime.state.lock().await;
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
                (
                    track_codex_agent_message_stream(active, item_id),
                    first_delta_elapsed,
                )
            };
            if let Some(elapsed) = first_delta_elapsed {
                record_agent_activity(
                    pool,
                    Some(agent_id),
                    Some(stream.run_id),
                    "acting",
                    "Responding",
                    format!("first_token_ms={}", elapsed.as_millis()),
                )
                .await?;
            }
            if let Some(previous) = &stream.superseded_stream_key {
                delete_streaming_agent_message_by_key(
                    pool,
                    previous,
                    "superseded_intermediate_reply",
                )
                .await?;
            }
            if let Some(channel_id) = stream.channel_id {
                adopt_streaming_agent_message_key(
                    pool,
                    &stream.pending_stream_key,
                    &stream.stream_key,
                )
                .await?;
                append_streaming_agent_message_deferred_completion(
                    pool,
                    agent_id,
                    channel_id,
                    stream.thread_root_id,
                    &stream.stream_key,
                    delta,
                )
                .await?;
            }
        }
        Some("item/completed") if codex_item_type(&value) == Some("agentMessage") => {
            let Some(item_id) = codex_item_id(&value) else {
                return Ok(());
            };
            let stream = {
                let mut state = runtime.state.lock().await;
                let Some(active) = state.active.as_mut() else {
                    return Ok(());
                };
                let stream = track_codex_agent_message_stream(active, item_id);
                active
                    .completed_agent_message_stream_keys
                    .insert(stream.stream_key.clone());
                stream
            };
            if let Some(previous) = &stream.superseded_stream_key {
                delete_streaming_agent_message_by_key(
                    pool,
                    previous,
                    "superseded_intermediate_reply",
                )
                .await?;
            }
            if let Some(channel_id) = stream.channel_id {
                adopt_streaming_agent_message_key(
                    pool,
                    &stream.pending_stream_key,
                    &stream.stream_key,
                )
                .await?;
                if streaming_message_body_is_empty(pool, &stream.stream_key).await? {
                    if let Some(text) = value
                        .pointer("/params/item/text")
                        .and_then(Value::as_str)
                        .filter(|text| !text.is_empty())
                    {
                        append_streaming_agent_message_deferred_completion(
                            pool,
                            agent_id,
                            channel_id,
                            stream.thread_root_id,
                            &stream.stream_key,
                            text,
                        )
                        .await?;
                    }
                }
            }
        }
        Some("item/completed") => {
            let Some(run_id) = active_run_id else {
                return Ok(());
            };
            if let Some((kind, title, detail)) = codex_tool_completion_activity(&value) {
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
        Some("item/started") => {
            let Some(run_id) = active_run_id else {
                return Ok(());
            };
            let (kind, title, detail) = codex_item_started_activity(&value);
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
        Some("item/reasoning/textDelta") | Some("item/reasoning/summaryTextDelta") => {
            let Some(run_id) = active_run_id else {
                return Ok(());
            };
            if let Some(delta) = value.pointer("/params/delta").and_then(Value::as_str) {
                if !delta.trim().is_empty() {
                    append_run_log(pool, run_id, format!("[thinking] {delta}\n")).await?;
                }
            }
        }
        Some("turn/completed") => {
            if value.pointer("/params/threadId").and_then(Value::as_str)
                == Some(runtime.thread_id.as_str())
            {
                finish_warm_codex_active_turn(pool, agent_id, runtime, true, None).await?;
            }
        }
        Some("error") => {
            if let Some(detail) = codex_error_notification_detail(&value) {
                finish_warm_codex_active_turn(pool, agent_id, runtime, false, Some(detail)).await?;
            } else {
                let mut state = runtime.state.lock().await;
                state.last_activity = Instant::now();
            }
        }
        _ => {}
    }

    Ok(())
}

async fn codex_warm_stderr_reader<R>(
    pool: SqlitePool,
    agent_id: Uuid,
    runtime: Arc<WarmCodexRuntime>,
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

async fn wait_for_warm_codex_process(
    pool: SqlitePool,
    registry: WarmCodexRegistry,
    agent_id: Uuid,
    runtime: Arc<WarmCodexRuntime>,
    mut child: tokio::process::Child,
) {
    let wait_result = child.wait().await;
    {
        let mut state = runtime.state.lock().await;
        state.alive = false;
    }
    let detail = match wait_result {
        Ok(status) => format!("codex app-server exited: {status}"),
        Err(err) => format!("codex app-server wait failed: {err}"),
    };
    let _ =
        finish_warm_codex_active_turn(&pool, agent_id, &runtime, false, Some(detail.clone())).await;
    let _ = sqlx::query(
        r#"
        update runtime_sessions
        set status = 'stopped', updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where agent_id = $1
          and runtime = 'codex'
          and provider_thread_id = $2
        "#,
    )
    .bind(agent_id)
    .bind(&runtime.thread_id)
    .execute(&pool)
    .await;
    let _ = record_agent_activity(
        &pool,
        Some(agent_id),
        None,
        "run",
        "Codex warm app-server exited",
        detail,
    )
    .await;
    remove_warm_codex_runtime_if_same(&registry, agent_id, &runtime).await;
    let _ = notify_supervisor_wake(&pool).await;
}

async fn remove_warm_codex_runtime_if_same(
    registry: &WarmCodexRegistry,
    agent_id: Uuid,
    runtime: &Arc<WarmCodexRuntime>,
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

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, HashSet},
        process::Stdio,
        sync::Arc,
        time::{Duration, Instant},
    };

    use sqlx::Row;
    use tokio::{process::Command, sync::Mutex as AsyncMutex};

    use crate::runtime::surface::{codex_active_turn_schedule_state, CodexActiveTurnScheduleState};
    use crate::test_support::{
        drop_test_schema, insert_test_agent, insert_test_channel, test_pool,
    };

    use super::{
        codex_rotation_marker, finish_warm_codex_active_turn, prepend_codex_rotation_marker,
        track_codex_agent_message_stream, CodexActiveTurn, WarmCodexRuntime,
        CODEX_TURN_START_TIMEOUT,
    };

    async fn test_runtime_with_active_turn(
        run_id: uuid::Uuid,
        work_item_id: uuid::Uuid,
        channel_id: uuid::Uuid,
        stream_key: String,
    ) -> Result<Arc<WarmCodexRuntime>, String> {
        let mut command = Command::new("sleep");
        command.arg("60").stdin(Stdio::piped());
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().map_err(|err| err.to_string())?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "test process stdin unavailable".to_owned())?;
        Ok(Arc::new(WarmCodexRuntime {
            stdin: AsyncMutex::new(stdin),
            state: AsyncMutex::new(super::WarmCodexState {
                alive: true,
                active: Some(CodexActiveTurn {
                    run_id,
                    turn_request_id: 1,
                    turn_id: Some("turn-1".to_owned()),
                    started_at: Instant::now(),
                    last_event_at: Instant::now(),
                    first_delta_at: None,
                    work_item_id: Some(work_item_id),
                    channel_id: Some(channel_id),
                    thread_root_id: None,
                    stream_keys: HashSet::from([stream_key]),
                    completed_agent_message_stream_keys: HashSet::new(),
                    latest_agent_message_stream_key: None,
                    steer_requests: HashMap::new(),
                    steer_disabled: false,
                    interrupt_request_id: None,
                }),
                next_request_id: 2,
                pending_rotation_marker: None,
                last_activity: Instant::now(),
            }),
            thread_id: "test-codex-thread".to_owned(),
            pid: child.id().map(|id| id as i32),
            environment_variables: String::new(),
        }))
    }

    #[tokio::test]
    async fn failed_warm_codex_turn_keeps_agent_routable_and_stops_runtime() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "codex-failure-agent").await?;
            sqlx::query("update agents set status = 'running', runtime = 'codex' where id = $1")
                .bind(agent_id)
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            let channel_id = insert_test_channel(&pool, "codex-failure-channel").await?;
            let work_item_id: uuid::Uuid = sqlx::query_scalar(
                r#"
                insert into agent_work_items (agent_id, channel_id, title, status)
                values ($1, $2, 'warm codex failure', 'running')
                returning id
                "#,
            )
            .bind(agent_id)
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let run_id: uuid::Uuid = sqlx::query_scalar(
                r#"
                insert into agent_runs (agent_id, work_item_id, command, status)
                values ($1, $2, 'codex app-server', 'running')
                returning id
                "#,
            )
            .bind(agent_id)
            .bind(work_item_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let stream_key = format!("{run_id}:warm-codex");
            sqlx::query(
                r#"
                insert into messages (
                    channel_id, sender_agent_id, sender_name, sender_role,
                    body, delivery_state, stream_key
                )
                values ($1, $2, 'codex-failure-agent', 'agent', 'partial', 'streaming', $3)
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
            finish_warm_codex_active_turn(
                &pool,
                agent_id,
                &runtime,
                false,
                Some("stream disconnected before completion".to_owned()),
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
                .contains("stream disconnected before completion"));
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
                "select status from runtime_sessions where agent_id = $1 and runtime = 'codex'",
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

    #[test]
    fn codex_active_turn_without_turn_id_times_out_for_scheduling() {
        assert_eq!(
            codex_active_turn_schedule_state(
                Some("turn-1"),
                false,
                CODEX_TURN_START_TIMEOUT + Duration::from_secs(1),
                CODEX_TURN_START_TIMEOUT,
            ),
            CodexActiveTurnScheduleState::ReadyForSteer
        );
        assert_eq!(
            codex_active_turn_schedule_state(
                None,
                false,
                CODEX_TURN_START_TIMEOUT - Duration::from_secs(1),
                CODEX_TURN_START_TIMEOUT,
            ),
            CodexActiveTurnScheduleState::WaitingForTurnId
        );
        assert_eq!(
            codex_active_turn_schedule_state(
                None,
                false,
                CODEX_TURN_START_TIMEOUT + Duration::from_secs(1),
                CODEX_TURN_START_TIMEOUT,
            ),
            CodexActiveTurnScheduleState::StuckBeforeTurnId
        );
    }

    #[test]
    fn codex_rotation_marker_points_to_run_read_context_tool() {
        let run_id = uuid::Uuid::new_v4();
        let marker = codex_rotation_marker(run_id, 190_000, 180_000);

        assert!(marker.contains("Lantor rotated away"));
        assert!(marker.contains("190000 input tokens"));
        assert!(marker.contains("run-read"));
        assert!(marker.contains(&run_id.to_string()));

        let prompt = prepend_codex_rotation_marker("Current inbox item", &marker);
        assert!(prompt.starts_with("Lantor rotated away"));
        assert!(prompt.contains("Current Lantor request after context rotation"));
        assert!(prompt.ends_with("Current inbox item"));
    }

    #[test]
    fn later_codex_agent_message_supersedes_hidden_candidate() {
        let run_id = uuid::Uuid::new_v4();
        let pending_stream_key = format!("{run_id}:pending");
        let mut active = CodexActiveTurn {
            run_id,
            turn_request_id: 1,
            turn_id: Some("turn-1".to_owned()),
            started_at: Instant::now(),
            last_event_at: Instant::now(),
            first_delta_at: None,
            work_item_id: None,
            channel_id: None,
            thread_root_id: None,
            stream_keys: HashSet::from([pending_stream_key]),
            completed_agent_message_stream_keys: HashSet::new(),
            latest_agent_message_stream_key: None,
            steer_requests: HashMap::new(),
            steer_disabled: false,
            interrupt_request_id: None,
        };

        let first = track_codex_agent_message_stream(&mut active, "item-1");
        assert!(first.superseded_stream_key.is_none());
        assert!(active.stream_keys.contains(&first.stream_key));
        assert!(active.latest_agent_message_stream_key.as_ref() == Some(&first.stream_key));
        active
            .completed_agent_message_stream_keys
            .insert(first.stream_key.clone());

        let second = track_codex_agent_message_stream(&mut active, "item-2");
        assert_eq!(second.superseded_stream_key, Some(first.stream_key.clone()));
        assert!(!active.stream_keys.contains(&first.stream_key));
        assert!(!active
            .completed_agent_message_stream_keys
            .contains(&first.stream_key));
        assert!(active.stream_keys.contains(&second.stream_key));
        assert!(active.latest_agent_message_stream_key.as_ref() == Some(&second.stream_key));
    }
}
