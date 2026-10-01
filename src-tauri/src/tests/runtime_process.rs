use super::classify_agent_output_activity;
use crate::events::activity::activity_status;
use serde_json::{json, Value};

#[test]
fn process_group_signals_separate_negative_pid_from_options() {
    for signal in ["-TERM", "-0", "-KILL"] {
        let command = super::process_group_signal_command(119669, signal);
        let args: Vec<_> = command.as_std().get_args().collect();
        assert_eq!(args, [signal, "--", "-119669"]);
    }
}

#[tokio::test]
async fn process_group_termination_rejects_broadcast_and_invalid_targets() {
    for pid in [i32::MIN, -123, -1, 0, 1] {
        let error = super::terminate_process_group(pid).await.unwrap_err();
        assert!(error.contains("invalid process group leader pid"));
    }
}

#[test]
fn ignores_known_codex_manifest_default_prompt_warning() {
    let line = json!({
        "timestamp": "2026-05-14T13:05:55.340546Z",
        "level": "WARN",
        "fields": {
            "message": "ignoring interface.defaultPrompt: maximum length exceeded"
        },
        "target": "codex_core_plugins::manifest"
    })
    .to_string();

    assert_eq!(classify_agent_output_activity("stderr", &line), None);
    assert_eq!(classify_agent_output_activity("stdout", &line), None);
}

#[test]
fn ignores_known_codex_skill_loader_icon_warning() {
    for message in [
        "ignoring interface.icon_small: icon path must not contain '..'",
        "ignoring interface.icon_large: icon path must not contain '..'",
    ] {
        let line = json!({
            "timestamp": "2026-05-14T13:05:55.340546Z",
            "level": "WARN",
            "fields": {
                "message": message
            },
            "target": "codex_core_skills::loader"
        })
        .to_string();

        assert_eq!(classify_agent_output_activity("stderr", &line), None);
        assert_eq!(classify_agent_output_activity("stdout", &line), None);
    }
}

#[test]
fn maps_structured_stderr_warning_to_runtime_warning() {
    let line = json!({
        "timestamp": "2026-05-14T13:05:55.340546Z",
        "level": "WARN",
        "fields": {
            "message": "plugin manifest used a deprecated field"
        },
        "target": "codex_core_plugins::manifest"
    })
    .to_string();
    let activity =
        classify_agent_output_activity("stderr", &line).expect("warning should be classified");

    assert_eq!(activity.0, "run");
    assert_eq!(activity.1, "Runtime warning");
    let detail: Value = serde_json::from_str(&activity.2).expect("structured detail");
    assert_eq!(detail["level"], "WARN");
    assert_eq!(detail["target"], "codex_core_plugins::manifest");
    assert_eq!(detail["message"], "plugin manifest used a deprecated field");

    let stdout_activity =
        classify_agent_output_activity("stdout", &line).expect("warning should be classified");
    assert_eq!(stdout_activity.0, "run");
    assert_eq!(stdout_activity.1, "Runtime warning");
}

#[test]
fn ignores_known_codex_legacy_notify_hook_warning() {
    let line = json!({
        "timestamp": "2026-05-14T13:16:30.388210Z",
        "level": "WARN",
        "fields": {
            "error": "No such file or directory (os error 2)",
            "hook_name": "legacy_notify",
            "message": "after_agent hook failed; continuing",
            "turn_id": "019e26a1-7ad5-7642-af8a-e042a0738a84"
        },
        "target": "codex_core::session::turn"
    })
    .to_string();

    assert_eq!(classify_agent_output_activity("stderr", &line), None);
}

#[test]
fn maps_structured_warning_with_error_words_to_runtime_warning() {
    let line = json!({
        "timestamp": "2026-05-14T13:16:30.388210Z",
        "level": "WARN",
        "fields": {
            "error": "retryable operation failed once",
            "message": "operation failed; continuing"
        },
        "target": "codex_core::session::turn"
    })
    .to_string();
    let activity =
        classify_agent_output_activity("stderr", &line).expect("warning should be classified");

    assert_eq!(activity.0, "run");
    assert_eq!(activity.1, "Runtime warning");
    assert_eq!(activity_status(activity.0, activity.1), "warning");
}

#[test]
fn downgrades_retryable_codex_infra_stderr_to_warning() {
    for line in [
        "\u{1b}[2m2026-05-18T14:32:02.340702Z\u{1b}[0m \u{1b}[31mERROR\u{1b}[0m \u{1b}[2mcodex_api::endpoint::responses_websocket\u{1b}[0m\u{1b}[2m:\u{1b}[0m failed to connect to websocket: IO error: tls handshake eof",
        "\u{1b}[2m2026-05-18T14:32:21.393770Z\u{1b}[0m \u{1b}[31mERROR\u{1b}[0m \u{1b}[2mcodex_models_manager::manager\u{1b}[0m\u{1b}[2m:\u{1b}[0m failed to refresh available models: timeout waiting for child process to exit",
        "\u{1b}[2m2026-05-18T14:31:57.219245Z\u{1b}[0m \u{1b}[31mERROR\u{1b}[0m \u{1b}[2mrmcp::transport::worker\u{1b}[0m\u{1b}[2m:\u{1b}[0m worker quit with fatal: Transport channel closed, when Client(HttpRequest(HttpRequest(\"http/request failed: error sending request for url (https://chatgpt.com/backend-api/wham/apps)\")))",
    ] {
        let activity = classify_agent_output_activity("stderr", line)
            .expect("retryable stderr should remain visible");
        assert_eq!(activity.0, "run");
        assert_eq!(activity.1, "Runtime warning");
        assert_eq!(activity_status(activity.0, activity.1), "warning");
    }
}

#[test]
fn maps_unclassified_stderr_to_runtime_output_not_thinking() {
    assert_eq!(
        classify_agent_output_activity("stderr", "runtime heartbeat"),
        Some(("run", "Runtime output", "runtime heartbeat".to_owned()))
    );
    assert_eq!(
        classify_agent_output_activity("stdout", "model is considering options"),
        Some((
            "thinking",
            "Thinking",
            "model is considering options".to_owned()
        ))
    );
}
