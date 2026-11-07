use std::time::Duration;

use super::interrupt::{interrupt_warm_claude_turn, terminate_if_interrupt_ignored};
use super::*;
use crate::test_support::{drop_test_schema, insert_test_agent, test_pool};

fn active_turn(run_id: Uuid, interrupt_requested: bool) -> ClaudeActiveTurn {
    ClaudeActiveTurn {
        run_id,
        started_at: Instant::now(),
        first_delta_at: None,
        work_item_id: None,
        channel_id: None,
        thread_root_id: None,
        stream_key: claude_stream_key(run_id),
        pending_memory_context: None,
        text: ClaudeTextState::default(),
        interrupt_requested,
        auth_required: false,
        network_failure: false,
    }
}

/// A warm runtime whose stdin feeds `child`.
fn runtime_over(
    child: &mut tokio::process::Child,
    pid: Option<i32>,
    active: Option<ClaudeActiveTurn>,
) -> Arc<WarmClaudeRuntime> {
    let stdin = child.stdin.take().expect("child stdin is piped");
    Arc::new(WarmClaudeRuntime {
        stdin: AsyncMutex::new(stdin),
        state: AsyncMutex::new(WarmClaudeState {
            alive: true,
            active,
            session_id: Some("test-claude-session".to_owned()),
            context_tokens: 0,
            pending_rotation_marker: None,
            last_surface: None,
            injected_memory_context: None,
            last_activity: Instant::now(),
            pending_steers: Vec::new(),
        }),
        pid,
        environment_variables: String::new(),
        provider: ClaudeCli,
    })
}

async fn insert_running_run(pool: &SqlitePool, agent_id: Uuid) -> Uuid {
    sqlx::query("update agents set status = 'running', runtime = 'claude' where id = $1")
        .bind(agent_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query_scalar(
        "insert into agent_runs (agent_id, command, status) values ($1, 'claude', 'running') returning id",
    )
    .bind(agent_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn interrupt_writes_one_control_request_and_finishes_as_stop() {
    let (pool, database) = test_pool().await.expect("SQLite fixture must initialize");
    let agent_id = insert_test_agent(&pool, "claude-interrupt").await.unwrap();
    let run_id = insert_running_run(&pool, agent_id).await;
    let input = std::env::temp_dir().join(format!("lantor-claude-interrupt-{run_id}.jsonl"));
    let mut child = Command::new("sh")
        .arg("-c")
        .arg("cat > \"$1\"")
        .arg("sh")
        .arg(&input)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let runtime = runtime_over(&mut child, None, Some(active_turn(run_id, false)));

    assert!(!interrupt_warm_claude_turn(&pool, &runtime, Uuid::new_v4())
        .await
        .unwrap());
    assert!(interrupt_warm_claude_turn(&pool, &runtime, run_id)
        .await
        .unwrap());
    // A repeated stop is accepted without writing a second request.
    assert!(interrupt_warm_claude_turn(&pool, &runtime, run_id)
        .await
        .unwrap());

    // The CLI ends an interrupted turn with an error result.
    finish_warm_claude_active_turn(
        &pool,
        agent_id,
        &runtime,
        false,
        Some("error_during_execution".to_owned()),
    )
    .await
    .unwrap();
    let run_status: String = sqlx::query_scalar("select status from agent_runs where id = $1")
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(run_status, "cancelled");
    let agent_status: String = sqlx::query_scalar("select status from agents where id = $1")
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(agent_status, "idle");
    {
        let state = runtime.state.lock().await;
        assert!(state.alive, "a stop must keep the warm process");
        assert!(state.active.is_none());
    }

    drop(runtime);
    child.wait().await.unwrap();
    let written = std::fs::read_to_string(&input).unwrap();
    let _ = std::fs::remove_file(&input);
    let lines = written.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 1, "{written}");
    let request: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(request["type"], "control_request");
    assert_eq!(request["request"]["subtype"], "interrupt");
    assert_eq!(
        request["request_id"],
        format!("lantor-interrupt-{run_id}").as_str()
    );
    drop_test_schema(pool, database).await;
}

#[tokio::test]
async fn ignored_interrupt_falls_back_to_terminating_the_process_group() {
    let (pool, database) = test_pool().await.expect("SQLite fixture must initialize");
    let agent_id = insert_test_agent(&pool, "claude-interrupt-ignored")
        .await
        .unwrap();
    let run_id = insert_running_run(&pool, agent_id).await;
    // Its own process group, so the signal cannot reach the test runner.
    let mut command = Command::new("sleep");
    command.arg("30").stdin(Stdio::piped());
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().unwrap();
    let pid = child.id().map(|id| id as i32);
    let runtime = runtime_over(&mut child, pid, None);

    // A finished turn is left alone.
    terminate_if_interrupt_ignored(
        pool.clone(),
        agent_id,
        runtime.clone(),
        run_id,
        Duration::ZERO,
    )
    .await;
    assert!(child.try_wait().unwrap().is_none());

    runtime.state.lock().await.active = Some(active_turn(run_id, true));
    terminate_if_interrupt_ignored(
        pool.clone(),
        agent_id,
        runtime.clone(),
        run_id,
        Duration::ZERO,
    )
    .await;
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("SIGTERM must stop the process group")
        .unwrap();
    assert!(!status.success());
    let log: String = sqlx::query_scalar("select log from agent_runs where id = $1")
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(log.contains("after interrupt; sent SIGTERM"), "{log}");
    drop_test_schema(pool, database).await;
}
