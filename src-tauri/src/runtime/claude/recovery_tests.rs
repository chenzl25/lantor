use super::*;
use crate::test_support::{drop_test_schema, insert_test_agent, insert_test_channel, test_pool};
use serde_json::json;

struct Fixture {
    pool: SqlitePool,
    database: String,
    agent: Uuid,
    channel: Uuid,
    run: Uuid,
    key: String,
    runtime: Arc<WarmClaudeRuntime>,
}

impl Fixture {
    async fn new() -> Self {
        let (pool, database) = test_pool().await.expect("SQLite fixture must initialize");
        let agent = insert_test_agent(&pool, "claude-recovery").await.unwrap();
        let channel = insert_test_channel(&pool, "claude-recovery").await.unwrap();
        sqlx::query("insert into channel_members (channel_id, agent_id) values ($1, $2)")
            .bind(channel)
            .bind(agent)
            .execute(&pool)
            .await
            .unwrap();
        let work: Uuid = sqlx::query_scalar("insert into agent_work_items (agent_id, channel_id, title, status) values ($1, $2, 'recover reply', 'running') returning id")
            .bind(agent).bind(channel).fetch_one(&pool).await.unwrap();
        let run: Uuid = sqlx::query_scalar("insert into agent_runs (agent_id, work_item_id, command, status) values ($1, $2, 'claude', 'running') returning id")
            .bind(agent).bind(work).fetch_one(&pool).await.unwrap();
        let key = claude_stream_key(run);
        let runtime = super::tests::test_runtime_with_active_turn(run, work, channel, key.clone())
            .await
            .unwrap();
        ensure_streaming_agent_message(&pool, agent, channel, None, &key)
            .await
            .unwrap();
        Self {
            pool,
            database,
            agent,
            channel,
            run,
            key,
            runtime,
        }
    }

    async fn send(&self, event: Value) {
        handle_claude_warm_stdout_line(&self.pool, self.agent, &self.runtime, &event.to_string())
            .await
            .unwrap();
    }

    async fn stream(&self, text: &str) {
        self.send(json!({"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}})).await;
        self.send(json!({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}})).await;
    }

    async fn assistant(&self, id: &str, text: &str) {
        self.send(json!({"type":"assistant","uuid":id,"message":{"id":id,"content":[{"type":"text","text":text}]}})).await;
    }

    async fn result(&self, text: &str) {
        self.send(json!({"type":"result","subtype":"success","is_error":false,"result":text}))
            .await;
    }

    async fn body(&self) -> String {
        sqlx::query_scalar("select body from messages where stream_key=$1")
            .bind(&self.key)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn close(self) {
        drop_test_schema(self.pool, self.database).await;
    }
}

#[tokio::test]
async fn activity_only_result_removes_placeholder_after_processing_controls() {
    let f = Fixture::new().await;
    let progress = "LANTOR_EVENT {\"type\":\"activity\",\"kind\":\"command\",\"title\":\"Checking the build\"}";
    f.stream(progress).await;
    f.assistant("progress", progress).await;
    assert_eq!(
        f.body().await,
        "",
        "keep the placeholder while tools can still run"
    );
    f.result(progress).await;
    // A repeated provider result must not recreate an empty bubble or duplicate controls.
    f.result(progress).await;
    let messages: i64 = sqlx::query_scalar("select count(*) from messages where stream_key=$1")
        .bind(&f.key)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(messages, 0);
    let activities: i64 = sqlx::query_scalar(
        "select count(*) from agent_activities where run_id=$1 and title='Checking the build'",
    )
    .bind(f.run)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(activities, 1);
    let deletes: i64 = sqlx::query_scalar("select count(*) from ui_events where json_extract(event_json, '$.reason')='empty_stream_finished'")
        .fetch_one(&f.pool).await.unwrap();
    assert_eq!(deletes, 1);
    let run_status: String = sqlx::query_scalar("select status from agent_runs where id=$1")
        .bind(f.run)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(run_status, "exited");
    assert!(f.runtime.state.lock().await.active.is_none());
    f.close().await;
}

#[tokio::test]
async fn result_only_recovers_missing_final_after_progress() {
    let f = Fixture::new().await;
    f.stream("Reading papers.").await;
    f.assistant("progress", "Reading papers.").await;
    f.result("The complete final answer.").await;
    assert_eq!(
        f.body().await,
        "Reading papers.\n\nThe complete final answer."
    );
    f.close().await;
}

#[tokio::test]
async fn final_text_replaces_missing_middle_without_duplicate_prose() {
    let f = Fixture::new().await;
    f.stream("检查完成。错尾").await;
    f.assistant("answer", "检查完成。完整结果🦀").await;
    f.result("检查完成。完整结果🦀").await;
    assert_eq!(f.body().await, "检查完成。完整结果🦀");
    f.close().await;
}

#[tokio::test]
async fn lone_prefix_recovers_four_messages_and_final_once() {
    let f = Fixture::new().await;
    f.stream("Reading the papers.").await;
    f.assistant("progress", "Reading the papers.").await;
    let mut final_text = String::new();
    for index in 1..=4 {
        final_text.push_str(&format!("LANTOR_EVENT {}\n", json!({"type":"channel_message_create","channel_id":f.channel,"body":format!("Paper {index}: {}", "正文🦀".repeat(400))})));
    }
    final_text.push_str("Paper 5: complete final answer.");
    f.stream("L").await;
    f.assistant("papers", &final_text).await;
    f.result(&final_text).await;
    let messages: Vec<String> =
        sqlx::query_scalar("select body from messages where sender_agent_id=$1 order by seq")
            .bind(f.agent)
            .fetch_all(&f.pool)
            .await
            .unwrap();
    assert_eq!(messages.len(), 5);
    let body = f.body().await;
    assert!(body.starts_with("Reading the papers."));
    assert!(body.ends_with("Paper 5: complete final answer."));
    assert_eq!(body.matches("Paper 5:").count(), 1);
    assert!(!body.contains("LANTOR_EVENT"));
    for index in 1..=4 {
        assert_eq!(
            messages
                .iter()
                .filter(|body| body.starts_with(&format!("Paper {index}:")))
                .count(),
            1
        );
    }
    let receipts: i64 =
        sqlx::query_scalar("select count(*) from agent_event_receipts where run_id=$1")
            .bind(f.run)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(receipts, 4);
    f.close().await;
}

#[tokio::test]
async fn result_repairs_partial_control_without_assistant_event() {
    let f = Fixture::new().await;
    let control = format!(
        "LANTOR_EVENT {}",
        json!({"type":"channel_message_create","channel_id":f.channel,"body":"Recovered message"})
    );
    f.stream(&control[..40]).await;
    f.result(&control).await;
    let messages: Vec<String> =
        sqlx::query_scalar("select body from messages where sender_agent_id=$1")
            .bind(f.agent)
            .fetch_all(&f.pool)
            .await
            .unwrap();
    assert_eq!(messages, vec!["Recovered message"]);
    f.close().await;
}

#[tokio::test]
async fn replay_keeps_existing_control_effect_once_and_updates_ui() {
    let f = Fixture::new().await;
    let control = format!(
        "LANTOR_EVENT {}\n",
        json!({"type":"channel_message_create","channel_id":f.channel,"body":"Already sent"})
    );
    f.stream(&format!("{control}Wrong tail")).await;
    let complete = format!("{control}Correct final text");
    f.assistant("answer", &complete).await;
    f.assistant("answer", &complete).await;
    f.result(&complete).await;
    assert_eq!(f.body().await.trim(), "Correct final text");
    let count: i64 = sqlx::query_scalar(
        "select count(*) from messages where sender_agent_id=$1 and body='Already sent'",
    )
    .bind(f.agent)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
    let event: String = sqlx::query_scalar("select event_json from ui_events where json_extract(event_json, '$.reason')='stream_reconciled' order by id desc limit 1")
        .fetch_one(&f.pool).await.unwrap();
    assert!(event.contains("Correct final text"));
    assert!(!event.contains("LANTOR_EVENT"));
    f.close().await;
}

#[tokio::test]
async fn same_text_in_distinct_blocks_is_preserved_and_result_not_duplicated() {
    let f = Fixture::new().await;
    f.stream("Repeated paragraph.").await;
    f.assistant("one", "Repeated paragraph.").await;
    f.stream("Repeated paragraph.").await;
    f.assistant("two", "Repeated paragraph.").await;
    f.result("Repeated paragraph.").await;
    assert_eq!(f.body().await, "Repeated paragraph.\n\nRepeated paragraph.");
    f.close().await;
}

#[tokio::test]
async fn old_unconfirmed_message_is_not_overwritten_by_final_result() {
    let f = Fixture::new().await;
    f.send(
        json!({"type":"stream_event","event":{"type":"message_start","message":{"id":"progress"}}}),
    )
    .await;
    f.stream("Progress.").await;
    // The progress assistant event was lost, but final stream/assistant arrived.
    f.send(
        json!({"type":"stream_event","event":{"type":"message_start","message":{"id":"answer"}}}),
    )
    .await;
    f.stream("Answer.").await;
    f.assistant("answer", "Answer.").await;
    f.result("Answer.").await;
    assert_eq!(f.body().await, "Progress.\n\nAnswer.");
    f.close().await;
}

#[tokio::test]
async fn block_start_text_and_fenced_controls_remain_literal() {
    let f = Fixture::new().await;
    let answer = "```text\nLANTOR_EVENT {\"type\":\"activity\",\"title\":\"Example\"}\n```\n完成。";
    f.send(json!({"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":"```text\n"}}})).await;
    f.assistant("answer", answer).await;
    f.result(answer).await;
    assert_eq!(f.body().await, answer);
    let receipts: i64 =
        sqlx::query_scalar("select count(*) from agent_event_receipts where run_id=$1")
            .bind(f.run)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(receipts, 0);
    f.close().await;
}

#[tokio::test]
async fn missing_resume_session_requeues_request_for_a_new_session() {
    let f = Fixture::new().await;
    let missing = "0b5c8f3e-1111-4222-8333-944455556666";
    upsert_runtime_thread_id(&f.pool, f.agent, "claude", missing, "idle")
        .await
        .unwrap();
    session::store_claude_context_tokens(&f.pool, f.agent, 42_000)
        .await
        .unwrap();
    f.send(json!({
        "type": "result",
        "subtype": "error_during_execution",
        "is_error": true,
        "num_turns": 0,
        "session_id": missing,
        "errors": [format!("No conversation found with session ID: {missing}")]
    }))
    .await;

    let session = sqlx::query(
        "select provider_thread_id, context_tokens from runtime_sessions where agent_id=$1 and runtime='claude'",
    )
    .bind(f.agent)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(session.get::<String, _>("provider_thread_id"), "");
    assert_eq!(session.get::<i64, _>("context_tokens"), 0);
    assert_eq!(
        session::plan_claude_session_start(&f.pool, f.agent, 200_000)
            .await
            .unwrap(),
        session::ClaudeSessionStart::Fresh
    );
    let run_status: String = sqlx::query_scalar("select status from agent_runs where id=$1")
        .bind(f.run)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(run_status, "failed");
    let work_status: String = sqlx::query_scalar(
        "select w.status from agent_work_items w join agent_runs r on r.work_item_id = w.id where r.id=$1",
    )
    .bind(f.run)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(work_status, "queued", "the request reruns in a new session");
    let messages: i64 = sqlx::query_scalar("select count(*) from messages where stream_key=$1")
        .bind(&f.key)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(messages, 0);
    let state = f.runtime.state.lock().await;
    assert!(!state.alive);
    assert!(state.active.is_none());
    drop(state);
    f.close().await;
}

#[tokio::test]
async fn finished_turn_persists_context_size_for_the_next_spawn() {
    let f = Fixture::new().await;
    f.send(json!({"type":"assistant","parent_tool_use_id":null,"message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}],"usage":{"input_tokens":3,"cache_creation_input_tokens":1_000,"cache_read_input_tokens":120_000,"output_tokens":50}}})).await;
    // A subagent's request describes its own context, not the session's.
    f.send(json!({"type":"assistant","parent_tool_use_id":"t1","message":{"id":"s1","content":[{"type":"text","text":"sub"}],"usage":{"input_tokens":5,"cache_creation_input_tokens":0,"cache_read_input_tokens":9_000,"output_tokens":5}}})).await;
    f.result("").await;
    let context_tokens: i64 = sqlx::query_scalar(
        "select context_tokens from runtime_sessions where agent_id=$1 and runtime='claude'",
    )
    .bind(f.agent)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(context_tokens, 121_003);
    f.close().await;
}
