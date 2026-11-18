use super::{render_surface_context, same_codex_surface};
use crate::channels::open_dm_with_agent_in_pool;
use crate::test_support::{drop_test_schema, insert_test_agent, insert_test_channel, test_pool};
use uuid::Uuid;

#[tokio::test]
async fn surface_context_injects_only_current_thread_messages() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let channel_id = insert_test_channel(&pool, "claude-context").await?;
        let thread_a: Uuid = sqlx::query_scalar(
            r#"
            insert into messages (channel_id, sender_name, sender_role, body, is_task)
            values ($1, 'Dylan', 'owner', 'thread A root', false)
            returning id
            "#,
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let thread_b: Uuid = sqlx::query_scalar(
            r#"
            insert into messages (channel_id, sender_name, sender_role, body, is_task)
            values ($1, 'Dylan', 'owner', 'thread B root with forbidden bleed', false)
            returning id
            "#,
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        sqlx::query(
            r#"
            insert into messages (channel_id, thread_root_id, sender_name, sender_role, body, is_task)
            values ($1, $2, 'Agent', 'agent', 'thread A reply evidence', false),
                   ($1, $3, 'Agent', 'agent', 'thread B reply forbidden bleed', false)
            "#,
        )
        .bind(channel_id)
        .bind(thread_a)
        .bind(thread_b)
        .execute(&pool)
        .await
        .map_err(|err| err.to_string())?;

        let context = render_surface_context(
            &pool,
            "Default inbox item: summarize",
            None,
            Some(channel_id),
            Some("claude-context"),
            Some(thread_a),
        )
        .await?
        .context;
        assert!(context.contains("Same-thread recent context"));
        assert!(context.contains("Resolve contextual follow-ups from this block"));
        assert!(context.contains("thread A root"));
        assert!(context.contains("thread A reply evidence"));
        assert!(!context.contains("thread B root with forbidden bleed"));
        assert!(!context.contains("thread B reply forbidden bleed"));
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn dm_codex_surface_requires_same_thread_root() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "dm-surface").await?;
        let dm_channel_id = Uuid::parse_str(&open_dm_with_agent_in_pool(&pool, agent_id).await?)
            .map_err(|err| err.to_string())?;
        let root_id: Uuid = sqlx::query_scalar(
            r#"
            insert into messages (channel_id, sender_name, sender_role, body, is_task)
            values ($1, 'Dylan', 'owner', 'thread root', false)
            returning id
            "#,
        )
        .bind(dm_channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;

        assert!(
            same_codex_surface(&pool, Some(dm_channel_id), None, Some(dm_channel_id), None).await?
        );
        assert!(
            same_codex_surface(
                &pool,
                Some(dm_channel_id),
                Some(root_id),
                Some(dm_channel_id),
                Some(root_id),
            )
            .await?
        );
        assert!(
            !same_codex_surface(
                &pool,
                Some(dm_channel_id),
                Some(root_id),
                Some(dm_channel_id),
                None,
            )
            .await?
        );
        assert!(
            !same_codex_surface(
                &pool,
                Some(dm_channel_id),
                None,
                Some(dm_channel_id),
                Some(root_id),
            )
            .await?
        );
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn a_session_that_holds_the_thread_gets_only_what_others_posted_since() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "delta-agent").await?;
        let other_agent = insert_test_agent(&pool, "other-agent").await?;
        let channel_id = insert_test_channel(&pool, "delta-context").await?;
        let post = |body: &'static str,
                    sender: Option<Uuid>,
                    stream_key: &'static str,
                    delivery_state: &'static str,
                    thread_root_id: Option<Uuid>| {
            let pool = pool.clone();
            async move {
                let id: Uuid = sqlx::query_scalar(
                    r#"
                    insert into messages (
                        channel_id, thread_root_id, sender_agent_id, sender_name, sender_role,
                        body, stream_key, delivery_state, is_task
                    )
                    values ($1, $2, $3, 'sender', case when $3 is null then 'owner' else 'agent' end,
                            $4, $5, $6, false)
                    returning id
                    "#,
                )
                .bind(channel_id)
                .bind(thread_root_id)
                .bind(sender)
                .bind(body)
                .bind(stream_key)
                .bind(delivery_state)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
                // A trigger assigns the seq after the insert.
                let seq: i64 = sqlx::query_scalar("select seq from messages where id = $1")
                    .bind(id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
                Ok::<_, String>((id, seq))
            }
        };
        let (root, _) = post("root question", None, "", "complete", None).await?;
        let (_, held_seq) = post("first detail", None, "", "complete", Some(root)).await?;
        post("own streamed answer", Some(agent_id), "run-1:claude-assistant", "complete", Some(root)).await?;
        post("own loop output", Some(agent_id), "", "complete", Some(root)).await?;
        let (_, last_complete_seq) = post("second question", None, "", "complete", Some(root)).await?;
        post("another agent is still writing", Some(other_agent), "run-2:claude-assistant", "streaming", Some(root)).await?;

        let render = || async {
            render_surface_context(
                &pool,
                "Default inbox item: wake",
                Some(agent_id),
                Some(channel_id),
                Some("delta-context"),
                Some(root),
            )
            .await
        };
        // No session holds the thread yet: the whole block, and no shorter form.
        let rendered = render().await?;
        assert!(rendered.delta.is_none());
        assert!(rendered.context.contains("root question"));
        assert_eq!(
            rendered.delivered_seq, last_complete_seq,
            "a message still being streamed is delivered once it is complete"
        );

        crate::freshness::advance_agent_target_watermark(
            &pool,
            agent_id,
            Some(channel_id),
            Some(root),
            held_seq,
        )
        .await?;
        crate::runtime::session_context::store_surface_delivery(
            &pool,
            agent_id,
            channel_id,
            Some(root),
            "session-1",
            held_seq,
        )
        .await?;
        let rendered = render().await?;
        assert!(rendered.context.contains("root question"), "the full form is unchanged");
        let (thread_id, delta) = rendered.delta.expect("a delta for the session");
        assert_eq!(thread_id, "session-1");
        assert!(delta.starts_with("Default inbox item: wake"));
        assert!(delta.contains("Same-thread recent context"));
        assert!(delta.contains("Only what others posted on this surface since your last turn"));
        for held in ["root question", "first detail", "own streamed answer"] {
            assert!(!delta.contains(held), "{held} is already in the session");
        }
        // The session never saw what Lantor posted in the agent's name.
        for new in ["own loop output", "second question", "another agent is still writing"] {
            assert_eq!(delta.matches(new).count(), 1, "{new}");
        }

        // Nothing but its own answers since: the block says so instead of
        // listing the thread again.
        crate::runtime::session_context::store_surface_delivery(
            &pool,
            agent_id,
            channel_id,
            Some(root),
            "session-1",
            last_complete_seq + 1,
        )
        .await?;
        let (_, delta) = render().await?.delta.expect("a delta for the session");
        assert!(delta.contains("No one else has posted on this surface since your last turn"));
        assert!(!delta.contains("second question\n"));
        assert!(!delta.contains("[target="));
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}
