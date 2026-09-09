use std::{env, process::Stdio};

use serde_json::{json, Value};
use sqlx::SqlitePool;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    process::Command,
};
use uuid::Uuid;

use crate::agent_environment::apply_agent_environment_variables;
use crate::agent_memory::append_run_log;
use crate::events::activity::{
    record_agent_activity, record_agent_activity_throttled, work_status_title,
};
use crate::events::control::{
    extract_agent_event_json, ingest_agent_event_line, replay_agent_events_from_run_log_if_needed,
    silent_reply_reason,
};
use crate::freshness::advance_agent_target_watermark_for_work_item;
use crate::runtime::streaming::mark_run_work_item_silent;
use crate::ui_notifications::{
    enqueue_ui_agent_run_changed_in_tx, enqueue_ui_work_item_changed_in_tx, notify_supervisor_wake,
    reconcile_work_item_change,
};
use crate::{
    app::{to_string, CommandResult},
    db::db_url,
    mark_task_after_work_item_finished,
};

const LANTOR_CONTEXT_TOOL_ENV: &str = "LANTOR_CONTEXT_TOOL";
const WORK_ITEM_PROMPT_ENV_LIMIT: usize = 128 * 1024;

pub(crate) struct ProcessAgentLaunch {
    pub(crate) agent_id: Uuid,
    pub(crate) work_item_id: Option<Uuid>,
    pub(crate) handle: String,
    pub(crate) working_directory: String,
    pub(crate) command_text: String,
    pub(crate) environment_variables: String,
    pub(crate) work_item_prompt: String,
}

pub(crate) fn truncate_activity_detail(value: &str) -> String {
    let trimmed = value.trim();
    let mut chars = trimmed.chars();
    let mut out: String = chars.by_ref().take(600).collect();
    if chars.next().is_some() {
        out.push_str("...");
    }
    out
}

fn structured_log_string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn structured_log_message(value: &Value) -> Option<&str> {
    value
        .pointer("/fields/message")
        .and_then(Value::as_str)
        .or_else(|| structured_log_string(value, "message"))
}

fn structured_log_field_string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get("fields")
        .and_then(|fields| fields.get(key))
        .and_then(Value::as_str)
        .or_else(|| structured_log_string(value, key))
}

fn structured_log_detail(
    value: &Value,
    level: &str,
    target: Option<&str>,
    message: Option<&str>,
) -> String {
    let mut detail = serde_json::Map::new();
    detail.insert("level".to_owned(), json!(level));
    if let Some(target) = target.map(str::trim).filter(|target| !target.is_empty()) {
        detail.insert("target".to_owned(), json!(target));
    }
    if let Some(message) = message.map(str::trim).filter(|message| !message.is_empty()) {
        detail.insert("message".to_owned(), json!(message));
    }
    if detail.len() > 1 {
        return Value::Object(detail).to_string();
    }
    truncate_activity_detail(&value.to_string())
}

fn is_ignored_codex_manifest_warning(
    level: &str,
    target: Option<&str>,
    message: Option<&str>,
) -> bool {
    matches!(level.to_ascii_uppercase().as_str(), "WARN" | "WARNING")
        && target == Some("codex_core_plugins::manifest")
        && message
            .map(str::trim)
            .is_some_and(|message| message.starts_with("ignoring interface.defaultPrompt:"))
}

fn is_ignored_codex_skill_loader_warning(
    level: &str,
    target: Option<&str>,
    message: Option<&str>,
) -> bool {
    matches!(level.to_ascii_uppercase().as_str(), "WARN" | "WARNING")
        && target == Some("codex_core_skills::loader")
        && message.map(str::trim).is_some_and(|message| {
            matches!(
                message,
                "ignoring interface.icon_small: icon path must not contain '..'"
                    | "ignoring interface.icon_large: icon path must not contain '..'"
            )
        })
}

fn is_ignored_codex_legacy_notify_warning(
    value: &Value,
    level: &str,
    target: Option<&str>,
    message: Option<&str>,
) -> bool {
    matches!(level.to_ascii_uppercase().as_str(), "WARN" | "WARNING")
        && target == Some("codex_core::session::turn")
        && message.map(str::trim) == Some("after_agent hook failed; continuing")
        && structured_log_field_string(value, "hook_name") == Some("legacy_notify")
        && structured_log_field_string(value, "error")
            .map(str::trim)
            .is_some_and(|error| error.contains("No such file or directory"))
}

fn strip_ansi_escape_sequences(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            output.push(ch);
            continue;
        }
        if chars.peek() != Some(&'[') {
            continue;
        }
        chars.next();
        for next in chars.by_ref() {
            if next.is_ascii_alphabetic() {
                break;
            }
        }
    }
    output
}

fn is_retryable_codex_stderr_error(line: &str) -> bool {
    let cleaned = strip_ansi_escape_sequences(line).to_lowercase();
    (cleaned.contains("codex_api::endpoint::responses_websocket")
        && cleaned.contains("failed to connect to websocket"))
        || (cleaned.contains("codex_models_manager::manager")
            && cleaned.contains("failed to refresh available models"))
        || (cleaned.contains("rmcp::transport::worker")
            && cleaned.contains("https://chatgpt.com/backend-api/wham/apps"))
}

fn classify_structured_stderr_log(
    line: &str,
) -> Option<Option<(&'static str, &'static str, String)>> {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return None;
    };
    if !value.is_object() {
        return None;
    }
    let level = structured_log_string(&value, "level")?;

    let level = level.trim();
    let target = structured_log_string(&value, "target");
    let message = structured_log_message(&value);
    if is_ignored_codex_manifest_warning(level, target, message) {
        return Some(None);
    }
    if is_ignored_codex_skill_loader_warning(level, target, message) {
        return Some(None);
    }
    if is_ignored_codex_legacy_notify_warning(&value, level, target, message) {
        return Some(None);
    }

    let detail = structured_log_detail(&value, level, target, message);
    match level.to_ascii_uppercase().as_str() {
        "ERROR" => Some(Some(("error", "Runtime error", detail))),
        "WARN" | "WARNING" => Some(Some(("run", "Runtime warning", detail))),
        "DEBUG" | "TRACE" | "INFO" => Some(None),
        _ => Some(Some(("run", "Runtime log", detail))),
    }
}

pub(crate) fn classify_agent_output_activity(
    label: &str,
    line: &str,
) -> Option<(&'static str, &'static str, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || extract_agent_event_json(trimmed).is_some() {
        return None;
    }

    let lower = trimmed.to_lowercase();
    if let Some(activity) = classify_structured_stderr_log(trimmed) {
        return activity;
    }

    if label == "stderr" && is_retryable_codex_stderr_error(trimmed) {
        return Some(("run", "Runtime warning", truncate_activity_detail(trimmed)));
    }

    let is_error = label == "stderr"
        && [
            "error",
            "failed",
            "panic",
            "exception",
            "traceback",
            "permission denied",
            "not found",
        ]
        .iter()
        .any(|needle| lower.contains(needle));
    if is_error {
        return Some(("error", "Error output", truncate_activity_detail(trimmed)));
    }

    let is_warning = label == "stderr"
        && (lower.contains("warning")
            || lower.contains("warn:")
            || lower.contains("level=warn")
            || lower.contains("\"level\":\"warn\""));
    if is_warning {
        return Some(("run", "Runtime warning", truncate_activity_detail(trimmed)));
    }

    let is_tool = [
        "tool",
        "exec_command",
        "apply_patch",
        "running command",
        "cargo ",
        "npm ",
        "git ",
        "psql ",
        "rg ",
        "sed ",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        || trimmed.starts_with("$ ");
    if is_tool {
        return Some(("tools", "Using tools", truncate_activity_detail(trimmed)));
    }

    let is_action = [
        "message sent",
        "created",
        "updated",
        "deleted",
        "fixed",
        "committed",
        "commit ",
        "done",
        "completed",
        "finished",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    if is_action {
        return Some(("acting", "Acting", truncate_activity_detail(trimmed)));
    }

    if label == "stderr" {
        return Some(("run", "Runtime output", truncate_activity_detail(trimmed)));
    }

    Some(("thinking", "Thinking", truncate_activity_detail(trimmed)))
}

pub(crate) async fn pipe_run_output<R>(
    pool: SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
    stream: R,
    label: &'static str,
    parse_agent_events: bool,
) where
    R: AsyncRead + Unpin,
{
    let mut lines = BufReader::new(stream).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                let _ = append_run_log(&pool, run_id, format!("[{label}] {line}\n")).await;
                if let Some((kind, title, detail)) = classify_agent_output_activity(label, &line) {
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
                if parse_agent_events {
                    if let Some(reason) = silent_reply_reason(&line) {
                        let _ = mark_run_work_item_silent(&pool, agent_id, run_id, &reason).await;
                        continue;
                    }
                    match ingest_agent_event_line(&pool, agent_id, run_id, &line).await {
                        Ok(Some(note)) => {
                            let _ =
                                append_run_log(&pool, run_id, format!("[event] {note}\n")).await;
                            let _ = record_agent_activity(
                                &pool,
                                Some(agent_id),
                                Some(run_id),
                                "event",
                                "Stdout event accepted",
                                note,
                            )
                            .await;
                        }
                        Ok(None) => {}
                        Err(err) => {
                            let _ =
                                append_run_log(&pool, run_id, format!("[event] rejected: {err}\n"))
                                    .await;
                            let _ = record_agent_activity(
                                &pool,
                                Some(agent_id),
                                Some(run_id),
                                "event_error",
                                "Stdout event rejected",
                                err.to_string(),
                            )
                            .await;
                        }
                    }
                }
            }
            Ok(None) => break,
            Err(err) => {
                let _ =
                    append_run_log(&pool, run_id, format!("[{label}] read error: {err}\n")).await;
                break;
            }
        }
    }
}

pub(crate) async fn load_runtime_thread_id(
    pool: &SqlitePool,
    agent_id: Uuid,
    runtime: &str,
) -> CommandResult<Option<String>> {
    let thread_id: Option<String> = sqlx::query_scalar(
        r#"
        select provider_thread_id
        from runtime_sessions
        where agent_id = $1
          and runtime = $2
        "#,
    )
    .bind(agent_id)
    .bind(runtime)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;
    Ok(thread_id.filter(|thread_id| !thread_id.trim().is_empty()))
}

pub(crate) async fn upsert_runtime_thread_id(
    pool: &SqlitePool,
    agent_id: Uuid,
    runtime: &str,
    provider_thread_id: &str,
    status: &str,
) -> CommandResult<()> {
    sqlx::query(
        r#"
        insert into runtime_sessions (agent_id, runtime, provider_thread_id, status)
        values ($1, $2, $3, $4)
        on conflict (agent_id, runtime) do update set
            provider_thread_id = excluded.provider_thread_id,
            status = excluded.status,
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        "#,
    )
    .bind(agent_id)
    .bind(runtime)
    .bind(provider_thread_id)
    .bind(status)
    .execute(pool)
    .await
    .map_err(to_string)?;
    Ok(())
}

#[cfg(test)]
#[path = "../tests/runtime_process.rs"]
mod relocated_tests;

pub(crate) async fn wait_for_agent_run(
    pool: SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
    work_item_id: Option<Uuid>,
    pipe_tasks: Vec<tokio::task::JoinHandle<()>>,
    mut child: tokio::process::Child,
) {
    let result = child.wait().await;
    for task in pipe_tasks {
        let _ = task.await;
    }
    let _ = replay_agent_events_from_run_log_if_needed(&pool, agent_id, run_id).await;
    let current_run_status: Option<String> =
        sqlx::query_scalar("select status from agent_runs where id = $1")
            .bind(run_id)
            .fetch_optional(&pool)
            .await
            .ok()
            .flatten();
    let (run_status, agent_status, exit_code, log_line) = match result {
        _ if current_run_status.as_deref() == Some("failed") => (
            "failed",
            "error",
            None,
            "process exited after runtime error\n".to_owned(),
        ),
        Ok(status) if status.success() => (
            "exited",
            "idle",
            status.code(),
            format!("process exited successfully: {status}\n"),
        ),
        Ok(status) => (
            "stopped",
            "idle",
            status.code(),
            format!("process stopped: {status}\n"),
        ),
        Err(err) => (
            "failed",
            "error",
            None,
            format!("failed while waiting for process: {err}\n"),
        ),
    };

    let resolved_work_status = if let Some(work_item_id) = work_item_id {
        let current_status: Option<String> =
            sqlx::query_scalar("select status from agent_work_items where id = $1")
                .bind(work_item_id)
                .fetch_optional(&pool)
                .await
                .ok()
                .flatten();
        let work_status = if current_status.as_deref() == Some("cancelling") {
            "cancelled"
        } else if current_status.as_deref() == Some("silent") && run_status == "exited" {
            "silent"
        } else if run_status == "exited" {
            "done"
        } else {
            "failed"
        };
        Some((work_item_id, work_status))
    } else {
        None
    };
    let persist_result: CommandResult<()> = async {
        let mut transaction = pool.begin().await.map_err(to_string)?;
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
        .bind(run_status)
        .bind(exit_code)
        .bind(&log_line)
        .execute(&mut *transaction)
        .await
        .map_err(to_string)?;
        sqlx::query("update agents set status = $2 where id = $1")
            .bind(agent_id)
            .bind(agent_status)
            .execute(&mut *transaction)
            .await
            .map_err(to_string)?;
        enqueue_ui_agent_run_changed_in_tx(&mut transaction, run_id, "run_finished").await?;

        if let Some((work_item_id, work_status)) = resolved_work_status {
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
            .bind(work_status)
            .execute(&mut *transaction)
            .await
            .map_err(to_string)?;
            enqueue_ui_work_item_changed_in_tx(
                &mut transaction,
                work_item_id,
                "work_item_finished",
            )
            .await?;
        }
        transaction.commit().await.map_err(to_string)
    }
    .await;
    if let Err(error) = &persist_result {
        eprintln!("Failed to persist terminal process state for run {run_id}: {error}");
    }

    if let Some((work_item_id, work_status)) = resolved_work_status {
        if persist_result.is_ok() {
            let _ = reconcile_work_item_change(&pool, work_item_id, "work_item_finished").await;
        }
        let _ =
            mark_task_after_work_item_finished(&pool, work_item_id, agent_id, run_id, work_status)
                .await;
        let _ = record_agent_activity(
            &pool,
            Some(agent_id),
            Some(run_id),
            "dispatch",
            work_status_title(work_status),
            work_item_id.to_string(),
        )
        .await;
    }

    let _ = record_agent_activity(
        &pool,
        Some(agent_id),
        Some(run_id),
        "run",
        format!("Run {run_status}"),
        log_line.trim(),
    )
    .await;
}

pub(crate) async fn start_process_agent(
    pool: &SqlitePool,
    launch: ProcessAgentLaunch,
) -> CommandResult<()> {
    let ProcessAgentLaunch {
        agent_id,
        work_item_id,
        handle,
        working_directory,
        command_text,
        environment_variables,
        work_item_prompt,
    } = launch;
    let initial_log = if work_item_prompt.is_empty() {
        format!("$ {command_text}\n")
    } else {
        format!("$ {command_text}\n\n[agent request]\n{work_item_prompt}\n")
    };

    let mut transaction = pool.begin().await.map_err(to_string)?;
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
    .fetch_one(&mut *transaction)
    .await
    .map_err(to_string)?;
    enqueue_ui_agent_run_changed_in_tx(&mut transaction, run_id, "run_created").await?;
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
        .execute(&mut *transaction)
        .await
        .map_err(to_string)?;
        enqueue_ui_work_item_changed_in_tx(&mut transaction, work_item_id, "work_item_running")
            .await?;
    }
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
        "Run created",
        "Supervisor is preparing the launch command",
    )
    .await?;

    let mut command = Command::new("/bin/zsh");
    command.arg("-lc").arg(&command_text);
    apply_agent_environment_variables(&mut command, &environment_variables)?;
    configure_agent_identity_env(&mut command, agent_id, &handle);
    configure_agent_context_tool_env(&mut command);
    let run_id_value = run_id.to_string();
    command.env("LANTOR_RUN_ID", run_id_value);
    let work_item_id_value = work_item_id
        .map(|id| id.to_string())
        .unwrap_or_else(String::new);
    command.env("LANTOR_WORK_ITEM_ID", work_item_id_value);
    // Oversized env vars make spawn fail with E2BIG (macOS arg+env budget is
    // ~1MB); hand large prompts over via a file and keep a truncated preview
    // in the env var so simple launch commands keep working.
    if work_item_prompt.len() > WORK_ITEM_PROMPT_ENV_LIMIT {
        let prompt_path = env::temp_dir().join(format!("lantor-work-item-prompt-{run_id}.txt"));
        match tokio::fs::write(&prompt_path, &work_item_prompt).await {
            Ok(()) => {
                let mut cut = WORK_ITEM_PROMPT_ENV_LIMIT;
                while !work_item_prompt.is_char_boundary(cut) {
                    cut -= 1;
                }
                command.env("LANTOR_WORK_ITEM_PROMPT_FILE", &prompt_path);
                command.env(
                    "LANTOR_WORK_ITEM_PROMPT",
                    format!(
                        "{}\n\n[prompt truncated: full text in the file at $LANTOR_WORK_ITEM_PROMPT_FILE]",
                        &work_item_prompt[..cut]
                    ),
                );
            }
            Err(_) => {
                // Best effort: fall back to the env var (same behavior as before).
                command.env("LANTOR_WORK_ITEM_PROMPT", &work_item_prompt);
            }
        }
    } else {
        command.env("LANTOR_WORK_ITEM_PROMPT", &work_item_prompt);
    }
    #[cfg(unix)]
    command.process_group(0);
    if !working_directory.is_empty() {
        command.current_dir(&working_directory);
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            let error_log = format!("failed to start process: {err}\n");
            let mut transaction = pool.begin().await.map_err(to_string)?;
            sqlx::query(
                r#"
                update agent_runs
                set status = 'failed', log = substr(log || $2, -20000), stopped_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
                where id = $1
                "#,
            )
            .bind(run_id)
            .bind(error_log)
            .execute(&mut *transaction)
            .await
            .map_err(to_string)?;
            sqlx::query("update agents set status = 'error' where id = $1")
                .bind(agent_id)
                .execute(&mut *transaction)
                .await
                .map_err(to_string)?;
            enqueue_ui_agent_run_changed_in_tx(&mut transaction, run_id, "run_failed").await?;
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
                .execute(&mut *transaction)
                .await
                .map_err(to_string)?;
                enqueue_ui_work_item_changed_in_tx(
                    &mut transaction,
                    work_item_id,
                    "work_item_failed",
                )
                .await?;
            }
            transaction.commit().await.map_err(to_string)?;
            if let Some(work_item_id) = work_item_id {
                reconcile_work_item_change(pool, work_item_id, "work_item_failed").await?;
            }
            record_agent_activity(
                pool,
                Some(agent_id),
                Some(run_id),
                "run_error",
                "Run failed to start",
                err.to_string(),
            )
            .await?;
            return Err(err.to_string());
        }
    };

    let pid = child.id().map(|id| id as i32);
    let mut transaction = pool.begin().await.map_err(to_string)?;
    sqlx::query("update agent_runs set status = 'running', pid = $2 where id = $1")
        .bind(run_id)
        .bind(pid)
        .execute(&mut *transaction)
        .await
        .map_err(to_string)?;
    sqlx::query("update agents set status = 'running' where id = $1")
        .bind(agent_id)
        .execute(&mut *transaction)
        .await
        .map_err(to_string)?;
    enqueue_ui_agent_run_changed_in_tx(&mut transaction, run_id, "run_running").await?;
    transaction.commit().await.map_err(to_string)?;
    record_agent_activity(
        pool,
        Some(agent_id),
        Some(run_id),
        "run",
        "Run started",
        pid.map(|pid| format!("pid={pid}"))
            .unwrap_or_else(|| "pid unavailable".to_owned()),
    )
    .await?;
    if let Some(work_item_id) = work_item_id {
        advance_agent_target_watermark_for_work_item(pool, agent_id, work_item_id, 0).await?;
    }

    let mut pipe_tasks = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        pipe_tasks.push(tokio::spawn(pipe_run_output(
            pool.clone(),
            agent_id,
            run_id,
            stdout,
            "stdout",
            true,
        )));
    }
    if let Some(stderr) = child.stderr.take() {
        pipe_tasks.push(tokio::spawn(pipe_run_output(
            pool.clone(),
            agent_id,
            run_id,
            stderr,
            "stderr",
            true,
        )));
    }

    tokio::spawn(wait_for_agent_run(
        pool.clone(),
        agent_id,
        run_id,
        work_item_id,
        pipe_tasks,
        child,
    ));

    Ok(())
}

pub(crate) fn effective_launch_command(
    launch_command: String,
    _runtime: String,
    _model: String,
    _handle: String,
) -> String {
    if !launch_command.trim().is_empty() {
        return launch_command.trim().to_owned();
    }

    "printf 'Lantor placeholder runtime. Configure launch_command to run a real agent.\\n'; sleep 3600"
        .to_owned()
}

pub(crate) fn configure_agent_context_tool_env(command: &mut Command) {
    if let Ok(exe_path) = env::current_exe() {
        command.env(LANTOR_CONTEXT_TOOL_ENV, exe_path);
    }
    command.env("LANTOR_DATABASE_URL", db_url());
}

pub(crate) fn configure_agent_identity_env(command: &mut Command, agent_id: Uuid, handle: &str) {
    command.env("LANTOR_AGENT_ID", agent_id.to_string());
    command.env("LANTOR_AGENT_HANDLE", handle);
}

const PROCESS_KILL_ESCALATION_DELAY_SECS: u64 = 10;

pub(crate) async fn terminate_process_group(pid: i32) -> CommandResult<()> {
    let status = Command::new("kill")
        .arg("-TERM")
        .arg(format!("-{pid}"))
        .status()
        .await
        .map_err(to_string)?;

    if !status.success() {
        return Err(format!("failed to terminate process group {pid}: {status}"));
    }

    // A process that ignores SIGTERM would otherwise never stop; escalate to
    // SIGKILL if the group is still alive after a grace period.
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(
            PROCESS_KILL_ESCALATION_DELAY_SECS,
        ))
        .await;
        let alive = Command::new("kill")
            .arg("-0")
            .arg(format!("-{pid}"))
            .status()
            .await
            .map(|status| status.success())
            .unwrap_or(false);
        if alive {
            let _ = Command::new("kill")
                .arg("-KILL")
                .arg(format!("-{pid}"))
                .status()
                .await;
        }
    });

    Ok(())
}

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

/// Shared cleanup for warm streaming runtimes (codex/claude) when a turn fails
/// after its run/work item rows were created but before the turn actually
/// started. Without this the run stays `running` and the agent looks busy
/// until the next app restart.
pub(crate) async fn cleanup_failed_warm_start(
    pool: &SqlitePool,
    runtime_label: &str,
    agent_id: Uuid,
    run_id: Uuid,
    work_item_id: Option<Uuid>,
    error: &str,
    failure: WarmStartFailure,
) -> CommandResult<()> {
    let error_log = format!("{runtime_label} warm turn failed before start: {error}\n");
    let mut transaction = pool.begin().await.map_err(to_string)?;
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
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;

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
    let requeue_work_item = match failure {
        WarmStartFailure::RuntimeBusy => true,
        WarmStartFailure::Transient => work_item_id.is_some() && attempts < MAX_WARM_START_ATTEMPTS,
        WarmStartFailure::Fatal => false,
    };

    match failure {
        WarmStartFailure::RuntimeBusy => {
            sqlx::query("update agents set status = 'running' where id = $1")
                .bind(agent_id)
                .execute(&mut *transaction)
                .await
                .map_err(to_string)?;
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
            sqlx::query("update agents set status = 'error' where id = $1")
                .bind(agent_id)
                .execute(&mut *transaction)
                .await
                .map_err(to_string)?;
        }
    }
    enqueue_ui_agent_run_changed_in_tx(&mut transaction, run_id, "run_failed").await?;

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
            .execute(&mut *transaction)
            .await
            .map_err(to_string)?;
            enqueue_ui_work_item_changed_in_tx(&mut transaction, work_item_id, "work_item_queued")
                .await?;
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
            .execute(&mut *transaction)
            .await
            .map_err(to_string)?;
            enqueue_ui_work_item_changed_in_tx(&mut transaction, work_item_id, "work_item_failed")
                .await?;
        }
    }
    transaction.commit().await.map_err(to_string)?;
    if let Some(work_item_id) = work_item_id {
        reconcile_work_item_change(
            pool,
            work_item_id,
            if requeue_work_item {
                "work_item_queued"
            } else {
                "work_item_failed"
            },
        )
        .await?;
    }
    if requeue_work_item {
        // The scheduler sweep relaunches the queued request on its next pass.
        let _ = notify_supervisor_wake(pool).await;
    }

    let (kind, title, detail) = match failure {
        WarmStartFailure::RuntimeBusy => ("dispatch", "Request requeued", error.to_owned()),
        WarmStartFailure::Transient if requeue_work_item => (
            "dispatch",
            "Request start retry scheduled",
            format!("attempt {attempts}/{MAX_WARM_START_ATTEMPTS} failed: {error}"),
        ),
        WarmStartFailure::Transient if work_item_id.is_some() => (
            "run_error",
            "Run failed to start",
            format!("gave up after {attempts} attempts: {error}"),
        ),
        WarmStartFailure::Transient | WarmStartFailure::Fatal => {
            ("run_error", "Run failed to start", error.to_owned())
        }
    };
    record_agent_activity(pool, Some(agent_id), Some(run_id), kind, title, detail).await?;
    Ok(())
}
