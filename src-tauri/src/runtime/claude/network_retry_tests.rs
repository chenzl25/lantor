use super::*;
use crate::test_support::{drop_test_schema, insert_test_agent, insert_test_channel, test_pool};
use serde_json::json;

const DROPPED: &str = "API Error: Connection dropped (ECONNRESET)";

/// The two lines the CLI prints when a turn ends because of `error`.
fn api_error_lines(text: &str, status: Value) -> Vec<Value> {
    vec![
        json!({
            "type": "assistant",
            "message": {
                "id": "synthetic",
                "model": "<synthetic>",
                "role": "assistant",
                "type": "message",
                "content": [{"type": "text", "text": text}],
            },
            "session_id": "s1",
            "error": "server_error",
            "is_api_error_message": true,
        }),
        json!({
            "type": "result",
            "subtype": "success",
            "is_error": true,
            "api_error_status": status,
            "terminal_reason": "api_error",
            "result": text,
            "session_id": "s1",
        }),
    ]
}

fn text_delta(text: &str) -> Value {
    json!({
        "type": "stream_event",
        "event": {
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": text},
        },
        "session_id": "s1",
    })
}

fn success_result(text: &str) -> Value {
    json!({
        "type": "result",
        "subtype": "success",
        "is_error": false,
        "result": text,
        "session_id": "s1",
    })
}

struct Fixture {
    pool: SqlitePool,
    database: String,
    agent: Uuid,
    channel: Uuid,
    work: Uuid,
}

struct Turn {
    run: Uuid,
    key: String,
    runtime: Arc<WarmClaudeRuntime>,
}

impl Fixture {
    async fn new(handle: &str) -> Self {
        let (pool, database) = test_pool().await.expect("SQLite fixture must initialize");
        let agent = insert_test_agent(&pool, handle).await.unwrap();
        sqlx::query("update agents set status = 'running', runtime = 'claude' where id = $1")
            .bind(agent)
            .execute(&pool)
            .await
            .unwrap();
        let channel = insert_test_channel(&pool, handle).await.unwrap();
        sqlx::query("insert into channel_members (channel_id, agent_id) values ($1, $2)")
            .bind(channel)
            .bind(agent)
            .execute(&pool)
            .await
            .unwrap();
        let work: Uuid = sqlx::query_scalar(
            "insert into agent_work_items (agent_id, channel_id, title, context, status) \
             values ($1, $2, 'look at the logs', 'context', 'queued') returning id",
        )
        .bind(agent)
        .bind(channel)
        .fetch_one(&pool)
        .await
        .unwrap();
        Self {
            pool,
            database,
            agent,
            channel,
            work,
        }
    }

    /// Starts a run of the work item, as the supervisor does for each attempt.
    async fn start_turn(&self) -> Turn {
        let run: Uuid = sqlx::query_scalar(
            "insert into agent_runs (agent_id, work_item_id, command, status) \
             values ($1, $2, 'warm runtime', 'running') returning id",
        )
        .bind(self.agent)
        .bind(self.work)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        sqlx::query("update agent_work_items set status = 'running', run_id = $2 where id = $1")
            .bind(self.work)
            .bind(run)
            .execute(&self.pool)
            .await
            .unwrap();
        let key = claude_stream_key(run);
        let runtime =
            super::tests::test_runtime_with_active_turn(run, self.work, self.channel, key.clone())
                .await
                .unwrap();
        ensure_streaming_agent_message(&self.pool, self.agent, self.channel, None, &key)
            .await
            .unwrap();
        Turn { run, key, runtime }
    }

    async fn send(&self, turn: &Turn, lines: &[Value]) {
        for line in lines {
            handle_claude_warm_stdout_line(
                &self.pool,
                self.agent,
                &turn.runtime,
                &line.to_string(),
            )
            .await
            .unwrap();
        }
    }

    async fn fail_turn(&self, text: &str) {
        let turn = self.start_turn().await;
        self.send(&turn, &api_error_lines(text, Value::Null)).await;
        assert!(turn.runtime.state.lock().await.active.is_none());
    }

    async fn status(&self) -> String {
        sqlx::query_scalar("select status from agent_work_items where id = $1")
            .bind(self.work)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn close(self) {
        drop_test_schema(self.pool, self.database).await;
    }
}

#[tokio::test]
async fn a_dropped_connection_sends_the_request_back_to_the_queue() {
    let fixture = Fixture::new("claude-network-requeue").await;
    let turn = fixture.start_turn().await;
    fixture.send(&turn, &[text_delta("Half an answer\n")]).await;
    fixture
        .send(&turn, &api_error_lines(DROPPED, Value::Null))
        .await;

    assert_eq!(fixture.status().await, "queued");
    let run_status: String = sqlx::query_scalar("select status from agent_runs where id = $1")
        .bind(turn.run)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(run_status, "failed");
    let replies: i64 = sqlx::query_scalar("select count(*) from messages where stream_key = $1")
        .bind(&turn.key)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(replies, 0, "the partial reply must not stay in the thread");
    let auth: Option<String> =
        sqlx::query_scalar("select status from provider_auth_state where runtime = 'claude'")
            .fetch_optional(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(auth, None, "a network failure does not pause the runtime");
    fixture.close().await;
}

impl Fixture {
    async fn network_state(&self, work: Uuid) -> (i64, bool, bool) {
        let row = sqlx::query(
            "select network_failures, network_requeued_at, retry_not_before \
             from agent_work_items where id = $1",
        )
        .bind(work)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        (
            row.get("network_failures"),
            row.get::<Option<String>, _>("network_requeued_at")
                .is_some(),
            row.get::<Option<String>, _>("retry_not_before").is_some(),
        )
    }

    /// Every reply of the agent, as (body, delivery state).
    async fn replies(&self) -> Vec<(String, String)> {
        sqlx::query(
            "select body, delivery_state from messages where sender_agent_id = $1 order by seq",
        )
        .bind(self.agent)
        .fetch_all(&self.pool)
        .await
        .unwrap()
        .into_iter()
        .map(|row| (row.get("body"), row.get("delivery_state")))
        .collect()
    }

    async fn owner_says(&self, body: &str) -> i64 {
        let message: Uuid = sqlx::query_scalar(
            "insert into messages (channel_id, sender_name, sender_role, body, is_task) \
             values ($1, 'Dylan', 'owner', $2, false) returning id",
        )
        .bind(self.channel)
        .bind(body)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        sqlx::query_scalar("select seq from messages where id = $1")
            .bind(message)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn the_retried_turn_leaves_one_reply_and_clears_the_count() {
    let fixture = Fixture::new("claude-network-one-reply").await;
    let first = fixture.start_turn().await;
    fixture
        .send(&first, &[text_delta("Half an answer\n")])
        .await;
    fixture
        .send(&first, &api_error_lines(DROPPED, Value::Null))
        .await;
    assert_eq!(fixture.replies().await, []);
    assert_eq!(fixture.network_state(fixture.work).await, (1, true, true));

    let second = fixture.start_turn().await;
    assert_ne!(first.key, second.key);
    fixture
        .send(
            &second,
            &[
                text_delta("The whole answer"),
                success_result("The whole answer"),
            ],
        )
        .await;

    assert_eq!(fixture.status().await, "done");
    assert_eq!(
        fixture.replies().await,
        [("The whole answer".to_owned(), "complete".to_owned())]
    );
    let (failures, _, waiting) = fixture.network_state(fixture.work).await;
    assert_eq!(failures, 0, "a success clears the count");
    assert!(!waiting);
    fixture.close().await;
}

#[tokio::test]
async fn the_owner_gets_a_card_only_when_the_retries_run_out() {
    let fixture = Fixture::new("claude-network-limit").await;
    for failures in 1..=5 {
        fixture.fail_turn(DROPPED).await;
        assert_eq!(fixture.status().await, "queued");
        assert_eq!(
            fixture.network_state(fixture.work).await,
            (failures, true, true)
        );
        let cards: i64 = sqlx::query_scalar("select count(*) from decisions")
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
        assert_eq!(cards, 0, "retrying is not worth telling the owner");
    }

    fixture.fail_turn(DROPPED).await;

    assert_eq!(fixture.status().await, "failed");
    assert_eq!(
        fixture.replies().await,
        [(DROPPED.to_owned(), "error".to_owned())],
        "the last attempt stays visible, like any failed turn"
    );
    let card = sqlx::query(
        "select id, title, context, retry_work_item_id, requester_agent_id \
         from decisions where status = 'open'",
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(
        card.get::<Option<Uuid>, _>("retry_work_item_id"),
        Some(fixture.work)
    );
    assert_eq!(card.get::<Option<Uuid>, _>("requester_agent_id"), None);
    assert!(card
        .get::<String, _>("title")
        .contains("claude-network-limit"));
    let context: String = card.get("context");
    assert!(context.contains("look at the logs"), "{context}");
    assert!(context.contains("ECONNRESET"), "{context}");

    crate::decision_store::answer_decision_in_pool(&fixture.pool, card.get("id"), Some("a"), None)
        .await
        .unwrap();

    let again: Uuid = sqlx::query_scalar(
        "select id from agent_work_items where agent_id = $1 and status = 'queued'",
    )
    .bind(fixture.agent)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_ne!(again, fixture.work);
    assert_eq!(
        fixture.network_state(again).await,
        (0, true, false),
        "the new request starts with a full budget, runs at once and is told to continue"
    );
    fixture.close().await;
}

#[tokio::test]
async fn failures_from_sleep_do_not_use_up_the_retries() {
    let fixture = Fixture::new("claude-network-sleep").await;
    for _ in 0..8 {
        let turn = fixture.start_turn().await;
        // The run began hours ago on the wall clock, seconds ago on the turn's
        // monotonic clock, which stops while the Mac sleeps.
        sqlx::query(
            "update agent_runs set started_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-4 hours') \
             where id = $1",
        )
        .bind(turn.run)
        .execute(&fixture.pool)
        .await
        .unwrap();
        fixture
            .send(
                &turn,
                &api_error_lines(
                    "API Error: Connection lost while your computer was asleep",
                    Value::Null,
                ),
            )
            .await;
        assert_eq!(fixture.status().await, "queued");
        assert_eq!(fixture.network_state(fixture.work).await, (0, true, true));
    }
    fixture.close().await;
}

#[tokio::test]
async fn other_errors_fail_as_before() {
    for (handle, text, status) in [
        (
            "claude-limit",
            "You've hit your session limit · resets 11pm (Asia/Singapore)",
            json!(429),
        ),
        (
            "claude-forbidden",
            "Failed to authenticate. API Error: 403 Request not allowed",
            json!(403),
        ),
        (
            "claude-server-error",
            "API Error: 500 Internal Server Error",
            json!(500),
        ),
        (
            "claude-too-long",
            "API Error: Prompt is too long",
            Value::Null,
        ),
        // The model can write this; only an API error counts.
        ("claude-model-text", "The logs say ECONNRESET", Value::Null),
    ] {
        let fixture = Fixture::new(handle).await;
        let turn = fixture.start_turn().await;
        fixture.send(&turn, &api_error_lines(text, status)).await;
        assert_eq!(fixture.status().await, "failed", "{text}");
        assert_eq!(fixture.network_state(fixture.work).await, (0, false, false));
        fixture.close().await;
    }
}

#[tokio::test]
async fn a_process_that_dies_is_not_retried() {
    let fixture = Fixture::new("claude-network-died").await;
    let turn = fixture.start_turn().await;
    super::turn::finish_warm_claude_active_turn(
        &fixture.pool,
        fixture.agent,
        &turn.runtime,
        false,
        Some("stdout closed".to_owned()),
    )
    .await
    .unwrap();
    assert_eq!(fixture.status().await, "failed");
    fixture.close().await;
}

#[tokio::test]
async fn a_stopped_turn_is_not_retried() {
    let fixture = Fixture::new("claude-network-stopped").await;
    let turn = fixture.start_turn().await;
    turn.runtime
        .state
        .lock()
        .await
        .active
        .as_mut()
        .unwrap()
        .interrupt_requested = true;
    fixture
        .send(&turn, &api_error_lines(DROPPED, Value::Null))
        .await;
    assert_eq!(fixture.status().await, "cancelled");
    assert_eq!(fixture.network_state(fixture.work).await, (0, false, false));
    fixture.close().await;
}

#[tokio::test]
async fn a_retried_turn_works_with_freshness_holds() {
    let fixture = Fixture::new("claude-network-freshness").await;
    let seen = fixture.owner_says("look at the logs").await;
    sqlx::query("update agent_work_items set context_max_seq = $2 where id = $1")
        .bind(fixture.work)
        .bind(seen)
        .execute(&fixture.pool)
        .await
        .unwrap();
    fixture.fail_turn(DROPPED).await;
    fixture.owner_says("also check the other one").await;

    // The retry answers without having seen the newer message: held once.
    let second = fixture.start_turn().await;
    fixture
        .send(
            &second,
            &[text_delta("The answer"), success_result("The answer")],
        )
        .await;
    assert_eq!(fixture.status().await, "held");
    let refresh = sqlx::query(
        "select id, status, freshness_generation from agent_work_items \
         where agent_id = $1 and source_kind = 'freshness_retry'",
    )
    .bind(fixture.agent)
    .fetch_all(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(refresh.len(), 1, "one refresh, not one per attempt");
    let refresh_id: Uuid = refresh[0].get("id");
    assert_eq!(refresh[0].get::<String, _>("status"), "queued");
    assert_eq!(refresh[0].get::<i64, _>("freshness_generation"), 1);
    assert_eq!(fixture.network_state(refresh_id).await, (0, false, false));

    // The refresh itself loses its connection: it keeps its identity.
    let retry_fixture = Fixture {
        pool: fixture.pool.clone(),
        database: fixture.database.clone(),
        agent: fixture.agent,
        channel: fixture.channel,
        work: refresh_id,
    };
    retry_fixture.fail_turn(DROPPED).await;
    let row = sqlx::query(
        "select status, source_kind, freshness_generation from agent_work_items where id = $1",
    )
    .bind(refresh_id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("status"), "queued");
    assert_eq!(row.get::<String, _>("source_kind"), "freshness_retry");
    assert_eq!(row.get::<i64, _>("freshness_generation"), 1);
    assert_eq!(fixture.network_state(refresh_id).await, (1, true, true));
    let held: i64 = sqlx::query_scalar("select count(*) from agent_held_outputs")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(held, 1);
    fixture.close().await;
}

#[tokio::test]
async fn lost_connections_do_not_use_up_start_attempts() {
    let fixture = Fixture::new("claude-network-starts").await;
    for _ in 0..3 {
        fixture.fail_turn(DROPPED).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let run: Uuid = sqlx::query_scalar(
        "insert into agent_runs (agent_id, work_item_id, command, status) \
         values ($1, $2, 'warm runtime', 'starting') returning id",
    )
    .bind(fixture.agent)
    .bind(fixture.work)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    let failed = crate::runtime::lifecycle::fail_warm_start(
        &fixture.pool,
        fixture.agent,
        run,
        Some(fixture.work),
        "database is locked",
        WarmStartFailure::Transient,
    )
    .await
    .unwrap();
    assert_eq!(failed.attempts, 1);
    assert!(failed.requeued);
    fixture.close().await;
}
