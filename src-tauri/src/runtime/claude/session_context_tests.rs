use std::time::Duration;

use serde_json::json;

use super::*;
use crate::freshness::advance_agent_target_watermark;
use crate::runtime::session_context::{
    load_surface_delivery, store_injected_memory, store_surface_delivery, SurfaceContextDelta,
};
use crate::test_support::{drop_test_schema, insert_test_agent, insert_test_channel, test_pool};

const SESSION: &str = "test-claude-session";

async fn claude_agent_in_channel(pool: &SqlitePool, handle: &str) -> (Uuid, Uuid) {
    let agent = insert_test_agent(pool, handle).await.unwrap();
    sqlx::query("update agents set runtime = 'claude' where id = $1")
        .bind(agent)
        .execute(pool)
        .await
        .unwrap();
    let channel = insert_test_channel(pool, handle).await.unwrap();
    sqlx::query("insert into channel_members (channel_id, agent_id) values ($1, $2)")
        .bind(channel)
        .bind(agent)
        .execute(pool)
        .await
        .unwrap();
    // A turn's start creates these rows before any turn ends.
    advance_agent_target_watermark(pool, agent, Some(channel), None, 1)
        .await
        .unwrap();
    upsert_runtime_thread_id(pool, agent, "claude", SESSION, "idle")
        .await
        .unwrap();
    (agent, channel)
}

async fn work_item(pool: &SqlitePool, agent: Uuid, channel: Uuid, status: &str) -> Uuid {
    sqlx::query_scalar(
        "insert into agent_work_items (agent_id, channel_id, title, status) values ($1, $2, 'request', $3) returning id",
    )
    .bind(agent)
    .bind(channel)
    .bind(status)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Starts a turn on an idle process that resumed `SESSION` and returns the
/// prompt Lantor wrote to the CLI.
async fn prompt_sent_to(delta_thread_id: &str) -> String {
    let (pool, database) = test_pool().await.expect("SQLite fixture must initialize");
    let (agent, channel) = claude_agent_in_channel(&pool, "claude-delta").await;
    let work = work_item(&pool, agent, channel, "queued").await;

    let input = std::env::temp_dir().join(format!("lantor-claude-delta-{work}.jsonl"));
    let mut child = Command::new("sh")
        .arg("-c")
        .arg("cat > \"$1\"")
        .arg("sh")
        .arg(&input)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    let registry = WarmClaudeRegistry::default();
    registry
        .insert(
            agent,
            Arc::new(WarmClaudeRuntime {
                stdin: AsyncMutex::new(stdin),
                state: AsyncMutex::new(WarmClaudeState {
                    alive: true,
                    active: None,
                    session_id: Some(SESSION.to_owned()),
                    context_tokens: 0,
                    last_surface: None,
                    injected_memory_context: None,
                    last_activity: Instant::now(),
                    pending_steers: Vec::new(),
                }),
                pid: None,
                environment_variables: String::new(),
                provider: ClaudeCli,
            }),
        )
        .await;

    supervisor_start_claude_streaming_agent(
        &pool,
        &registry,
        agent,
        Some(work),
        "claude-delta".to_owned(),
        "opus".to_owned(),
        String::new(),
        String::new(),
        String::new(),
        "FULL PROMPT".to_owned(),
        7,
        SurfaceDelivery {
            delivered_seq: 7,
            delta: Some(SurfaceContextDelta {
                thread_id: delta_thread_id.to_owned(),
                prompt: "DELTA PROMPT".to_owned(),
            }),
        },
        None,
    )
    .await
    .unwrap();

    let runtime = registry.get(agent).await.unwrap();
    assert_eq!(
        runtime
            .state
            .lock()
            .await
            .active
            .as_ref()
            .and_then(|active| active.surface_delivered_seq),
        Some(7)
    );
    let mut written = String::new();
    for _ in 0..200 {
        written = std::fs::read_to_string(&input).unwrap_or_default();
        if written.contains('\n') {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let _ = child.kill().await;
    let _ = std::fs::remove_file(&input);
    drop_test_schema(pool, database).await;
    written
}

#[tokio::test]
async fn the_session_that_holds_the_surface_gets_the_shorter_prompt() {
    let written = prompt_sent_to(SESSION).await;
    assert!(written.contains("DELTA PROMPT"), "{written}");
    assert!(!written.contains("FULL PROMPT"), "{written}");
}

#[tokio::test]
async fn another_session_gets_the_whole_prompt() {
    let written = prompt_sent_to("some-earlier-session").await;
    assert!(written.contains("FULL PROMPT"), "{written}");
    assert!(!written.contains("DELTA PROMPT"), "{written}");
}

/// A running turn whose prompt brought the surface up to seq 7 and a new
/// memory snapshot.
struct RunningTurn {
    pool: SqlitePool,
    database: String,
    agent: Uuid,
    channel: Uuid,
    runtime: Arc<WarmClaudeRuntime>,
}

impl RunningTurn {
    async fn new(handle: &str) -> Self {
        let (pool, database) = test_pool().await.expect("SQLite fixture must initialize");
        let (agent, channel) = claude_agent_in_channel(&pool, handle).await;
        let work = work_item(&pool, agent, channel, "running").await;
        let run: Uuid = sqlx::query_scalar(
            "insert into agent_runs (agent_id, work_item_id, command, status) values ($1, $2, 'claude', 'running') returning id",
        )
        .bind(agent)
        .bind(work)
        .fetch_one(&pool)
        .await
        .unwrap();
        let key = claude_stream_key(run);
        let runtime = super::tests::test_runtime_with_active_turn(run, work, channel, key.clone())
            .await
            .unwrap();
        ensure_streaming_agent_message(&pool, agent, channel, None, &key)
            .await
            .unwrap();
        {
            let mut state = runtime.state.lock().await;
            let active = state.active.as_mut().unwrap();
            active.surface_delivered_seq = Some(7);
            active.pending_memory_context = Some(Some("memory v2".to_owned()));
        }
        Self {
            pool,
            database,
            agent,
            channel,
            runtime,
        }
    }

    async fn send(&self, event: Value) {
        handle_claude_warm_stdout_line(&self.pool, self.agent, &self.runtime, &event.to_string())
            .await
            .unwrap();
    }

    /// What the session holds on record: the surface seq and the memory.
    async fn held(&self) -> (Option<(String, i64)>, Option<String>) {
        (
            load_surface_delivery(&self.pool, self.agent, self.channel, None)
                .await
                .unwrap(),
            load_injected_memory(&self.pool, self.agent, "claude", SESSION)
                .await
                .unwrap(),
        )
    }

    async fn close(self) {
        drop_test_schema(self.pool, self.database).await;
    }
}

#[tokio::test]
async fn a_finished_turn_records_what_its_prompt_gave_the_session() {
    let turn = RunningTurn::new("claude-held").await;
    assert_eq!(turn.held().await, (None, None));
    turn.send(json!({"type":"result","subtype":"success","is_error":false,"result":"done"}))
        .await;
    assert_eq!(
        turn.held().await,
        (Some((SESSION.to_owned(), 7)), Some("memory v2".to_owned()))
    );
    turn.close().await;
}

#[tokio::test]
async fn a_failed_turn_records_nothing() {
    let turn = RunningTurn::new("claude-failed").await;
    finish_warm_claude_active_turn(
        &turn.pool,
        turn.agent,
        &turn.runtime,
        false,
        Some("the provider gave up".to_owned()),
    )
    .await
    .unwrap();
    assert_eq!(turn.held().await, (None, None));
    turn.close().await;
}

#[tokio::test]
async fn a_compacted_session_is_given_everything_again() {
    let turn = RunningTurn::new("claude-compacted").await;
    // Earlier turns left the session holding the surface and a snapshot.
    store_surface_delivery(&turn.pool, turn.agent, turn.channel, None, SESSION, 5)
        .await
        .unwrap();
    store_injected_memory(&turn.pool, turn.agent, "claude", SESSION, Some("memory v1"))
        .await
        .unwrap();
    turn.runtime.state.lock().await.injected_memory_context = Some("memory v1".to_owned());

    turn.send(json!({
        "type": "system",
        "subtype": "compact_boundary",
        "compact_metadata": {"trigger": "auto", "pre_tokens": 970000, "post_tokens": 40000}
    }))
    .await;
    assert_eq!(turn.held().await, (None, None));

    // The turn that was compacted does not record its own prompt either: it
    // is part of the summary now.
    turn.send(json!({"type":"result","subtype":"success","is_error":false,"result":"done"}))
        .await;
    assert_eq!(turn.held().await, (None, None));
    assert_eq!(
        turn.runtime.state.lock().await.injected_memory_context,
        None
    );
    turn.close().await;
}
