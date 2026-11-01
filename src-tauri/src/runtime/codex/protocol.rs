use std::env;

use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::app::{to_string, CommandResult};
use crate::runtime::process::truncate_activity_detail;

const CODEX_CONTEXT_ROTATE_DEFAULT_INPUT_TOKENS: i64 = 180_000;
const CODEX_CONTEXT_ROTATE_MIN_INPUT_TOKENS: i64 = 50_000;
const CODEX_CONTEXT_ROTATE_ENV: &str = "LANTOR_CODEX_CONTEXT_ROTATE_INPUT_TOKENS";

fn codex_context_rotate_input_tokens_from_env(value: Option<&str>) -> i64 {
    value
        .and_then(|value| value.trim().parse::<i64>().ok())
        .filter(|tokens| *tokens >= CODEX_CONTEXT_ROTATE_MIN_INPUT_TOKENS)
        .unwrap_or(CODEX_CONTEXT_ROTATE_DEFAULT_INPUT_TOKENS)
}

pub(super) fn codex_context_rotate_input_tokens() -> i64 {
    codex_context_rotate_input_tokens_from_env(env::var(CODEX_CONTEXT_ROTATE_ENV).ok().as_deref())
}

pub(super) fn codex_context_rotate_env() -> &'static str {
    CODEX_CONTEXT_ROTATE_ENV
}

pub(super) fn codex_stream_key(run_id: Uuid, item_id: &str) -> String {
    format!("{run_id}:{item_id}")
}

pub(super) fn codex_pending_stream_key(run_id: Uuid) -> String {
    format!("{run_id}:pending")
}

pub(super) async fn codex_write_json(
    stdin: &mut tokio::process::ChildStdin,
    value: Value,
) -> CommandResult<()> {
    let mut line = serde_json::to_vec(&value).map_err(to_string)?;
    line.push(b'\n');
    stdin.write_all(&line).await.map_err(to_string)?;
    stdin.flush().await.map_err(to_string)?;
    Ok(())
}

pub(super) fn codex_request_error(value: &Value) -> Option<String> {
    value.get("error").map(|error| {
        error
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| error.to_string())
    })
}

pub(super) fn codex_unattended_server_request_response(value: &Value) -> Option<Value> {
    if value.get("method").and_then(Value::as_str) != Some("mcpServer/elicitation/request") {
        return None;
    }
    let request_id = value.get("id")?;
    if !(request_id.is_string() || request_id.as_i64().is_some()) {
        return None;
    }
    Some(json!({
        "id": request_id.clone(),
        "result": {
            "action": "decline",
            "content": null,
            "_meta": null
        }
    }))
}

pub(super) fn codex_error_notification_detail(value: &Value) -> Option<String> {
    if value.pointer("/params/willRetry").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    value
        .pointer("/params/error/message")
        .or_else(|| value.pointer("/params/message"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| Some("codex emitted error notification".to_owned()))
}

/// The app-server tags errors caused by an invalid login (expired or revoked
/// ChatGPT tokens, a rejected API key) with `codexErrorInfo: "unauthorized"`,
/// both on `error` notifications and on a failed turn.
pub(super) fn codex_auth_failure(value: &Value) -> bool {
    [
        "/params/error/codexErrorInfo",
        "/params/turn/error/codexErrorInfo",
    ]
    .iter()
    .any(|pointer| value.pointer(pointer).and_then(Value::as_str) == Some("unauthorized"))
}

pub(super) fn codex_thread_id_from_response(value: &Value) -> Option<String> {
    value
        .pointer("/result/thread/id")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

pub(super) fn codex_turn_id_from_value(value: &Value) -> Option<String> {
    value
        .pointer("/result/turn/id")
        .or_else(|| value.pointer("/params/turn/id"))
        .or_else(|| value.pointer("/params/turnId"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

pub(super) fn codex_item_type(value: &Value) -> Option<&str> {
    value.pointer("/params/item/type").and_then(Value::as_str)
}

pub(super) fn codex_item_id(value: &Value) -> Option<&str> {
    value.pointer("/params/item/id").and_then(Value::as_str)
}

fn codex_item_summary(value: &Value) -> String {
    let Some(item) = value.pointer("/params/item") else {
        return "item".to_owned();
    };
    match item.get("type").and_then(Value::as_str).unwrap_or("item") {
        "commandExecution" => item
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("shell command")
            .to_owned(),
        "mcpToolCall" => format!(
            "{}.{}",
            item.get("server").and_then(Value::as_str).unwrap_or("mcp"),
            item.get("tool").and_then(Value::as_str).unwrap_or("tool")
        ),
        "dynamicToolCall" => format!(
            "{}{}",
            item.get("namespace")
                .and_then(Value::as_str)
                .map(|namespace| format!("{namespace}."))
                .unwrap_or_default(),
            item.get("tool").and_then(Value::as_str).unwrap_or("tool")
        ),
        "webSearch" => item
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("web search")
            .to_owned(),
        "fileChange" => "File change".to_owned(),
        "reasoning" => "Thinking".to_owned(),
        "agentMessage" => "Writing response".to_owned(),
        other => other.to_owned(),
    }
}

fn first_nonempty_item_value<'a>(item: &'a Value, fields: &[&str]) -> Option<&'a str> {
    fields
        .iter()
        .find_map(|field| item.pointer(field).or_else(|| item.get(*field)))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

fn codex_item_file_summary(item: &Value) -> String {
    let path = first_nonempty_item_value(
        item,
        &[
            "path",
            "file",
            "filePath",
            "filename",
            "/path",
            "/file",
            "/filePath",
            "/changes/0/path",
            "/changes/0/file",
        ],
    )
    .unwrap_or("file");
    let operation = first_nonempty_item_value(
        item,
        &["operation", "action", "change", "/operation", "/action"],
    )
    .unwrap_or("edit");
    json!({ "file": path, "operation": operation }).to_string()
}

pub(super) fn codex_item_started_activity(value: &Value) -> (&'static str, &'static str, String) {
    let Some(item) = value.pointer("/params/item") else {
        return ("activity", "Codex activity", "item".to_owned());
    };
    match item.get("type").and_then(Value::as_str).unwrap_or("item") {
        "reasoning" => ("thinking", "Thinking", "Thinking".to_owned()),
        "commandExecution" => (
            "command",
            "Running command",
            json!({ "command": codex_item_summary(value) }).to_string(),
        ),
        "fileChange" => ("file_edit", "Editing file", codex_item_file_summary(item)),
        "mcpToolCall" | "dynamicToolCall" | "webSearch" => {
            ("tools", "Using tool", codex_item_summary(value))
        }
        "agentMessage" => ("acting", "Writing response", "Writing response".to_owned()),
        _ => ("activity", "Codex activity", codex_item_summary(value)),
    }
}

fn first_nonempty_item_string<'a>(item: &'a Value, fields: &[&str]) -> Option<&'a str> {
    fields
        .iter()
        .find_map(|field| item.get(*field).and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty())
}

pub(super) fn codex_tool_completion_activity(
    value: &Value,
) -> Option<(&'static str, &'static str, String)> {
    let item = value.pointer("/params/item")?;
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or("item");
    if !matches!(
        item_type,
        "commandExecution" | "mcpToolCall" | "dynamicToolCall" | "webSearch" | "fileChange"
    ) {
        return None;
    }

    let (kind, title, mut metadata) = match item_type {
        "commandExecution" => (
            "command",
            "Command finished",
            json!({ "command": codex_item_summary(value) }),
        ),
        "fileChange" => (
            "file_edit",
            "File edit finished",
            serde_json::from_str(&codex_item_file_summary(item))
                .unwrap_or_else(|_| json!({ "file": "file" })),
        ),
        _ => (
            "tools",
            "Tool completed",
            json!({ "tool": codex_item_summary(value) }),
        ),
    };

    if let Some(object) = metadata.as_object_mut() {
        if let Some(exit_code) = item.get("exitCode").and_then(Value::as_i64) {
            object.insert("exit_code".to_owned(), json!(exit_code));
        }
        if let Some(status) = first_nonempty_item_string(item, &["status", "state"]) {
            object.insert("status".to_owned(), json!(status));
        }
        if let Some(output) =
            first_nonempty_item_string(item, &["output", "stdout", "stderr", "result", "error"])
        {
            object.insert("output".to_owned(), json!(truncate_activity_detail(output)));
        }
    }

    Some((kind, title, metadata.to_string()))
}

pub(super) fn effective_codex_cwd(working_directory: &str) -> CommandResult<String> {
    if working_directory.trim().is_empty() {
        Ok(env::current_dir()
            .map_err(to_string)?
            .to_string_lossy()
            .to_string())
    } else {
        Ok(working_directory.trim().to_owned())
    }
}

pub(super) fn codex_model_value(model: &str) -> Value {
    if model.trim().is_empty() {
        Value::Null
    } else {
        json!(model.trim())
    }
}

fn apply_codex_service_tier(params: &mut Value, service_tier: &str) {
    if let Some(object) = params.as_object_mut() {
        let service_tier = service_tier.trim();
        if !service_tier.is_empty() {
            object.insert("serviceTier".to_owned(), json!(service_tier));
        }
    }
}

pub(super) fn apply_codex_thread_options(params: &mut Value, service_tier: &str) {
    apply_codex_service_tier(params, service_tier);
}

pub(super) fn apply_codex_turn_options(
    params: &mut Value,
    reasoning_effort: &str,
    service_tier: &str,
) {
    if let Some(object) = params.as_object_mut() {
        let reasoning_effort = reasoning_effort.trim();
        if !reasoning_effort.is_empty() {
            object.insert("effort".to_owned(), json!(reasoning_effort));
        }
    }
    apply_codex_service_tier(params, service_tier);
}

#[cfg(test)]
mod tests {
    use super::{
        apply_codex_thread_options, apply_codex_turn_options,
        codex_context_rotate_input_tokens_from_env, codex_error_notification_detail,
        codex_item_started_activity, codex_turn_id_from_value,
        codex_unattended_server_request_response, CODEX_CONTEXT_ROTATE_DEFAULT_INPUT_TOKENS,
    };
    use crate::usage::{usage_from_run_log, usage_from_runtime_event};
    use serde_json::json;

    #[test]
    fn codex_runtime_options_match_current_app_server_fields() {
        let mut thread_params = json!({});
        apply_codex_thread_options(&mut thread_params, " fast ");
        assert_eq!(thread_params, json!({ "serviceTier": "fast" }));

        let mut turn_params = json!({});
        apply_codex_turn_options(&mut turn_params, " medium ", " fast ");
        assert_eq!(
            turn_params,
            json!({ "effort": "medium", "serviceTier": "fast" })
        );
    }

    #[test]
    fn codex_context_rotation_threshold_is_configurable_with_floor() {
        assert_eq!(
            codex_context_rotate_input_tokens_from_env(None),
            CODEX_CONTEXT_ROTATE_DEFAULT_INPUT_TOKENS
        );
        assert_eq!(
            codex_context_rotate_input_tokens_from_env(Some("220000")),
            220_000
        );
        assert_eq!(
            codex_context_rotate_input_tokens_from_env(Some("49999")),
            CODEX_CONTEXT_ROTATE_DEFAULT_INPUT_TOKENS
        );
        assert_eq!(
            codex_context_rotate_input_tokens_from_env(Some("not-a-number")),
            CODEX_CONTEXT_ROTATE_DEFAULT_INPUT_TOKENS
        );
    }

    #[test]
    fn extracts_codex_turn_ids_from_response_and_notification() {
        assert_eq!(
            codex_turn_id_from_value(&json!({"result": {"turn": {"id": "turn-1"}}})),
            Some("turn-1".to_owned())
        );
        assert_eq!(
            codex_turn_id_from_value(&json!({"params": {"turn": {"id": "turn-2"}}})),
            Some("turn-2".to_owned())
        );
        assert_eq!(
            codex_turn_id_from_value(&json!({"params": {"turnId": "turn-3"}})),
            Some("turn-3".to_owned())
        );
    }

    #[test]
    fn ignores_retryable_codex_error_notifications() {
        assert_eq!(
            codex_error_notification_detail(&json!({
                "method": "error",
                "params": {
                    "error": {"message": "Reconnecting... 2/5"},
                    "willRetry": true
                }
            })),
            None
        );
        assert_eq!(
            codex_error_notification_detail(&json!({
                "method": "error",
                "params": {
                    "error": {"message": "stream disconnected"},
                    "willRetry": false
                }
            })),
            Some("stream disconnected".to_owned())
        );
    }

    #[test]
    fn declines_codex_mcp_elicitations_with_the_original_request_id() {
        let request = json!({
            "method": "mcpServer/elicitation/request",
            "id": 0,
            "params": {
                "threadId": "thread-1",
                "serverName": "codex_apps",
                "mode": "form",
                "message": "Connect GitHub?",
                "requestedSchema": {"type": "object", "properties": {}}
            }
        });
        assert_eq!(
            codex_unattended_server_request_response(&request),
            Some(json!({
                "id": 0,
                "result": {
                    "action": "decline",
                    "content": null,
                    "_meta": null
                }
            }))
        );

        let request = json!({
            "method": "mcpServer/elicitation/request",
            "id": "elicitation-1",
            "params": {}
        });
        assert_eq!(
            codex_unattended_server_request_response(&request)
                .and_then(|response| response.get("id").cloned()),
            Some(json!("elicitation-1"))
        );
    }

    #[test]
    fn does_not_reuse_the_elicitation_response_for_other_codex_messages() {
        assert_eq!(
            codex_unattended_server_request_response(&json!({
                "method": "item/commandExecution/requestApproval",
                "id": 0,
                "params": {}
            })),
            None
        );
        assert_eq!(
            codex_unattended_server_request_response(&json!({
                "id": 0,
                "result": {"turn": {"id": "turn-1"}}
            })),
            None
        );
    }

    #[test]
    fn maps_codex_command_and_file_activity() {
        assert_eq!(
            codex_item_started_activity(&json!({
                "params": {
                    "item": {
                        "type": "commandExecution",
                        "command": "cargo test"
                    }
                }
            })),
            (
                "command",
                "Running command",
                json!({"command": "cargo test"}).to_string()
            )
        );
        assert_eq!(
            codex_item_started_activity(&json!({
                "params": {
                    "item": {
                        "type": "fileChange",
                        "path": "src/main.rs",
                        "operation": "update"
                    }
                }
            })),
            (
                "file_edit",
                "Editing file",
                json!({"file": "src/main.rs", "operation": "update"}).to_string()
            )
        );
    }

    #[test]
    fn parses_codex_thread_token_usage_events() {
        let value = json!({
            "method": "thread/tokenUsage/updated",
            "params": {
                "tokenUsage": {
                    "total": {
                        "inputTokens": 11488567,
                        "outputTokens": 36332
                    },
                    "last": {
                        "inputTokens": 33569,
                        "cachedInputTokens": 31616,
                        "outputTokens": 1278
                    }
                }
            }
        });
        assert_eq!(usage_from_runtime_event(&value), Some((33569, 1278)));

        let log = format!(
            "[codex] {{\"method\":\"item/agentMessage/delta\",\"params\":{{\"delta\":\"hi\"}}}}\n[codex] {value}"
        );
        assert_eq!(usage_from_run_log(&log), Some((33569, 1278)));
    }
}
