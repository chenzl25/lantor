use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use super::ClaudeSurface;
use crate::app::{to_string, CommandResult};
use crate::runtime::process::truncate_activity_detail;

pub(super) const CLAUDE_MAX_RETRIES_ENV: &str = "ANTHROPIC_MAX_RETRIES";
pub(super) const DEFAULT_CLAUDE_MAX_RETRIES: &str = "5";

pub(super) fn claude_stream_key(run_id: Uuid) -> String {
    format!("{run_id}:claude-assistant")
}

pub(super) fn claude_text_delta(value: &Value) -> Option<&str> {
    if value.get("type").and_then(Value::as_str) != Some("stream_event") {
        return None;
    }
    if value.pointer("/event/type").and_then(Value::as_str) == Some("content_block_start")
        && value
            .pointer("/event/content_block/type")
            .and_then(Value::as_str)
            == Some("text")
    {
        return value
            .pointer("/event/content_block/text")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty());
    }
    if value.pointer("/event/delta/type").and_then(Value::as_str) != Some("text_delta") {
        return None;
    }
    value.pointer("/event/delta/text").and_then(Value::as_str)
}

pub(super) fn claude_session_id(value: &Value) -> Option<&str> {
    value.get("session_id").and_then(Value::as_str)
}

/// Text blocks of an `assistant` message in content order. Claude Code emits
/// one `assistant` line per content block, so this is usually a single entry.
pub(super) fn claude_message_text_blocks(value: &Value) -> Option<Vec<String>> {
    if value.get("type").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let content = value.pointer("/message/content")?.as_array()?;
    let blocks = content
        .iter()
        .filter_map(|block| {
            if block.get("type").and_then(Value::as_str) == Some("text") {
                block.get("text").and_then(Value::as_str).map(str::to_owned)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    blocks
        .iter()
        .any(|text| !text.trim().is_empty())
        .then_some(blocks)
}

#[cfg(test)]
pub(super) fn claude_message_text(value: &Value) -> Option<String> {
    claude_message_text_blocks(value).map(|blocks| blocks.join(""))
}

pub(super) fn claude_result_text(value: &Value) -> Option<String> {
    if value.get("type").and_then(Value::as_str) != Some("result") {
        return None;
    }
    value
        .get("result")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
}

pub(super) fn claude_result_error(value: &Value) -> Option<String> {
    if value.get("type").and_then(Value::as_str) != Some("result") {
        return None;
    }
    let is_error = value
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let api_error_status = value
        .get("api_error_status")
        .and_then(Value::as_str)
        .filter(|status| !status.trim().is_empty());
    if !is_error && api_error_status.is_none() {
        return None;
    }
    value
        .get("error")
        .and_then(Value::as_str)
        .or(api_error_status)
        .or_else(|| value.get("result").and_then(Value::as_str))
        .map(str::to_owned)
        .or_else(|| Some("Claude stream-json result reported an error".to_owned()))
}

fn claude_api_retry_detail(value: &Value) -> String {
    let attempt = value
        .get("attempt")
        .or_else(|| value.pointer("/retry/attempt"))
        .and_then(Value::as_i64);
    let max_retries = value
        .get("max_retries")
        .or_else(|| value.pointer("/retry/max_retries"))
        .and_then(Value::as_i64);
    let status = value
        .get("error_status")
        .or_else(|| value.pointer("/error/status"))
        .and_then(Value::as_i64)
        .map(claude_api_retry_status_label);
    let error = value
        .get("error")
        .or_else(|| value.pointer("/error/type"))
        .or_else(|| value.pointer("/error/error"))
        .and_then(Value::as_str);

    let mut parts = vec!["Lantor will retry automatically; no action needed".to_owned()];
    match (attempt, max_retries) {
        (Some(attempt), Some(max_retries)) => {
            parts.push(format!("attempt {attempt}/{max_retries}"));
        }
        (Some(attempt), None) => {
            parts.push(format!("attempt {attempt}"));
        }
        _ => {}
    }
    if let Some(status) = status {
        parts.push(status);
    }
    if let Some(error) = error {
        parts.push(format!("error {error}"));
    }

    truncate_activity_detail(&parts.join(" · "))
}

fn claude_api_retry_status_label(status: i64) -> String {
    match status {
        429 => "status 429 (rate limited)".to_owned(),
        529 => "status 529 (overloaded)".to_owned(),
        _ => format!("status {status}"),
    }
}

pub(super) fn claude_stream_event_activity(
    value: &Value,
) -> Option<(&'static str, &'static str, String)> {
    match value.get("type").and_then(Value::as_str)? {
        "system" => match value.get("subtype").and_then(Value::as_str) {
            Some("init") => Some(("run", "Runtime ready", "Claude stream connected".to_owned())),
            Some("api_retry") => Some((
                "run_retry",
                "Claude provider retrying",
                claude_api_retry_detail(value),
            )),
            Some(_) => None,
            None => None,
        },
        "rate_limit_event" => {
            let status = value
                .pointer("/rate_limit_info/status")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            if status.eq_ignore_ascii_case("allowed") {
                None
            } else {
                Some((
                    "run_retry",
                    "Waiting on rate limit",
                    format!("status={status}"),
                ))
            }
        }
        "stream_event" => {
            let event_type = value.pointer("/event/type").and_then(Value::as_str)?;
            match event_type {
                "content_block_start" => {
                    let block_type = value
                        .pointer("/event/content_block/type")
                        .and_then(Value::as_str)
                        .unwrap_or("content");
                    if block_type == "tool_use" {
                        let name = value
                            .pointer("/event/content_block/name")
                            .and_then(Value::as_str)
                            .unwrap_or("tool");
                        match name {
                            "Bash" => Some((
                                "command",
                                "Running command",
                                json!({ "tool": name }).to_string(),
                            )),
                            "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => Some((
                                "file_edit",
                                "Editing file",
                                json!({ "tool": name }).to_string(),
                            )),
                            _ => Some(("tools", "Using tool", name.to_owned())),
                        }
                    } else if block_type == "thinking" {
                        Some(("thinking", "Thinking", "Claude is thinking".to_owned()))
                    } else {
                        None
                    }
                }
                "content_block_stop" | "message_stop" => None,
                _ => None,
            }
        }
        _ => None,
    }
}

pub(super) fn claude_streaming_command_text(model: &str, reasoning_effort: &str) -> String {
    let effort = reasoning_effort.trim();
    let effort_arg = if effort.is_empty() {
        String::new()
    } else {
        format!(" --effort {effort}")
    };
    format!(
        "{CLAUDE_MAX_RETRIES_ENV}={DEFAULT_CLAUDE_MAX_RETRIES} claude --model {model}{effort_arg} --output-format stream-json --input-format stream-json --include-partial-messages --verbose --permission-mode bypassPermissions"
    )
}

pub(super) fn claude_user_input(prompt: &str) -> Value {
    json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{
                "type": "text",
                "text": prompt
            }]
        }
    })
}

pub(super) async fn claude_write_input(
    stdin: &mut tokio::process::ChildStdin,
    value: Value,
) -> CommandResult<()> {
    let mut line = serde_json::to_vec(&value).map_err(to_string)?;
    line.push(b'\n');
    stdin.write_all(&line).await.map_err(to_string)?;
    stdin.flush().await.map_err(to_string)
}

pub(super) fn claude_surface_boundary_marker(
    previous: Option<ClaudeSurface>,
    current: ClaudeSurface,
) -> Option<String> {
    let previous = previous?;
    if previous == current {
        return None;
    }
    Some(format!(
        "\n\n--- Lantor thread boundary ---\nPrevious surface: channel_id={}, thread_root_id={}\nCurrent surface: channel_id={}, thread_root_id={}\nThe warm Claude conversation may contain older turns from the previous surface. Treat this current surface and the injected current-thread context as authoritative. Do not use details from the previous surface unless the current prompt explicitly includes them.\n--- end boundary ---\n\n",
        display_optional_uuid(previous.channel_id),
        display_optional_uuid(previous.thread_root_id),
        display_optional_uuid(current.channel_id),
        display_optional_uuid(current.thread_root_id),
    ))
}

fn display_optional_uuid(id: Option<Uuid>) -> String {
    id.map(|id| id.to_string())
        .unwrap_or_else(|| "none".to_owned())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use super::*;

    #[test]
    fn claude_streaming_command_does_not_use_print_mode_flag() {
        let command = claude_streaming_command_text("sonnet", "high");
        assert!(!command.contains(" claude -p "));
        assert!(!command.contains(" --print"));
        assert!(command.contains("claude --model sonnet --effort high"));
        assert!(command.contains("--input-format stream-json"));
        assert!(command.contains("--output-format stream-json"));
    }

    #[test]
    fn claude_streaming_command_supports_fable_5_1() {
        let command = claude_streaming_command_text("claude-fable-5-1", "max");
        assert!(command.contains("claude --model claude-fable-5-1 --effort max"));
    }

    #[test]
    fn claude_surface_boundary_only_appears_when_surface_changes() {
        let channel_id = Uuid::new_v4();
        let thread_a = Uuid::new_v4();
        let thread_b = Uuid::new_v4();
        let previous = ClaudeSurface {
            channel_id: Some(channel_id),
            thread_root_id: Some(thread_a),
        };

        assert!(claude_surface_boundary_marker(None, previous).is_none());
        assert!(claude_surface_boundary_marker(Some(previous), previous).is_none());

        let marker = claude_surface_boundary_marker(
            Some(previous),
            ClaudeSurface {
                channel_id: Some(channel_id),
                thread_root_id: Some(thread_b),
            },
        )
        .expect("changed surface should produce a boundary");
        assert!(marker.contains("Lantor thread boundary"));
        assert!(marker.contains("Previous surface:"));
        assert!(marker.contains("Current surface:"));
    }

    #[test]
    fn parses_claude_stream_json_events() {
        assert_eq!(
            claude_text_delta(
                &json!({"type": "stream_event", "event": {"delta": {"type": "text_delta", "text": "hi"}}})
            ),
            Some("hi")
        );
        assert_eq!(
            claude_message_text(&json!({
                "type": "assistant",
                "message": {
                    "content": [
                        {"type": "text", "text": "hello"},
                        {"type": "tool_use", "name": "Read"}
                    ]
                }
            })),
            Some("hello".to_owned())
        );
        assert_eq!(
            claude_result_error(
                &json!({"type": "result", "is_error": true, "result": "rate limited"})
            ),
            Some("rate limited".to_owned())
        );
        assert_eq!(
            claude_result_error(
                &json!({"type": "result", "is_error": false, "api_error_status": null, "result": "ok"})
            ),
            None
        );
        assert_eq!(
            claude_result_error(
                &json!({"type": "result", "is_error": false, "api_error_status": "rate_limited", "result": "busy"})
            ),
            Some("rate_limited".to_owned())
        );
        assert_eq!(
            claude_stream_event_activity(&json!({"type": "system", "subtype": "init"})),
            Some(("run", "Runtime ready", "Claude stream connected".to_owned()))
        );
        assert_eq!(
            claude_stream_event_activity(&json!({
                "type": "system",
                "subtype": "api_retry",
                "attempt": 2,
                "max_retries": 3,
                "error_status": 529,
                "error": "rate_limit"
            })),
            Some((
                "run_retry",
                "Claude provider retrying",
                "Lantor will retry automatically; no action needed · attempt 2/3 · status 529 (overloaded) · error rate_limit"
                    .to_owned()
            ))
        );
        assert_eq!(
            claude_stream_event_activity(&json!({"type": "system", "subtype": "api_retry"})),
            Some((
                "run_retry",
                "Claude provider retrying",
                "Lantor will retry automatically; no action needed".to_owned()
            ))
        );
        assert_eq!(
            claude_stream_event_activity(
                &json!({"type": "rate_limit_event", "rate_limit_info": {"status": "allowed"}})
            ),
            None
        );
        assert_eq!(
            claude_stream_event_activity(
                &json!({"type": "rate_limit_event", "rate_limit_info": {"status": "limited"}})
            ),
            Some((
                "run_retry",
                "Waiting on rate limit",
                "status=limited".to_owned()
            ))
        );
        assert_eq!(
            claude_stream_event_activity(
                &json!({"type": "stream_event", "event": {"type": "message_stop"}})
            ),
            None
        );
        assert_eq!(
            claude_stream_event_activity(
                &json!({"type": "stream_event", "event": {"type": "content_block_start", "content_block": {"type": "tool_use", "name": "Bash"}}})
            ),
            Some((
                "command",
                "Running command",
                json!({"tool": "Bash"}).to_string()
            ))
        );
        assert_eq!(
            claude_stream_event_activity(
                &json!({"type": "stream_event", "event": {"type": "content_block_start", "content_block": {"type": "tool_use", "name": "Edit"}}})
            ),
            Some((
                "file_edit",
                "Editing file",
                json!({"tool": "Edit"}).to_string()
            ))
        );
    }
}
