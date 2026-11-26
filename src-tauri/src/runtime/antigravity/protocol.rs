//! The headless stream-json protocol of Google Antigravity's `agy` CLI.
//!
//! `agy --input-format stream-json --output-format stream-json` reads one
//! `{"event":"user",...}` line per turn from stdin and writes one JSON event
//! per line: `init` once, `step_update` for each step of a turn (the user
//! input, the agent's text, each tool call), and `result` when the turn ends.
//! A new prompt must wait for the previous turn's `result`.

use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::app::{to_string, CommandResult};
use crate::usage::RunUsage;

use super::AntigravitySurface;

/// The flags every warm `agy` process starts with, before `--model` and
/// `--conversation`. `-p=` selects print mode without a prompt argument, so
/// every prompt comes from stdin.
pub(super) const AGY_STREAM_ARGS: &[&str] = &[
    "--input-format",
    "stream-json",
    "--output-format",
    "stream-json",
    "--dangerously-skip-permissions",
];

pub(super) fn agy_stream_key(run_id: Uuid) -> String {
    format!("{run_id}:antigravity-assistant")
}

pub(super) fn agy_command_text(model: &str) -> String {
    let model_arg = if model.is_empty() {
        String::new()
    } else {
        format!(" --model {model}")
    };
    format!("agy {}{model_arg} -p=", AGY_STREAM_ARGS.join(" "))
}

pub(super) fn agy_user_input(prompt: &str) -> Value {
    json!({ "event": "user", "message": { "content": prompt } })
}

pub(super) async fn agy_write_input(
    stdin: &mut tokio::process::ChildStdin,
    value: Value,
) -> CommandResult<()> {
    let mut line = serde_json::to_vec(&value).map_err(to_string)?;
    line.push(b'\n');
    stdin.write_all(&line).await.map_err(to_string)?;
    stdin.flush().await.map_err(to_string)
}

fn event_name(value: &Value) -> Option<&str> {
    value.get("event").and_then(Value::as_str)
}

fn step(value: &Value) -> Option<&Value> {
    (event_name(value) == Some("step_update"))
        .then(|| value.get("step_update"))
        .flatten()
}

/// The conversation the process runs, named once by `init`.
pub(super) fn agy_init_conversation_id(value: &Value) -> Option<&str> {
    if event_name(value) != Some("init") {
        return None;
    }
    value
        .get("conversation_id")
        .or_else(|| value.pointer("/init/conversation_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
}

/// A piece of the agent's reply and the step it belongs to. Each
/// `agent_response` step is one block of text.
pub(super) fn agy_text_delta(value: &Value) -> Option<(i64, &str)> {
    let step = step(value)?;
    if step.get("step_type").and_then(Value::as_str) != Some("agent_response") {
        return None;
    }
    let delta = step
        .get("text_delta")
        .and_then(Value::as_str)
        .filter(|delta| !delta.is_empty())?;
    Some((step.get("step_index").and_then(Value::as_i64)?, delta))
}

/// A tool call that just started, as an activity kind, title and detail.
/// Shell commands and file edits use the kinds every runtime reports them with.
pub(super) fn agy_tool_started(value: &Value) -> Option<(&'static str, &'static str, String)> {
    let step = step(value)?;
    if step.get("step_type").and_then(Value::as_str) != Some("tool")
        || step.get("state").and_then(Value::as_str) != Some("ACTIVE")
    {
        return None;
    }
    let name = step
        .get("tool_name")
        .or_else(|| step.pointer("/tool_info/name"))
        .and_then(Value::as_str)
        .unwrap_or("tool");
    let parameters = step.pointer("/tool_info/parameters");
    let parameter = |key: &str| {
        parameters
            .and_then(|parameters| parameters.get(key))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    if name == "run_command" {
        let detail = parameter("CommandLine").unwrap_or_else(|| name.to_owned());
        return Some(("command", "Running command", detail));
    }
    let lower = name.to_ascii_lowercase();
    if ["write", "edit", "replace", "create_file", "delete_file"]
        .iter()
        .any(|verb| lower.contains(verb))
    {
        let detail = parameter("Path")
            .or_else(|| parameter("TargetFile"))
            .unwrap_or_else(|| name.to_owned());
        return Some(("file_edit", "Editing file", detail));
    }
    Some(("tools", "Using tool", name.to_owned()))
}

/// Token counts as Lantor records them. agy reports input without its cache
/// reads, which it lists apart, and thinking apart from output; thinking is
/// billed as output.
fn usage(value: &Value) -> Option<RunUsage> {
    let number = |key: &str| value.get(key).and_then(Value::as_i64).unwrap_or(0).max(0);
    value.is_object().then(|| RunUsage {
        input_tokens: number("input_tokens"),
        cached_input_tokens: number("cache_read_tokens"),
        cache_write_tokens: 0,
        output_tokens: number("output_tokens") + number("thinking_tokens"),
    })
}

/// What one finished model request used, and the context it sent, from a
/// step's `usage`.
pub(super) fn agy_step_usage(value: &Value) -> Option<(RunUsage, i64)> {
    let request = usage(step(value)?.get("usage")?)?;
    let context_tokens = request.total_input_tokens();
    Some((request, context_tokens))
}

/// How a turn ended, from its `result`.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct AgyTurnResult {
    pub(super) success: bool,
    /// The final reply, without the notes written between tool calls.
    pub(super) response: String,
    pub(super) error: Option<String>,
    pub(super) usage: Option<RunUsage>,
}

pub(super) fn agy_turn_result(value: &Value) -> Option<AgyTurnResult> {
    if event_name(value) != Some("result") {
        return None;
    }
    let result = value.get("result")?;
    let status = result
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("UNKNOWN");
    let success = status == "SUCCESS";
    let error = (!success).then(|| {
        result
            .get("error")
            .and_then(|error| {
                error.as_str().map(str::to_owned).or_else(|| {
                    error
                        .get("message")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
            })
            .filter(|error| !error.trim().is_empty())
            .unwrap_or_else(|| format!("agy ended the turn with status {status}"))
    });
    Some(AgyTurnResult {
        success,
        response: result
            .get("response")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        error,
        usage: result.get("usage").and_then(usage),
    })
}

pub(super) fn agy_surface_boundary_marker(
    previous: Option<AntigravitySurface>,
    current: AntigravitySurface,
) -> Option<String> {
    let previous = previous?;
    if previous == current {
        return None;
    }
    let show = |id: Option<Uuid>| {
        id.map(|id| id.to_string())
            .unwrap_or_else(|| "none".to_owned())
    };
    Some(format!(
        "\n\n--- Lantor thread boundary ---\nPrevious surface: channel_id={}, thread_root_id={}\nCurrent surface: channel_id={}, thread_root_id={}\nThe warm Antigravity conversation may contain older turns from the previous surface. Treat this current surface and the injected current-thread context as authoritative. Do not use details from the previous surface unless the current prompt explicitly includes them.\n--- end boundary ---\n\n",
        show(previous.channel_id),
        show(previous.thread_root_id),
        show(current.channel_id),
        show(current.thread_root_id),
    ))
}

#[cfg(test)]
#[path = "../../tests/runtime_antigravity_protocol.rs"]
mod tests;
