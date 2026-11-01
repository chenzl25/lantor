use std::sync::atomic::{AtomicU32, Ordering};

use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use super::{
    provider_allows_start, provider_needs_login, record_provider_auth_failure,
    record_provider_turn_success, start_due_provider_probes,
};
use crate::agent_inbox_wake::enqueue_agent_work_if_available;
use crate::decision_store::{answer_decision_in_pool, dismiss_decision_in_pool, load_decisions};
use crate::runtime::turn_outcome::{
    finished_work_status, is_transient_sqlite_error, retry_transient_sqlite,
};
use crate::test_support::{drop_test_schema, insert_test_agent, insert_test_channel, test_pool};

async fn insert_claude_agent(pool: &SqlitePool, handle: &str) -> Result<Uuid, String> {
    let agent_id = insert_test_agent(pool, handle).await?;
    sqlx::query("update agents set runtime = 'claude' where id = $1")
        .bind(agent_id)
        .execute(pool)
        .await
        .map_err(|err| err.to_string())?;
    Ok(agent_id)
}

async fn insert_run(
    pool: &SqlitePool,
    agent_id: Uuid,
    status: &str,
    started_at: &str,
) -> Result<Uuid, String> {
    sqlx::query_scalar(
        r#"
        insert into agent_runs (agent_id, command, status, started_at)
        values ($1, 'claude', $2, $3)
        returning id
        "#,
    )
    .bind(agent_id)
    .bind(status)
    .bind(started_at)
    .fetch_one(pool)
    .await
    .map_err(|err| err.to_string())
}

async fn auth_status(pool: &SqlitePool, runtime: &str) -> Result<Option<String>, String> {
    sqlx::query_scalar("select status from provider_auth_state where runtime = $1")
        .bind(runtime)
        .fetch_optional(pool)
        .await
        .map_err(|err| err.to_string())
}

#[tokio::test]
async fn login_failure_pauses_runtime_with_one_card_until_a_later_turn_succeeds() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let all = insert_test_channel(&pool, "all").await?;
        let elsewhere = insert_test_channel(&pool, "elsewhere").await?;
        let arch = insert_claude_agent(&pool, "arch").await?;
        insert_claude_agent(&pool, "speed").await?;

        // Two agents failing in the same second share one card in #all.
        for _ in 0..2 {
            record_provider_auth_failure(
                &pool,
                "Claude",
                Some(elsewhere),
                "Failed to authenticate: OAuth session expired",
            )
            .await?;
        }
        let decisions = load_decisions(&pool).await?;
        assert_eq!(decisions.len(), 1);
        let card = &decisions[0];
        assert_eq!(card.channel_id, all);
        assert!(card.requester_agent_id.is_none());
        assert!(
            !card.title.contains('@'),
            "the answer must not mention agents"
        );
        assert!(card.context.contains("arch, speed"));
        assert!(card.context.contains("claude auth login"));

        assert_eq!(
            auth_status(&pool, "claude").await?.as_deref(),
            Some("needs_login")
        );
        assert!(!provider_allows_start(&pool, Some("claude")).await?);
        assert!(provider_needs_login(&pool, Some("claude")).await?);
        assert!(provider_allows_start(&pool, Some("codex")).await?);
        assert!(provider_allows_start(&pool, None).await?);

        // Answering the card lets exactly one turn try the login.
        answer_decision_in_pool(&pool, card.id, Some("a"), None).await?;
        assert_eq!(
            auth_status(&pool, "claude").await?.as_deref(),
            Some("probing")
        );
        assert!(!provider_needs_login(&pool, Some("claude")).await?);
        assert!(provider_allows_start(&pool, Some("claude")).await?);
        let probe = insert_run(&pool, arch, "running", "2999-01-01T00:00:00.000+00:00").await?;
        assert!(!provider_allows_start(&pool, Some("claude")).await?);

        // Still broken after the owner said it works: ask again.
        record_provider_auth_failure(&pool, "claude", None, "still expired").await?;
        let decisions = load_decisions(&pool).await?;
        assert_eq!(decisions.len(), 2);
        let second = decisions
            .iter()
            .find(|decision| decision.status == "open")
            .ok_or("second card missing")?;
        assert!(second.title.contains("still"));

        // A turn that began before the failure proves nothing.
        let stale = insert_run(&pool, arch, "exited", "2000-01-01T00:00:00.000+00:00").await?;
        record_provider_turn_success(&pool, "claude", stale).await?;
        assert_eq!(
            auth_status(&pool, "claude").await?.as_deref(),
            Some("needs_login")
        );

        // A turn that started after it reopens dispatch and closes the card.
        record_provider_turn_success(&pool, "claude", probe).await?;
        assert_eq!(auth_status(&pool, "claude").await?.as_deref(), Some("ok"));
        let reopened = load_decisions(&pool).await?;
        let closed = reopened
            .iter()
            .find(|decision| decision.id == second.id)
            .ok_or("card missing")?;
        assert_eq!(closed.status, "dismissed");
        let notice: String = sqlx::query_scalar(
            "select body from messages where thread_root_id = $1 and sender_role = 'system'",
        )
        .bind(second.message_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert!(notice.contains("Claude login works again"));
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn dismissed_login_card_is_not_raised_again_and_probes_start_on_their_own() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let channel = insert_test_channel(&pool, "work").await?;
        record_provider_auth_failure(&pool, "codex", Some(channel), "unauthorized").await?;
        let card = load_decisions(&pool).await?.remove(0);
        assert_eq!(card.channel_id, channel);
        assert!(card.context.contains("codex login"));
        dismiss_decision_in_pool(&pool, card.id).await?;

        record_provider_auth_failure(&pool, "codex", Some(channel), "unauthorized").await?;
        assert_eq!(load_decisions(&pool).await?.len(), 1);

        // Not due yet.
        start_due_provider_probes(&pool).await?;
        assert_eq!(auth_status(&pool, "codex").await?.as_deref(), Some("needs_login"));
        sqlx::query(
            "update provider_auth_state set updated_at = '2000-01-01T00:00:00.000+00:00' where runtime = 'codex'",
        )
        .execute(&pool)
        .await
        .map_err(|err| err.to_string())?;
        start_due_provider_probes(&pool).await?;
        assert_eq!(auth_status(&pool, "codex").await?.as_deref(), Some("probing"));
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn paused_runtime_keeps_new_requests_queued() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let channel = insert_test_channel(&pool, "work").await?;
        let agent = insert_claude_agent(&pool, "jarvis").await?;
        let work_item_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_work_items (agent_id, channel_id, title, status)
            values ($1, $2, 'daily memory', 'queued')
            returning id
            "#,
        )
        .bind(agent)
        .bind(channel)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        record_provider_auth_failure(&pool, "claude", Some(channel), "expired").await?;

        assert!(!enqueue_agent_work_if_available(&pool, agent, work_item_id).await?);
        let commands: i64 = sqlx::query_scalar("select count(*) from supervisor_commands")
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert_eq!(commands, 0);

        let card = load_decisions(&pool).await?.remove(0);
        answer_decision_in_pool(&pool, card.id, Some("a"), None).await?;
        assert!(enqueue_agent_work_if_available(&pool, agent, work_item_id).await?);
        let row = sqlx::query("select status from agent_work_items where id = $1")
            .bind(work_item_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert_eq!(row.get::<String, _>("status"), "queued");
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[test]
fn login_failures_requeue_and_other_outcomes_keep_their_status() {
    assert_eq!(
        finished_work_status(false, false, true, Some("running")),
        "queued"
    );
    assert_eq!(
        finished_work_status(false, true, true, Some("cancelling")),
        "cancelled"
    );
    assert_eq!(
        finished_work_status(false, false, false, Some("running")),
        "failed"
    );
    assert_eq!(
        finished_work_status(true, false, false, Some("silent")),
        "silent"
    );
    assert_eq!(
        finished_work_status(true, false, false, Some("held")),
        "held"
    );
    assert_eq!(
        finished_work_status(true, false, false, Some("running")),
        "done"
    );
}

#[tokio::test]
async fn transient_lock_errors_are_retried_and_others_are_not() {
    assert!(is_transient_sqlite_error(
        "error returned from database: (code: 5) database is locked"
    ));
    assert!(!is_transient_sqlite_error("no such table: agents"));

    let attempts = AtomicU32::new(0);
    let value = retry_transient_sqlite(|| async {
        if attempts.fetch_add(1, Ordering::SeqCst) < 2 {
            Err("error returned from database: (code: 5) database is locked".to_owned())
        } else {
            Ok(7)
        }
    })
    .await;
    assert_eq!(value, Ok(7));
    assert_eq!(attempts.load(Ordering::SeqCst), 3);

    let attempts = AtomicU32::new(0);
    let value: Result<(), String> = retry_transient_sqlite(|| async {
        attempts.fetch_add(1, Ordering::SeqCst);
        Err("no such table: agents".to_owned())
    })
    .await;
    assert!(value.is_err());
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}
