use super::{append_surface_context, same_codex_surface};
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

        let context = append_surface_context(
            &pool,
            "Default inbox item: summarize",
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
