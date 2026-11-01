use std::path::PathBuf;

use serde_json::json;

use super::interrupt::interrupt_warm_claude_turn;
use super::*;
use crate::agent_inbox_wake::enqueue_agent_work_if_available;
use crate::test_support::{drop_test_schema, insert_test_agent, insert_test_channel, test_pool};

/// A warm Claude runtime with one running turn in `channel`, whose stdin is
/// captured to a file so tests can read what Lantor wrote to the CLI.
struct Fixture {
    pool: SqlitePool,
    database: String,
    agent: Uuid,
    channel: Uuid,
    work: Uuid,
    run: Uuid,
    runtime: Arc<WarmClaudeRuntime>,
    child: tokio::process::Child,
    input: PathBuf,
}

impl Fixture {
    async fn new() -> Self {
        let (pool, database) = test_pool().await.expect("SQLite fixture must initialize");
        let agent = insert_test_agent(&pool, "claude-steer").await.unwrap();
        sqlx::query("update agents set status = 'running', runtime = 'claude' where id = $1")
            .bind(agent)
            .execute(&pool)
            .await
            .unwrap();
        let channel = insert_test_channel(&pool, "claude-steer").await.unwrap();
        sqlx::query("insert into channel_members (channel_id, agent_id) values ($1, $2)")
            .bind(channel)
            .bind(agent)
            .execute(&pool)
            .await
            .unwrap();
        let work = insert_work_item(&pool, agent, channel, None, "running").await;
        let run: Uuid = sqlx::query_scalar(
            "insert into agent_runs (agent_id, work_item_id, command, status) values ($1, $2, 'claude', 'running') returning id",
        )
        .bind(agent)
        .bind(work)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query("update agent_work_items set run_id = $2 where id = $1")
            .bind(work)
            .bind(run)
            .execute(&pool)
            .await
            .unwrap();
        let stream_key = claude_stream_key(run);
        ensure_streaming_agent_message(&pool, agent, channel, None, &stream_key)
            .await
            .unwrap();

        let input = std::env::temp_dir().join(format!("lantor-claude-steer-{run}.jsonl"));
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("cat > \"$1\"")
            .arg("sh")
            .arg(&input)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let runtime = Arc::new(WarmClaudeRuntime {
            stdin: AsyncMutex::new(stdin),
            state: AsyncMutex::new(WarmClaudeState {
                alive: true,
                active: Some(ClaudeActiveTurn {
                    run_id: run,
                    started_at: Instant::now(),
                    first_delta_at: None,
                    work_item_id: Some(work),
                    channel_id: Some(channel),
                    thread_root_id: None,
                    stream_key,
                    pending_memory_context: None,
                    text: ClaudeTextState::default(),
                    interrupt_requested: false,
                    auth_required: false,
                }),
                session_id: Some("test-claude-session".to_owned()),
                context_tokens: 0,
                pending_rotation_marker: None,
                last_surface: None,
                injected_memory_context: None,
                last_activity: Instant::now(),
                pending_steers: Vec::new(),
            }),
            pid: None,
            environment_variables: String::new(),
            provider: ClaudeCli,
        });
        Self {
            pool,
            database,
            agent,
            channel,
            work,
            run,
            runtime,
            child,
            input,
        }
    }

    async fn steer(&self, work_item_id: Uuid) -> bool {
        steer_warm_claude_turn_if_same_surface(
            &self.pool,
            self.agent,
            &self.runtime,
            ClaudeSteerRequest {
                work_item_id,
                prompt: "please also check the docs",
                rendered_context_max_seq: 0,
                command_text: "claude stream-json",
                working_directory: "",
            },
        )
        .await
        .unwrap()
    }

    async fn pending_uuids(&self) -> Vec<String> {
        self.runtime
            .state
            .lock()
            .await
            .pending_steers
            .iter()
            .map(|steer| steer.uuid.clone())
            .collect()
    }

    async fn send(&self, event: Value) {
        handle_claude_warm_stdout_line(&self.pool, self.agent, &self.runtime, &event.to_string())
            .await
            .unwrap();
    }

    async fn replay(&self, uuid: &str) {
        self.send(json!({"type":"user","isReplay":true,"uuid":uuid,"message":{"role":"user","content":"please also check the docs"}}))
            .await;
    }

    async fn reply(&self, text: &str) {
        self.send(json!({"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}})).await;
        self.send(json!({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}})).await;
        self.send(json!({"type":"result","subtype":"success","is_error":false,"result":text}))
            .await;
    }

    async fn work_item(&self, id: Uuid) -> (String, Option<Uuid>) {
        let row = sqlx::query("select status, run_id from agent_work_items where id = $1")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .unwrap();
        (row.get("status"), row.get("run_id"))
    }

    async fn active_run(&self) -> Option<(Uuid, Option<Uuid>)> {
        self.runtime
            .state
            .lock()
            .await
            .active
            .as_ref()
            .map(|active| (active.run_id, active.work_item_id))
    }

    /// Close the CLI's stdin and return every line Lantor wrote to it.
    async fn written(mut self) -> Vec<Value> {
        let Self {
            pool,
            database,
            runtime,
            input,
            ..
        } = self;
        drop(runtime);
        self.child.wait().await.unwrap();
        let written = std::fs::read_to_string(&input).unwrap_or_default();
        let _ = std::fs::remove_file(&input);
        drop_test_schema(pool, database).await;
        written
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

async fn insert_work_item(
    pool: &SqlitePool,
    agent: Uuid,
    channel: Uuid,
    thread_root_id: Option<Uuid>,
    status: &str,
) -> Uuid {
    sqlx::query_scalar(
        "insert into agent_work_items (agent_id, channel_id, thread_root_id, title, status) values ($1, $2, $3, 'request', $4) returning id",
    )
    .bind(agent)
    .bind(channel)
    .bind(thread_root_id)
    .bind(status)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn follow_up_replayed_before_the_result_joins_the_running_turn() {
    let fixture = Fixture::new().await;
    let follow_up = insert_work_item(
        &fixture.pool,
        fixture.agent,
        fixture.channel,
        None,
        "queued",
    )
    .await;

    assert!(fixture.steer(follow_up).await);
    assert_eq!(
        fixture.work_item(follow_up).await,
        ("running".to_owned(), Some(fixture.run))
    );
    let uuids = fixture.pending_uuids().await;
    assert_eq!(uuids.len(), 1);

    // The CLI consumed it at a tool boundary, inside the running turn.
    fixture.replay(&uuids[0]).await;
    assert_eq!(
        fixture.work_item(follow_up).await,
        ("done".to_owned(), Some(fixture.run))
    );
    assert!(fixture.pending_uuids().await.is_empty());

    fixture.reply("Done, docs checked too.").await;
    assert_eq!(fixture.work_item(fixture.work).await.0, "done");
    assert!(
        fixture.active_run().await.is_none(),
        "no extra turn is expected"
    );

    let written = fixture.written().await;
    assert_eq!(written.len(), 1);
    assert_eq!(written[0]["type"], "user");
    assert_eq!(written[0]["uuid"], uuids[0].as_str());
    assert_eq!(
        written[0]["message"]["content"][0]["text"],
        "please also check the docs"
    );
}

#[tokio::test]
async fn follow_up_not_replayed_by_the_result_gets_its_own_run() {
    let fixture = Fixture::new().await;
    let follow_up = insert_work_item(
        &fixture.pool,
        fixture.agent,
        fixture.channel,
        None,
        "queued",
    )
    .await;
    assert!(fixture.steer(follow_up).await);
    let uuid = fixture.pending_uuids().await.remove(0);

    // The turn ended without consuming the follow-up; the CLI starts a new
    // turn for it right away, which Lantor must own.
    fixture.reply("First answer.").await;
    assert_eq!(fixture.work_item(fixture.work).await.0, "done");
    let (adopted_run, adopted_work) = fixture.active_run().await.expect("follow-up turn");
    assert_ne!(adopted_run, fixture.run);
    assert_eq!(adopted_work, Some(follow_up));
    assert_eq!(
        fixture.work_item(follow_up).await,
        ("running".to_owned(), Some(adopted_run))
    );

    // Its replay opens that turn rather than joining another one.
    fixture.replay(&uuid).await;
    assert_eq!(fixture.work_item(follow_up).await.0, "running");
    assert!(fixture.pending_uuids().await.is_empty());

    fixture.reply("Second answer.").await;
    assert_eq!(
        fixture.work_item(follow_up).await,
        ("done".to_owned(), Some(adopted_run))
    );
    let bodies: Vec<String> = sqlx::query_scalar(
        "select body from messages where channel_id = $1 and sender_agent_id = $2 order by seq",
    )
    .bind(fixture.channel)
    .bind(fixture.agent)
    .fetch_all(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(bodies, vec!["First answer.", "Second answer."]);
    assert_eq!(fixture.written().await.len(), 1);
}

#[tokio::test]
async fn only_the_running_turns_surface_takes_follow_ups() {
    let fixture = Fixture::new().await;
    let root: Uuid = sqlx::query_scalar(
        "insert into messages (channel_id, sender_name, sender_role, body) values ($1, 'Dylan', 'owner', 'other thread') returning id",
    )
    .bind(fixture.channel)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    let other_thread = insert_work_item(
        &fixture.pool,
        fixture.agent,
        fixture.channel,
        Some(root),
        "queued",
    )
    .await;
    assert!(!fixture.steer(other_thread).await);
    assert_eq!(
        fixture.work_item(other_thread).await,
        ("queued".to_owned(), None)
    );

    // A turn being stopped takes no follow-ups either.
    let same_surface = insert_work_item(
        &fixture.pool,
        fixture.agent,
        fixture.channel,
        None,
        "queued",
    )
    .await;
    assert!(
        interrupt_warm_claude_turn(&fixture.pool, &fixture.runtime, fixture.run)
            .await
            .unwrap()
    );
    assert!(!fixture.steer(same_surface).await);
    assert_eq!(
        fixture.work_item(same_surface).await,
        ("queued".to_owned(), None)
    );
    assert!(fixture.pending_uuids().await.is_empty());

    let written = fixture.written().await;
    assert_eq!(written.len(), 1);
    assert_eq!(written[0]["type"], "control_request");
}

#[tokio::test]
async fn interrupted_turn_hands_unreplayed_follow_ups_to_a_new_run() {
    let fixture = Fixture::new().await;
    let follow_up = insert_work_item(
        &fixture.pool,
        fixture.agent,
        fixture.channel,
        None,
        "queued",
    )
    .await;
    assert!(fixture.steer(follow_up).await);
    sqlx::query("update agent_work_items set status = 'cancelling' where id = $1")
        .bind(fixture.work)
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert!(
        interrupt_warm_claude_turn(&fixture.pool, &fixture.runtime, fixture.run)
            .await
            .unwrap()
    );

    // An interrupt leaves queued input in place: the CLI reports it as
    // `still_queued` and runs it next.
    fixture
        .send(json!({"type":"result","subtype":"error_during_execution","is_error":true}))
        .await;
    assert_eq!(fixture.work_item(fixture.work).await.0, "cancelled");
    let (adopted_run, adopted_work) = fixture.active_run().await.expect("follow-up turn");
    assert_eq!(adopted_work, Some(follow_up));
    assert_eq!(
        fixture.work_item(follow_up).await,
        ("running".to_owned(), Some(adopted_run))
    );
    assert!(fixture.runtime.state.lock().await.alive);
    fixture.written().await;
}

#[tokio::test]
async fn failed_turn_requeues_follow_ups_the_cli_never_consumed() {
    let fixture = Fixture::new().await;
    let follow_up = insert_work_item(
        &fixture.pool,
        fixture.agent,
        fixture.channel,
        None,
        "queued",
    )
    .await;
    assert!(fixture.steer(follow_up).await);

    fixture
        .send(json!({"type":"result","subtype":"error","is_error":true,"error":"API Error: 500"}))
        .await;
    assert_eq!(fixture.work_item(fixture.work).await.0, "failed");
    assert_eq!(
        fixture.work_item(follow_up).await,
        ("queued".to_owned(), None)
    );
    assert!(fixture.active_run().await.is_none());
    assert!(fixture.pending_uuids().await.is_empty());
    fixture.written().await;
}

#[tokio::test]
async fn claude_follow_up_is_enqueued_only_for_the_running_runs_surface() {
    let fixture = Fixture::new().await;
    let follow_up = insert_work_item(
        &fixture.pool,
        fixture.agent,
        fixture.channel,
        None,
        "queued",
    )
    .await;
    assert!(
        enqueue_agent_work_if_available(&fixture.pool, fixture.agent, follow_up)
            .await
            .unwrap()
    );
    let starts: i64 = sqlx::query_scalar(
        "select count(*) from supervisor_commands where command_type = 'start_agent' and work_item_id = $1",
    )
    .bind(follow_up)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(starts, 1);
    let agent_status: String = sqlx::query_scalar("select status from agents where id = $1")
        .bind(fixture.agent)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(agent_status, "running");

    let other_channel = insert_test_channel(&fixture.pool, "claude-steer-other")
        .await
        .unwrap();
    let elsewhere =
        insert_work_item(&fixture.pool, fixture.agent, other_channel, None, "queued").await;
    assert!(
        !enqueue_agent_work_if_available(&fixture.pool, fixture.agent, elsewhere)
            .await
            .unwrap()
    );
    fixture.written().await;
}

async fn wait_for_status(pool: &SqlitePool, work_item_id: Uuid, wanted: &[&str]) -> String {
    let deadline = Instant::now() + std::time::Duration::from_secs(120);
    loop {
        let status: String =
            sqlx::query_scalar("select status from agent_work_items where id = $1")
                .bind(work_item_id)
                .fetch_one(pool)
                .await
                .unwrap();
        if wanted.contains(&status.as_str()) || Instant::now() > deadline {
            return status;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

/// Drives the real `claude` CLI through Lantor's spawn path:
/// `cargo test live_claude_cli -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "drives the real Claude CLI and spends tokens"]
async fn live_claude_cli_follow_up_and_interrupt() {
    let (pool, database) = test_pool().await.unwrap();
    let agent = insert_test_agent(&pool, "claude-live").await.unwrap();
    sqlx::query("update agents set runtime = 'claude' where id = $1")
        .bind(agent)
        .execute(&pool)
        .await
        .unwrap();
    let channel = insert_test_channel(&pool, "claude-live").await.unwrap();
    sqlx::query("insert into channel_members (channel_id, agent_id) values ($1, $2)")
        .bind(channel)
        .bind(agent)
        .execute(&pool)
        .await
        .unwrap();
    let workdir = std::env::temp_dir().join(format!("lantor-claude-live-{agent}"));
    std::fs::create_dir_all(&workdir).unwrap();
    let workdir = workdir.to_string_lossy().into_owned();
    let registry = WarmClaudeRegistry::default();
    let start = |work_item_id: Uuid, prompt: &'static str| {
        let pool = pool.clone();
        let registry = registry.clone();
        let workdir = workdir.clone();
        async move {
            supervisor_start_claude_streaming_agent(
                &pool,
                &registry,
                agent,
                Some(work_item_id),
                "claude-live".to_owned(),
                "haiku".to_owned(),
                String::new(),
                workdir,
                String::new(),
                prompt.to_owned(),
                0,
                None,
            )
            .await
            .unwrap();
        }
    };
    let runtime_pid = || async { registry.get(agent).await.and_then(|runtime| runtime.pid) };

    // A follow-up sent while a tool runs.
    let first = insert_work_item(&pool, agent, channel, None, "queued").await;
    start(first, "Run the bash command `sleep 12` with the Bash tool, then reply with exactly the word FIRST-DONE.").await;
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
    let follow_up = insert_work_item(&pool, agent, channel, None, "queued").await;
    start(follow_up, "New instruction while you work: once the command finishes, reply with exactly the word PINEAPPLE instead.").await;
    let first_status = wait_for_status(&pool, first, &["done", "failed", "held", "silent"]).await;
    let follow_status = wait_for_status(
        &pool,
        follow_up,
        &["done", "failed", "held", "silent", "queued"],
    )
    .await;
    let runs: Vec<(Option<Uuid>, String)> = sqlx::query_as(
        "select work_item_id, status from agent_runs where agent_id = $1 order by started_at",
    )
    .bind(agent)
    .fetch_all(&pool)
    .await
    .unwrap();
    let bodies: Vec<String> = sqlx::query_scalar(
        "select body from messages where channel_id = $1 and sender_agent_id = $2 order by seq",
    )
    .bind(channel)
    .bind(agent)
    .fetch_all(&pool)
    .await
    .unwrap();
    eprintln!(
        "follow-up: first={first_status} follow_up={follow_status} runs={runs:?} bodies={bodies:?}"
    );
    assert_eq!(first_status, "done");
    assert_eq!(follow_status, "done");
    assert!(
        bodies.iter().any(|body| body.contains("PINEAPPLE")),
        "{bodies:?}"
    );

    // A stop interrupts the turn and keeps the process for the next one.
    let pid_before = runtime_pid().await;
    let long = insert_work_item(&pool, agent, channel, None, "queued").await;
    start(long, "Run the bash command `sleep 60` with the Bash tool, then reply with exactly the word LONG-DONE.").await;
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
    let long_run: Uuid = sqlx::query_scalar("select run_id from agent_work_items where id = $1")
        .bind(long)
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query("update agent_work_items set status = 'cancelling' where id = $1")
        .bind(long)
        .execute(&pool)
        .await
        .unwrap();
    let stop_requested = Instant::now();
    assert!(interrupt_warm_claude_run(&pool, &registry, agent, long_run)
        .await
        .unwrap());
    let long_status = wait_for_status(&pool, long, &["cancelled", "done", "failed"]).await;
    let stop_latency = stop_requested.elapsed();
    let run_status: String = sqlx::query_scalar("select status from agent_runs where id = $1")
        .bind(long_run)
        .fetch_one(&pool)
        .await
        .unwrap();
    eprintln!("interrupt: work={long_status} run={run_status} latency={stop_latency:?}");
    assert_eq!(long_status, "cancelled");
    assert_eq!(run_status, "cancelled");
    assert!(stop_latency < std::time::Duration::from_secs(10));

    let after = insert_work_item(&pool, agent, channel, None, "queued").await;
    start(after, "Reply with exactly the word AFTER-STOP.").await;
    let after_status = wait_for_status(&pool, after, &["done", "failed", "held", "silent"]).await;
    let pid_after = runtime_pid().await;
    eprintln!("after stop: work={after_status} pid_before={pid_before:?} pid_after={pid_after:?}");
    assert_eq!(after_status, "done");
    assert!(pid_before.is_some());
    assert_eq!(pid_before, pid_after, "the stop must not respawn the CLI");

    // A follow-up that arrives with no tool boundary left runs as its own turn.
    let essay = insert_work_item(&pool, agent, channel, None, "queued").await;
    start(
        essay,
        "Without using any tools, write a 300-word story about a lighthouse keeper.",
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let late = insert_work_item(&pool, agent, channel, None, "queued").await;
    start(late, "Reply with exactly the word LATE-FOLLOW-UP.").await;
    let essay_status = wait_for_status(&pool, essay, &["done", "failed", "held", "silent"]).await;
    let late_status =
        wait_for_status(&pool, late, &["done", "failed", "held", "silent", "queued"]).await;
    let late_runs: Vec<(Option<Uuid>, String)> = sqlx::query_as(
        "select work_item_id, status from agent_runs where work_item_id in ($1, $2) order by started_at",
    )
    .bind(essay)
    .bind(late)
    .fetch_all(&pool)
    .await
    .unwrap();
    let late_run: Uuid = sqlx::query_scalar("select run_id from agent_work_items where id = $1")
        .bind(late)
        .fetch_one(&pool)
        .await
        .unwrap();
    let late_body: Option<String> =
        sqlx::query_scalar("select body from messages where stream_key = $1")
            .bind(claude_stream_key(late_run))
            .fetch_optional(&pool)
            .await
            .unwrap();
    eprintln!("separate turn: essay={essay_status} late={late_status} runs={late_runs:?} late_body={late_body:?}");
    assert_eq!(essay_status, "done");
    assert_eq!(late_status, "done");
    assert_eq!(
        late_runs.len(),
        2,
        "the late follow-up needs a run of its own"
    );
    assert!(late_body.is_some_and(|body| body.contains("LATE-FOLLOW-UP")));

    let pid_end = runtime_pid().await;
    assert_eq!(pid_before, pid_end);
    if let Some(pid) = pid_end {
        let _ = crate::runtime::process::terminate_process_group(pid).await;
    }
    let _ = std::fs::remove_dir_all(&workdir);
    drop_test_schema(pool, database).await;
}
