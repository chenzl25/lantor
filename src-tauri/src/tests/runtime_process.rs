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

async fn insert_cold_run(
    pool: &sqlx::SqlitePool,
    agent_id: uuid::Uuid,
    run_status: &str,
    work_status: &str,
) -> Result<(uuid::Uuid, uuid::Uuid), String> {
    let work_item_id: uuid::Uuid = sqlx::query_scalar(
        "insert into agent_work_items (agent_id, title, status) values ($1, 'cold', $2) returning id",
    )
    .bind(agent_id)
    .bind(work_status)
    .fetch_one(pool)
    .await
    .map_err(|err| err.to_string())?;
    let run_id: uuid::Uuid = sqlx::query_scalar(
        "insert into agent_runs (agent_id, work_item_id, command, status) values ($1, $2, 'true', $3) returning id",
    )
    .bind(agent_id)
    .bind(work_item_id)
    .bind(run_status)
    .fetch_one(pool)
    .await
    .map_err(|err| err.to_string())?;
    sqlx::query("update agent_work_items set run_id = $2 where id = $1")
        .bind(work_item_id)
        .bind(run_id)
        .execute(pool)
        .await
        .map_err(|err| err.to_string())?;
    Ok((run_id, work_item_id))
}

/// Run status, exit code, agent status, work item status, and whether the
/// run and the work item recorded when they stopped.
async fn cold_run_state(
    pool: &sqlx::SqlitePool,
    agent_id: uuid::Uuid,
    run_id: uuid::Uuid,
    work_item_id: uuid::Uuid,
) -> Result<(String, Option<i32>, String, String, bool, bool), String> {
    let (run_status, exit_code, run_stopped): (String, Option<i32>, bool) = sqlx::query_as(
        "select status, exit_code, stopped_at is not null from agent_runs where id = $1",
    )
    .bind(run_id)
    .fetch_one(pool)
    .await
    .map_err(|err| err.to_string())?;
    let agent_status: String = sqlx::query_scalar("select status from agents where id = $1")
        .bind(agent_id)
        .fetch_one(pool)
        .await
        .map_err(|err| err.to_string())?;
    let (work_status, work_completed): (String, bool) = sqlx::query_as(
        "select status, completed_at is not null from agent_work_items where id = $1",
    )
    .bind(work_item_id)
    .fetch_one(pool)
    .await
    .map_err(|err| err.to_string())?;
    Ok((
        run_status,
        exit_code,
        agent_status,
        work_status,
        run_stopped,
        work_completed,
    ))
}

#[tokio::test]
async fn cold_process_exit_settles_run_agent_and_work_item_together() {
    use crate::test_support::{
        drop_test_schema, insert_test_agent, run_and_work_item_events, test_pool, ui_event_cursor,
    };
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        // (exit code, run status before exit, work status before exit) ->
        // (run status, exit code, agent status, work status)
        let cases = [
            (
                (0, "running", "running"),
                ("exited", Some(0), "idle", "done"),
            ),
            (
                (3, "running", "running"),
                ("stopped", Some(3), "idle", "failed"),
            ),
            (
                (0, "running", "cancelling"),
                ("exited", Some(0), "idle", "cancelled"),
            ),
            (
                (0, "running", "silent"),
                ("exited", Some(0), "idle", "silent"),
            ),
            (
                (3, "running", "silent"),
                ("stopped", Some(3), "idle", "failed"),
            ),
            (
                (0, "failed", "running"),
                ("failed", None, "error", "failed"),
            ),
        ];
        for (index, ((exit_code, run_status, work_status), expected)) in
            cases.into_iter().enumerate()
        {
            let agent_id = insert_test_agent(&pool, &format!("cold-exit-{index}")).await?;
            sqlx::query("update agents set status = 'running' where id = $1")
                .bind(agent_id)
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            let (run_id, work_item_id) =
                insert_cold_run(&pool, agent_id, run_status, work_status).await?;
            let child = tokio::process::Command::new("sh")
                .arg("-c")
                .arg(format!("exit {exit_code}"))
                .spawn()
                .map_err(|err| err.to_string())?;
            let cursor = ui_event_cursor(&pool).await?;

            super::wait_for_agent_run(
                pool.clone(),
                agent_id,
                run_id,
                Some(work_item_id),
                Vec::new(),
                child,
            )
            .await;

            let (expected_run, expected_exit, expected_agent, expected_work) = expected;
            assert_eq!(
                cold_run_state(&pool, agent_id, run_id, work_item_id).await?,
                (
                    expected_run.to_owned(),
                    expected_exit,
                    expected_agent.to_owned(),
                    expected_work.to_owned(),
                    true,
                    true,
                ),
                "case {index}"
            );
            assert_eq!(
                run_and_work_item_events(&pool, cursor).await?,
                [
                    format!("agent_run_upsert:run_finished:{expected_run}"),
                    format!("work_item_upsert:work_item_finished:{expected_work}"),
                ],
                "case {index}"
            );
        }
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn cold_launch_failure_fails_run_agent_and_work_item_together() {
    use crate::test_support::{
        drop_test_schema, insert_test_agent, run_and_work_item_events, test_pool, ui_event_cursor,
    };
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "cold-launch").await?;
        let work_item_id: uuid::Uuid = sqlx::query_scalar(
            "insert into agent_work_items (agent_id, title, status) values ($1, 'cold', 'queued') returning id",
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let cursor = ui_event_cursor(&pool).await?;

        // A working directory that does not exist makes the spawn fail.
        let error = super::start_process_agent(
            &pool,
            super::ProcessAgentLaunch {
                agent_id,
                work_item_id: Some(work_item_id),
                handle: "cold-launch".to_owned(),
                working_directory: "/nonexistent/lantor-cold-launch-test".to_owned(),
                command_text: "true".to_owned(),
                environment_variables: String::new(),
                work_item_prompt: String::new(),
                rendered_context_max_seq: 0,
            },
        )
        .await
        .unwrap_err();
        assert!(!error.is_empty());

        let run_id: uuid::Uuid =
            sqlx::query_scalar("select run_id from agent_work_items where id = $1")
                .bind(work_item_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(
            cold_run_state(&pool, agent_id, run_id, work_item_id).await?,
            (
                "failed".to_owned(),
                None,
                "error".to_owned(),
                "failed".to_owned(),
                true,
                true,
            )
        );
        let log: String = sqlx::query_scalar("select log from agent_runs where id = $1")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert!(log.starts_with("$ true\n"), "{log}");
        assert!(log.contains("failed to start process: "), "{log}");
        assert_eq!(
            run_and_work_item_events(&pool, cursor).await?,
            [
                "agent_run_upsert:run_created:starting",
                "work_item_upsert:work_item_running:running",
                "agent_run_upsert:run_failed:failed",
                "work_item_upsert:work_item_failed:failed",
            ]
        );
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn process_launch_marks_the_rendered_surface_context_as_seen() {
    use crate::test_support::{
        drop_test_schema, insert_test_agent, insert_test_channel, test_pool,
    };
    // Process runtimes launch through /bin/zsh, which Linux CI runners lack.
    if !std::path::Path::new("/bin/zsh").exists() {
        eprintln!("skipping process launch test: /bin/zsh is missing");
        return;
    }
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "seen-launch").await?;
        let channel_id = insert_test_channel(&pool, "seen-launch").await?;
        let message_id: uuid::Uuid = sqlx::query_scalar(
            r#"
            insert into messages (channel_id, sender_name, sender_role, body, is_task)
            values ($1, 'Dylan', 'owner', 'shown in the prompt', false)
            returning id
            "#,
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        // seq is assigned by an after-insert trigger, so read it back.
        let seq: i64 = sqlx::query_scalar("select seq from messages where id = $1")
            .bind(message_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert!(seq > 0);
        let work_item_id: uuid::Uuid = sqlx::query_scalar(
            r#"
            insert into agent_work_items (agent_id, channel_id, title, status)
            values ($1, $2, 'seen', 'queued')
            returning id
            "#,
        )
        .bind(agent_id)
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;

        super::start_process_agent(
            &pool,
            super::ProcessAgentLaunch {
                agent_id,
                work_item_id: Some(work_item_id),
                handle: "seen-launch".to_owned(),
                working_directory: std::env::temp_dir().to_string_lossy().into_owned(),
                command_text: "true".to_owned(),
                environment_variables: String::new(),
                work_item_prompt: "prompt".to_owned(),
                rendered_context_max_seq: seq,
            },
        )
        .await?;

        // The work item stored no context, so only the rendered surface can
        // have moved the watermark.
        let seen: i64 = sqlx::query_scalar(
            r#"
            select last_seen_seq
            from agent_target_watermarks
            where agent_id = $1 and channel_id = $2 and thread_root_id is null
            "#,
        )
        .bind(agent_id)
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(seen, seq);

        // Let the run settle before the database is dropped.
        for _ in 0..100 {
            let status: String =
                sqlx::query_scalar("select status from agent_work_items where id = $1")
                    .bind(work_item_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            if status != "queued" && status != "running" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}
