use serde_json::json;

use super::{
    agy_command_text, agy_init_conversation_id, agy_model_and_effort, agy_step_usage,
    agy_text_delta, agy_tool_started, agy_turn_result, agy_user_input, AgyTurnResult,
};
use crate::usage::RunUsage;

const CONVERSATION: &str = "7faa23ee-10ed-4ea4-bacd-b8f22c7d4069";

#[test]
fn a_prompt_is_one_user_event() {
    assert_eq!(
        agy_user_input("hello"),
        json!({ "event": "user", "message": { "content": "hello" } })
    );
}

#[test]
fn model_and_effort_flags_split_legacy_suffixes_and_avoid_conflicts() {
    assert_eq!(
        agy_model_and_effort("gemini-4-neon", "xhigh"),
        ("gemini-4-neon".to_owned(), "xhigh".to_owned())
    );
    assert_eq!(
        agy_model_and_effort("gemini-4-neon-xhigh", ""),
        ("gemini-4-neon".to_owned(), "xhigh".to_owned())
    );
    assert_eq!(
        agy_model_and_effort("gemini-4-neon-xhigh", "low"),
        ("gemini-4-neon".to_owned(), "low".to_owned())
    );
    assert_eq!(
        agy_model_and_effort("gemini-pro-orchestrator", ""),
        ("gemini-pro-orchestrator".to_owned(), String::new())
    );
    assert_eq!(
        agy_command_text("gemini-4-neon", "xhigh"),
        "agy --input-format stream-json --output-format stream-json --dangerously-skip-permissions --model gemini-4-neon --effort xhigh -p="
    );
    assert_eq!(
        agy_command_text("gemini-pro-orchestrator", ""),
        "agy --input-format stream-json --output-format stream-json --dangerously-skip-permissions --model gemini-pro-orchestrator -p="
    );
}

#[test]
fn init_names_the_conversation() {
    let init = json!({
        "event": "init",
        "conversation_id": CONVERSATION,
        "init": { "cwd": "/tmp/agent", "tools": ["run_command"], "permission_mode": "bypass" }
    });
    assert_eq!(agy_init_conversation_id(&init), Some(CONVERSATION));
    let result = json!({ "event": "result", "result": { "conversation_id": CONVERSATION } });
    assert_eq!(agy_init_conversation_id(&result), None);
}

#[test]
fn reply_text_comes_from_agent_response_steps() {
    let delta = json!({ "event": "step_update", "step_update": {
        "conversation_id": CONVERSATION, "step_index": 3, "state": "ACTIVE",
        "step_type": "agent_response", "text_delta": "The command "
    }});
    assert_eq!(agy_text_delta(&delta), Some((3, "The command ")));
    let tool = json!({ "event": "step_update", "step_update": {
        "step_index": 2, "state": "ACTIVE", "step_type": "tool", "tool_name": "run_command",
        "text_delta": "not reply text"
    }});
    assert_eq!(agy_text_delta(&tool), None);
    let done_without_text = json!({ "event": "step_update", "step_update": {
        "step_index": 1, "state": "DONE", "step_type": "agent_response"
    }});
    assert_eq!(agy_text_delta(&done_without_text), None);
}

#[test]
fn tool_steps_map_to_command_edit_and_other_activities() {
    let step = |state: &str, name: &str, parameters: serde_json::Value| {
        json!({ "event": "step_update", "step_update": {
            "step_index": 2, "state": state, "step_type": "tool", "tool_name": name,
            "tool_info": { "name": name, "parameters": parameters }
        }})
    };
    assert_eq!(
        agy_tool_started(&step(
            "ACTIVE",
            "run_command",
            json!({ "CommandLine": "echo hi" })
        )),
        Some(("command", "Running command", "echo hi".to_owned()))
    );
    assert_eq!(
        agy_tool_started(&step(
            "ACTIVE",
            "write_file",
            json!({ "Path": "/tmp/agent/a.md" })
        )),
        Some(("file_edit", "Editing file", "/tmp/agent/a.md".to_owned()))
    );
    assert_eq!(
        agy_tool_started(&step("ACTIVE", "browser_get_dom", json!({}))),
        Some(("tools", "Using tool", "browser_get_dom".to_owned()))
    );
    assert_eq!(
        agy_tool_started(&step(
            "DONE",
            "run_command",
            json!({ "CommandLine": "echo hi" })
        )),
        None,
        "a finished tool step is not a new call"
    );
}

#[test]
fn usage_counts_cache_reads_as_cached_input_and_thinking_as_output() {
    let step = json!({ "event": "step_update", "step_update": {
        "step_index": 3, "state": "DONE", "step_type": "agent_response",
        "usage": { "input_tokens": 2234, "output_tokens": 29, "thinking_tokens": 19,
                   "cache_read_tokens": 6739, "total_tokens": 2263 }
    }});
    assert_eq!(
        agy_step_usage(&step),
        Some((
            RunUsage {
                input_tokens: 2234,
                cached_input_tokens: 6739,
                cache_write_tokens: 0,
                output_tokens: 48,
            },
            8973
        ))
    );
}

#[test]
fn result_reports_success_or_the_error() {
    let success = json!({ "event": "result", "result": {
        "conversation_id": CONVERSATION, "status": "SUCCESS",
        "response": "The command output `lantor-probe`.\n", "num_turns": 1,
        "usage": { "input_tokens": 11030, "output_tokens": 109, "thinking_tokens": 54,
                   "cache_read_tokens": 6739, "total_tokens": 11139 }
    }});
    assert_eq!(
        agy_turn_result(&success),
        Some(AgyTurnResult {
            success: true,
            response: "The command output `lantor-probe`.\n".to_owned(),
            error: None,
            usage: Some(RunUsage {
                input_tokens: 11030,
                cached_input_tokens: 6739,
                cache_write_tokens: 0,
                output_tokens: 163,
            }),
        })
    );

    let failed = json!({ "event": "result", "result": {
        "status": "ERROR", "error": "authentication required", "response": ""
    }});
    let failed = agy_turn_result(&failed).expect("a result");
    assert!(!failed.success);
    assert_eq!(failed.error.as_deref(), Some("authentication required"));

    let canceled = json!({ "event": "result", "result": { "status": "CANCELED" } });
    assert_eq!(
        agy_turn_result(&canceled).and_then(|result| result.error),
        Some("agy ended the turn with status CANCELED".to_owned())
    );
}
