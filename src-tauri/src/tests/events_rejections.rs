use uuid::Uuid;

use super::take_rejection_notice;
use crate::events::control::handle_streaming_agent_event_json;
use crate::test_support::{drop_test_schema, insert_test_agent, test_pool};

async fn work_item(pool: &sqlx::SqlitePool, agent_id: Uuid, status: &str) -> Uuid {
    sqlx::query_scalar(
        "insert into agent_work_items (agent_id, title, status) values ($1, 'request', $2) returning id",
    )
    .bind(agent_id)
    .bind(status)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn set_status(pool: &sqlx::SqlitePool, work_item_id: Uuid, status: &str) {
    sqlx::query("update agent_work_items set status = $2 where id = $1")
        .bind(work_item_id)
        .bind(status)
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_rejected_line_is_told_to_the_agent_until_a_finished_turn_showed_it() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "rejections-agent").await?;
        let other_agent = insert_test_agent(&pool, "rejections-other").await?;
        let earlier = work_item(&pool, agent_id, "running").await;
        let run_id: Uuid = sqlx::query_scalar(
            "insert into agent_runs (agent_id, work_item_id, command, status) values ($1, $2, 'claude', 'running') returning id",
        )
        .bind(agent_id)
        .bind(earlier)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;

        let removed = r#"{"type":"memory_append","body":"Dylan prefers concise replies."}"#;
        // The streamed line and the final text bring the same line twice.
        for _ in 0..2 {
            handle_streaming_agent_event_json(&pool, agent_id, run_id, removed).await?;
        }
        handle_streaming_agent_event_json(&pool, agent_id, run_id, "{not json").await?;
        // An accepted line is not reported.
        handle_streaming_agent_event_json(
            &pool,
            agent_id,
            run_id,
            r#"{"type":"activity","kind":"thinking","title":"Reading"}"#,
        )
        .await?;
        set_status(&pool, earlier, "done").await;

        assert_eq!(
            take_rejection_notice(&pool, other_agent, work_item(&pool, other_agent, "queued").await)
                .await?,
            None,
            "only the agent that wrote the line hears about it"
        );

        let next = work_item(&pool, agent_id, "queued").await;
        let notice = take_rejection_notice(&pool, agent_id, next)
            .await?
            .expect("a notice for the next turn");
        assert!(notice.starts_with("Lantor rejected control lines from your earlier replies"));
        assert_eq!(notice.matches("The line began").count(), 2, "{notice}");
        assert!(
            notice.contains("`memory_append`: unknown variant `memory_append`\n"),
            "{notice}"
        );
        assert!(notice.contains("Dylan prefers concise replies."), "{notice}");
        assert!(notice.contains("edit") || notice.contains("file tools"), "{notice}");
        assert!(notice.contains("a LANTOR_EVENT line"), "{notice}");
        assert!(!notice.contains("Reading"), "{notice}");

        // The start was put off: the request is still queued, so its next
        // start shows the notice again.
        assert!(take_rejection_notice(&pool, agent_id, next).await?.is_some());

        // A turn that failed did not finish reading it either.
        set_status(&pool, next, "failed").await;
        let retry = work_item(&pool, agent_id, "queued").await;
        assert!(take_rejection_notice(&pool, agent_id, retry).await?.is_some());

        // Once a turn that showed it is done, it is not repeated.
        set_status(&pool, retry, "done").await;
        let later = work_item(&pool, agent_id, "queued").await;
        assert_eq!(take_rejection_notice(&pool, agent_id, later).await?, None);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}
