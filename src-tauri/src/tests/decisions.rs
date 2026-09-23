use serde_json::json;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use super::{
    answer_decision_in_pool, dismiss_decision_in_pool, load_decisions, normalize_decision_options,
    DecisionOptionInput,
};
use crate::events::control::{handle_agent_event, AgentEvent};
use crate::test_support::{drop_test_schema, insert_test_agent, insert_test_channel, test_pool};

fn option(id: Option<&str>, label: &str, recommended: bool) -> DecisionOptionInput {
    DecisionOptionInput {
        id: id.map(str::to_owned),
        label: label.to_owned(),
        detail: None,
        recommended: Some(recommended),
    }
}

#[test]
fn options_default_to_approve_decline_and_normalize_ids() {
    let defaults = normalize_decision_options(None).unwrap();
    assert_eq!(
        defaults.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
        ["approve", "decline"]
    );

    let options = normalize_decision_options(Some(vec![
        option(None, "  Reject NULL keys ", true),
        option(Some("Keep U-/U+ pairs!"), "Keep pairs", true),
        option(Some("keep-u-u-pairs"), "Duplicate id", false),
    ]))
    .unwrap();
    assert_eq!(options[0].id, "a");
    assert_eq!(options[0].label, "Reject NULL keys");
    assert!(options[0].recommended);
    assert_eq!(options[1].id, "keep-u-u-pairs");
    assert!(
        !options[1].recommended,
        "only the first recommendation survives"
    );
    assert_eq!(options[2].id, "keep-u-u-pairs-2");

    assert!(normalize_decision_options(Some(vec![option(None, "Only", false)])).is_err());
    assert!(normalize_decision_options(Some(
        (0..7)
            .map(|i| option(None, &format!("o{i}"), false))
            .collect()
    ))
    .is_err());
    assert!(normalize_decision_options(Some(vec![
        option(None, "ok", false),
        option(None, "   ", false)
    ]))
    .is_err());
}

async fn insert_run_in_thread(
    pool: &SqlitePool,
    agent_id: Uuid,
    channel_id: Uuid,
    thread_root_id: Uuid,
) -> Result<Uuid, String> {
    let work_item_id: Uuid = sqlx::query_scalar(
        r#"
        insert into agent_work_items (agent_id, channel_id, thread_root_id, title, status)
        values ($1, $2, $3, 'Owner question', 'running')
        returning id
        "#,
    )
    .bind(agent_id)
    .bind(channel_id)
    .bind(thread_root_id)
    .fetch_one(pool)
    .await
    .map_err(|err| err.to_string())?;
    sqlx::query_scalar(
        r#"
        insert into agent_runs (agent_id, command, status, work_item_id)
        values ($1, 'claude', 'running', $2)
        returning id
        "#,
    )
    .bind(agent_id)
    .bind(work_item_id)
    .fetch_one(pool)
    .await
    .map_err(|err| err.to_string())
}

async fn insert_owner_root(pool: &SqlitePool, channel_id: Uuid) -> Result<Uuid, String> {
    sqlx::query_scalar(
        r#"
        insert into messages (channel_id, sender_name, sender_role, body)
        values ($1, 'Dylan', 'owner', '@decider please pick an approach')
        returning id
        "#,
    )
    .bind(channel_id)
    .fetch_one(pool)
    .await
    .map_err(|err| err.to_string())
}

fn decision_event(value: serde_json::Value) -> AgentEvent {
    serde_json::from_value(value).expect("decision event should parse")
}

#[tokio::test]
async fn decision_request_posts_card_in_run_thread_and_answer_wakes_requester() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "decider").await?;
        let channel_id = insert_test_channel(&pool, "decisions").await?;
        let root_id = insert_owner_root(&pool, channel_id).await?;
        let run_id = insert_run_in_thread(&pool, agent_id, channel_id, root_id).await?;

        handle_agent_event(
            &pool,
            agent_id,
            run_id,
            decision_event(json!({
                "type": "decision_request",
                "question": "How should NULL keys behave?",
                "context": "Changelog sinks need a stable key.",
                "options": [
                    {"label": "Reject NULL keys", "detail": "Fail at CREATE time", "recommended": true},
                    {"label": "Treat NULL as a key", "description": "Matches Postgres DISTINCT"}
                ]
            })),
        )
        .await?;

        let decisions = load_decisions(&pool).await?;
        assert_eq!(decisions.len(), 1);
        let decision = &decisions[0];
        assert_eq!(decision.status, "open");
        assert_eq!(decision.thread_root_id, Some(root_id));
        assert_eq!(decision.channel_name, "decisions");
        assert_eq!(decision.requester_handle.as_deref(), Some("decider"));
        assert_eq!(decision.options[1].detail, "Matches Postgres DISTINCT");

        let card = sqlx::query("select body, thread_root_id, sender_agent_id from messages where id = $1")
            .bind(decision.message_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        let body: String = card.get("body");
        assert!(body.starts_with("**Decision needed:** How should NULL keys behave?"));
        assert!(body.contains("- **[a] Reject NULL keys** (recommended) — Fail at CREATE time"));
        assert_eq!(card.get::<Option<Uuid>, _>("thread_root_id"), Some(root_id));
        assert_eq!(card.get::<Option<Uuid>, _>("sender_agent_id"), Some(agent_id));

        let refreshes: i64 = sqlx::query_scalar(
            "select count(*) from ui_events where event_json like '%decision_created%'",
        )
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(refreshes, 1);

        assert!(answer_decision_in_pool(&pool, decision.id, Some("zzz"), None).await.is_err());
        assert!(answer_decision_in_pool(&pool, decision.id, None, Some("  ")).await.is_err());

        let answered =
            answer_decision_in_pool(&pool, decision.id, Some("b"), Some("NULL-safe equality please"))
                .await?;
        assert_eq!(answered.status, "answered");
        assert_eq!(answered.answer_option_id.as_deref(), Some("b"));
        let answer_id = answered.answer_message_id.expect("answer message id");
        let answer = sqlx::query("select body, sender_role, thread_root_id from messages where id = $1")
            .bind(answer_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        let answer_body: String = answer.get("body");
        assert!(answer_body.starts_with("@decider Decision: How should NULL keys behave?"));
        assert!(answer_body.contains("**[b] Treat NULL as a key**"));
        assert!(answer_body.ends_with("NULL-safe equality please"));
        assert_eq!(answer.get::<String, _>("sender_role"), "owner");
        assert_eq!(answer.get::<Option<Uuid>, _>("thread_root_id"), Some(root_id));

        let inbox_items: i64 = sqlx::query_scalar(
            "select count(*) from agent_inbox_items where agent_id = $1 and source_message_id = $2",
        )
        .bind(agent_id)
        .bind(answer_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(inbox_items, 1, "the answer must wake the requester");

        let again = answer_decision_in_pool(&pool, decision.id, Some("a"), None).await;
        assert!(again.is_err(), "a second answer must be rejected");
        let answers: i64 = sqlx::query_scalar(
            "select count(*) from messages where sender_role = 'owner' and body like '%Decision:%'",
        )
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(answers, 1);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn root_level_decision_answers_in_card_thread_and_can_be_dismissed_or_withdrawn() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "asker").await?;
        let other_agent_id = insert_test_agent(&pool, "other").await?;
        let channel_id = insert_test_channel(&pool, "roots").await?;
        let run_id: Uuid = sqlx::query_scalar(
            "insert into agent_runs (agent_id, command, status) values ($1, 'codex', 'running') returning id",
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;

        let outside_turn = handle_agent_event(
            &pool,
            agent_id,
            run_id,
            decision_event(json!({"type": "decision_request", "title": "Merge #12?"})),
        )
        .await;
        assert!(outside_turn.is_err(), "a run without a conversation needs an explicit channel");

        for title in ["Merge #12?", "Push the hotfix?", "Rename the flag?"] {
            handle_agent_event(
                &pool,
                agent_id,
                run_id,
                decision_event(json!({
                    "type": "decision_request",
                    "channel_id": channel_id,
                    "title": title
                })),
            )
            .await?;
        }
        let decisions = load_decisions(&pool).await?;
        assert_eq!(decisions.len(), 3);
        let merge = decisions.iter().find(|d| d.title == "Merge #12?").unwrap();
        assert_eq!(merge.thread_root_id, None);
        assert_eq!(merge.options.len(), 2, "no options means approve/decline");

        let answered = answer_decision_in_pool(&pool, merge.id, Some("approve"), None).await?;
        let answer_thread: Option<Uuid> =
            sqlx::query_scalar("select thread_root_id from messages where id = $1")
                .bind(answered.answer_message_id.unwrap())
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(answer_thread, Some(merge.message_id), "root cards are answered in their own thread");

        let hotfix = decisions.iter().find(|d| d.title == "Push the hotfix?").unwrap();
        dismiss_decision_in_pool(&pool, hotfix.id).await?;
        assert!(dismiss_decision_in_pool(&pool, hotfix.id).await.is_err());

        let rename = decisions.iter().find(|d| d.title == "Rename the flag?").unwrap();
        let prefix = rename.message_id.simple().to_string()[..8].to_owned();
        let other_run: Uuid = sqlx::query_scalar(
            "insert into agent_runs (agent_id, command, status) values ($1, 'codex', 'running') returning id",
        )
        .bind(other_agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let foreign = handle_agent_event(
            &pool,
            other_agent_id,
            other_run,
            decision_event(json!({"type": "decision_withdraw", "message_id": prefix})),
        )
        .await;
        assert!(foreign.is_err(), "agents can only withdraw their own decisions");
        handle_agent_event(
            &pool,
            agent_id,
            run_id,
            decision_event(json!({
                "type": "decision_withdraw",
                "message_id": format!("msg={prefix}"),
                "reason": "Settled in chat"
            })),
        )
        .await?;

        let statuses = sqlx::query("select title, status, answer_note from decisions order by title")
            .fetch_all(&pool)
            .await
            .map_err(|err| err.to_string())?
            .into_iter()
            .map(|row| {
                (
                    row.get::<String, _>("title"),
                    row.get::<String, _>("status"),
                    row.get::<String, _>("answer_note"),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            statuses,
            vec![
                ("Merge #12?".to_owned(), "answered".to_owned(), String::new()),
                ("Push the hotfix?".to_owned(), "dismissed".to_owned(), String::new()),
                ("Rename the flag?".to_owned(), "withdrawn".to_owned(), "Settled in chat".to_owned()),
            ]
        );
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}
