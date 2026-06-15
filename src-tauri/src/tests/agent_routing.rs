use super::{
    extract_agent_mentions, queue_mentions_as_work_items, upsert_agent_thread_subscription,
    MentionDispatchOrigin,
};
use crate::channels::open_dm_with_agent_in_pool;
use crate::message_store::{
    insert_agent_message, send_owner_message_in_pool, send_owner_message_in_pool_with_client_id,
};
use crate::test_support::{drop_test_schema, insert_test_agent, insert_test_channel, test_pool};
use sqlx::Row;
use uuid::Uuid;

#[test]
fn extracts_unique_agent_mentions() {
    let mentions = extract_agent_mentions("ping @Hancock and @agent-2, then @Hancock again");
    assert_eq!(mentions, vec!["Hancock", "agent-2"]);
}

#[test]
fn extracts_mentions_after_non_ascii_text_and_punctuation() {
    let mentions = extract_agent_mentions("请@agent看一下，或者（@reviewer）再看 end.@observer");
    assert_eq!(mentions, vec!["agent", "reviewer", "observer"]);
}

#[test]
fn ignores_empty_or_email_like_at_signs() {
    let mentions = extract_agent_mentions("email a@b.com and a lone @ sign");
    assert!(mentions.is_empty());
}

#[tokio::test]
async fn dm_rejects_tasks_and_auto_dispatches_owner_messages() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "dm-dispatch").await?;
        let dm_channel_id =
            Uuid::parse_str(&open_dm_with_agent_in_pool(&pool, agent_id).await?)
                .map_err(|err| err.to_string())?;

        let owner_task_err =
            send_owner_message_in_pool(&pool, dm_channel_id, None, "task body", true, vec![])
                .await
                .unwrap_err();
        assert!(owner_task_err.contains("direct messages do not support tasks"));

        let agent_task_err =
            insert_agent_message(&pool, agent_id, dm_channel_id, None, "task body", true)
                .await
                .unwrap_err();
        assert!(agent_task_err.contains("direct messages do not support tasks"));

        send_owner_message_in_pool(
            &pool,
            dm_channel_id,
            None,
            "please inspect this",
            false,
            vec![],
        )
        .await?;
        let work_items: i64 = sqlx::query_scalar(
            r#"
            select count(*)
            from agent_work_items
            where channel_id = $1 and agent_id = $2 and status = 'queued'
            "#,
        )
        .bind(dm_channel_id)
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(work_items, 1);
        let source_kind: String = sqlx::query_scalar(
            "select source_kind from agent_work_items where channel_id = $1 and agent_id = $2",
        )
        .bind(dm_channel_id)
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(source_kind, "inbox_wake");
        let inbox = sqlx::query(
            r#"
            select i.id as inbox_item_id, i.kind, i.state, i.work_item_id, w.id as linked_work_item_id, w.inbox_item_id as linked_inbox_item_id
            from agent_inbox_items i
            join agent_work_items w on w.id = i.work_item_id
            where i.channel_id = $1 and i.agent_id = $2
            "#,
        )
        .bind(dm_channel_id)
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(inbox.get::<String, _>("kind"), "dm");
        assert_eq!(inbox.get::<String, _>("state"), "processing");
        assert_eq!(
            inbox.get::<Option<Uuid>, _>("work_item_id"),
            Some(inbox.get::<Uuid, _>("linked_work_item_id"))
        );
        assert_eq!(
            inbox.get::<Option<Uuid>, _>("linked_inbox_item_id"),
            Some(inbox.get::<Uuid, _>("inbox_item_id"))
        );
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn owner_client_message_id_is_idempotent_and_returned() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let channel_id = insert_test_channel(&pool, "client-id-channel").await?;
        let client_message_id = "client-message:local-test-send";

        let first = send_owner_message_in_pool_with_client_id(
            &pool,
            channel_id,
            None,
            "hello once",
            false,
            vec![],
            Some(client_message_id),
        )
        .await?;
        let second = send_owner_message_in_pool_with_client_id(
            &pool,
            channel_id,
            None,
            "hello once",
            false,
            vec![],
            Some(client_message_id),
        )
        .await?;

        assert_eq!(first.id, second.id);
        assert_eq!(first.stream_key, client_message_id);
        assert_eq!(second.body, "hello once");
        let message_count: i64 = sqlx::query_scalar(
            "select count(*) from messages where stream_key = $1 and sender_role = 'owner'",
        )
        .bind(client_message_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(message_count, 1);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn owner_channel_root_message_without_mentions_delivers_to_member_agent_inbox() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "channel-listener").await?;
        let channel_id = insert_test_channel(&pool, "channel-root-delivery").await?;
        sqlx::query(
            r#"
            insert into channel_members (channel_id, agent_id)
            values ($1, $2)
            "#,
        )
        .bind(channel_id)
        .bind(agent_id)
        .execute(&pool)
        .await
        .map_err(|err| err.to_string())?;

        send_owner_message_in_pool(
            &pool,
            channel_id,
            None,
            "Lantor README needs a quick review",
            false,
            vec![],
        )
        .await?;
        let message_id: Uuid =
            sqlx::query_scalar("select id from messages where channel_id = $1 and body = $2")
                .bind(channel_id)
                .bind("Lantor README needs a quick review")
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        let row = sqlx::query(
            r#"
            select
                w.source_kind,
                w.thread_root_id,
                w.source_message_id,
                i.kind as inbox_kind,
                i.priority,
                i.body_preview
            from agent_work_items w
            join agent_inbox_items i on i.work_item_id = w.id
            where w.agent_id = $1 and w.source_message_id = $2
            "#,
        )
        .bind(agent_id)
        .bind(message_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(row.get::<String, _>("source_kind"), "inbox_wake");
        assert_eq!(row.get::<String, _>("inbox_kind"), "channel_message");
        assert_eq!(row.get::<i32, _>("priority"), 35);
        assert_eq!(
            row.get::<Option<Uuid>, _>("thread_root_id"),
            Some(message_id)
        );
        assert_eq!(
            row.get::<Option<Uuid>, _>("source_message_id"),
            Some(message_id)
        );
        assert!(row
            .get::<String, _>("body_preview")
            .contains("README needs"));
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn owner_channel_root_message_does_not_dispatch_error_agent() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "quota-paused").await?;
        sqlx::query("update agents set status = 'error' where id = $1")
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        let channel_id = insert_test_channel(&pool, "error-agent-channel").await?;
        sqlx::query(
            r#"
            insert into channel_members (channel_id, agent_id)
            values ($1, $2)
            "#,
        )
        .bind(channel_id)
        .bind(agent_id)
        .execute(&pool)
        .await
        .map_err(|err| err.to_string())?;

        send_owner_message_in_pool(
            &pool,
            channel_id,
            None,
            "This should not wake a quota-limited agent",
            false,
            vec![],
        )
        .await?;

        let inbox_count: i64 =
            sqlx::query_scalar("select count(*) from agent_inbox_items where agent_id = $1")
                .bind(agent_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        let work_count: i64 =
            sqlx::query_scalar("select count(*) from agent_work_items where agent_id = $1")
                .bind(agent_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(inbox_count, 0);
        assert_eq!(work_count, 0);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn owner_mention_does_not_dispatch_error_agent() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "quota-mentioned").await?;
        sqlx::query("update agents set status = 'error' where id = $1")
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        let channel_id = insert_test_channel(&pool, "error-agent-mention").await?;

        send_owner_message_in_pool(
            &pool,
            channel_id,
            None,
            "@quota-mentioned please check this",
            false,
            vec![],
        )
        .await?;

        let inbox_count: i64 =
            sqlx::query_scalar("select count(*) from agent_inbox_items where agent_id = $1")
                .bind(agent_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        let work_count: i64 =
            sqlx::query_scalar("select count(*) from agent_work_items where agent_id = $1")
                .bind(agent_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(inbox_count, 0);
        assert_eq!(work_count, 0);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn owner_thread_followup_dispatches_to_thread_agents_without_mentions() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "thread-agent").await?;
        let channel_id = insert_test_channel(&pool, "thread-followup").await?;
        sqlx::query(
            r#"
            insert into channel_members (channel_id, agent_id)
            values ($1, $2)
            "#,
        )
        .bind(channel_id)
        .bind(agent_id)
        .execute(&pool)
        .await
        .map_err(|err| err.to_string())?;

        let root_id: Uuid = sqlx::query_scalar(
            r#"
            insert into messages (channel_id, sender_name, sender_role, body, is_task)
            values ($1, 'Dylan', 'owner', '@thread-agent please investigate', false)
            returning id
            "#,
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        sqlx::query(
            r#"
            insert into agent_work_items (
                agent_id, channel_id, thread_root_id, source_message_id, title, context, status
            )
            values ($1, $2, $3, $3, 'Initial dispatch', 'Initial dispatch', 'done')
            "#,
        )
        .bind(agent_id)
        .bind(channel_id)
        .bind(root_id)
        .execute(&pool)
        .await
        .map_err(|err| err.to_string())?;

        send_owner_message_in_pool(
            &pool,
            channel_id,
            Some(root_id),
            "我补充一下：这个复现只在 thread 里出现",
            false,
            vec![],
        )
        .await?;

        let work_items = sqlx::query(
            r#"
            select
                w.source_message_id,
                w.source_kind,
                w.title,
                w.context,
                i.kind as inbox_kind,
                i.body_preview
            from agent_work_items w
            join agent_inbox_items i on i.work_item_id = w.id
            where w.channel_id = $1 and w.agent_id = $2 and w.source_message_id <> $3
            "#,
        )
        .bind(channel_id)
        .bind(agent_id)
        .bind(root_id)
        .fetch_all(&pool)
        .await
        .map_err(|err| err.to_string())?;

        assert_eq!(work_items.len(), 1);
        assert_eq!(work_items[0].get::<String, _>("source_kind"), "inbox_wake");
        assert_eq!(
            work_items[0].get::<String, _>("inbox_kind"),
            "thread_followup"
        );
        assert_eq!(
            work_items[0]
                .get::<Option<String>, _>("title")
                .unwrap_or_default(),
            "Process inbox: 我补充一下：这个复现只在 thread 里出现"
        );
        let context: String = work_items[0].get("context");
        assert!(context.contains("Lantor agent inbox wake"));
        assert!(context.contains("inbox-read"));
        assert!(work_items[0]
            .get::<String, _>("body_preview")
            .contains("这个复现"));
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn owner_thread_followup_with_explicit_mention_does_not_fan_out_to_thread_agents() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let mentioned_agent_id = insert_test_agent(&pool, "mentioned-agent").await?;
        let bystander_agent_id = insert_test_agent(&pool, "bystander-agent").await?;
        let channel_id = insert_test_channel(&pool, "thread-explicit-owner").await?;
        for agent_id in [mentioned_agent_id, bystander_agent_id] {
            sqlx::query(
                r#"
                insert into channel_members (channel_id, agent_id)
                values ($1, $2)
                "#,
            )
            .bind(channel_id)
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        }

        let root_id: Uuid = sqlx::query_scalar(
            r#"
            insert into messages (channel_id, sender_name, sender_role, body, is_task)
            values ($1, 'Dylan', 'owner', 'root request', false)
            returning id
            "#,
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        upsert_agent_thread_subscription(
            &pool,
            mentioned_agent_id,
            channel_id,
            root_id,
            "mention",
            Some(root_id),
        )
        .await?;
        upsert_agent_thread_subscription(
            &pool,
            bystander_agent_id,
            channel_id,
            root_id,
            "mention",
            Some(root_id),
        )
        .await?;

        send_owner_message_in_pool(
            &pool,
            channel_id,
            Some(root_id),
            "@mentioned-agent 这个后续只给被点名的 agent",
            false,
            vec![],
        )
        .await?;

        let mentioned_inboxes: i64 = sqlx::query_scalar(
            r#"
            select count(*)
            from agent_inbox_items
            where agent_id = $1
              and source_message_id <> $2
              and kind = 'mention'
            "#,
        )
        .bind(mentioned_agent_id)
        .bind(root_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let bystander_inboxes: i64 = sqlx::query_scalar(
            r#"
            select count(*)
            from agent_inbox_items
            where agent_id = $1
              and source_message_id <> $2
            "#,
        )
        .bind(bystander_agent_id)
        .bind(root_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;

        assert_eq!(mentioned_inboxes, 1);
        assert_eq!(bystander_inboxes, 0);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn owner_thread_followup_with_unknown_mention_does_not_fall_back_to_thread_agents() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "known-thread-agent").await?;
        let channel_id = insert_test_channel(&pool, "thread-unknown-mention").await?;
        sqlx::query(
            r#"
            insert into channel_members (channel_id, agent_id)
            values ($1, $2)
            "#,
        )
        .bind(channel_id)
        .bind(agent_id)
        .execute(&pool)
        .await
        .map_err(|err| err.to_string())?;

        let root_id: Uuid = sqlx::query_scalar(
            r#"
            insert into messages (channel_id, sender_name, sender_role, body, is_task)
            values ($1, 'Dylan', 'owner', 'root request', false)
            returning id
            "#,
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        upsert_agent_thread_subscription(
            &pool,
            agent_id,
            channel_id,
            root_id,
            "mention",
            Some(root_id),
        )
        .await?;

        send_owner_message_in_pool(
            &pool,
            channel_id,
            Some(root_id),
            "@missing-agent 这个后续不应该 fallback 给 thread 参与者",
            false,
            vec![],
        )
        .await?;

        let inboxes: i64 = sqlx::query_scalar(
            r#"
            select count(*)
            from agent_inbox_items
            where agent_id = $1
              and source_message_id <> $2
            "#,
        )
        .bind(agent_id)
        .bind(root_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;

        assert_eq!(inboxes, 0);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn owner_thread_followup_uses_agent_thread_subscription_after_work_done() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "sub-agent").await?;
        let channel_id = insert_test_channel(&pool, "thread-subscription").await?;
        sqlx::query(
            r#"
            insert into channel_members (channel_id, agent_id)
            values ($1, $2)
            "#,
        )
        .bind(channel_id)
        .bind(agent_id)
        .execute(&pool)
        .await
        .map_err(|err| err.to_string())?;

        let root_id: Uuid = sqlx::query_scalar(
            r#"
            insert into messages (channel_id, sender_name, sender_role, body, is_task)
            values ($1, 'Dylan', 'owner', 'root request', false)
            returning id
            "#,
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        upsert_agent_thread_subscription(
            &pool,
            agent_id,
            channel_id,
            root_id,
            "mention",
            Some(root_id),
        )
        .await?;

        send_owner_message_in_pool(
            &pool,
            channel_id,
            Some(root_id),
            "继续补充，不需要重新 @ agent",
            false,
            vec![],
        )
        .await?;

        let row = sqlx::query(
            r#"
            select w.source_kind, w.thread_root_id, w.task_id, i.kind as inbox_kind
            from agent_work_items w
            join agent_inbox_items i on i.work_item_id = w.id
            where w.agent_id = $1 and w.source_message_id <> $2
            order by w.created_at desc
            limit 1
            "#,
        )
        .bind(agent_id)
        .bind(root_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(row.get::<String, _>("source_kind"), "inbox_wake");
        assert_eq!(row.get::<String, _>("inbox_kind"), "thread_followup");
        assert_eq!(row.get::<Option<Uuid>, _>("thread_root_id"), Some(root_id));
        assert_eq!(row.get::<Option<Uuid>, _>("task_id"), None);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn mentions_create_agent_requests_but_only_task_mode_creates_global_tasks() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "task-agent").await?;
        let channel_id = insert_test_channel(&pool, "task-semantics").await?;

        send_owner_message_in_pool(
            &pool,
            channel_id,
            None,
            "@task-agent please look at this",
            false,
            vec![],
        )
        .await?;
        let task_count: i64 = sqlx::query_scalar("select count(*) from tasks")
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert_eq!(task_count, 0);
        let mention_inbox_kind: String = sqlx::query_scalar(
            "select kind from agent_inbox_items where agent_id = $1 order by created_at desc limit 1",
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(mention_inbox_kind, "mention");

        send_owner_message_in_pool(
            &pool,
            channel_id,
            None,
            "@task-agent implement the tracked feature",
            true,
            vec![],
        )
        .await?;
        let task_count: i64 = sqlx::query_scalar("select count(*) from tasks")
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert_eq!(task_count, 1);
        let task = sqlx::query("select status, assignee_agent_id from tasks limit 1")
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert_eq!(task.get::<String, _>("status"), "in_progress");
        assert_eq!(
            task.get::<Option<Uuid>, _>("assignee_agent_id"),
            Some(agent_id)
        );
        let task_inbox_kind: String = sqlx::query_scalar(
            "select kind from agent_inbox_items where agent_id = $1 order by created_at desc limit 1",
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(task_inbox_kind, "task_assigned");
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn agent_mentions_dispatch_other_agents_once_in_same_thread() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let author_id = insert_test_agent(&pool, "author").await?;
        let reviewer_id = insert_test_agent(&pool, "reviewer").await?;
        let outsider_id = insert_test_agent(&pool, "outsider").await?;
        let channel_id = insert_test_channel(&pool, "collab").await?;
        sqlx::query(
            r#"
            insert into channel_members (channel_id, agent_id)
            values ($1, $2), ($1, $3)
            "#,
        )
        .bind(channel_id)
        .bind(author_id)
        .bind(reviewer_id)
        .execute(&pool)
        .await
        .map_err(|err| err.to_string())?;

        let message_id = insert_agent_message(
            &pool,
            author_id,
            channel_id,
            None,
            "@author ignore self, @reviewer please review this, @outsider ignore non-member",
            false,
        )
        .await?;
        queue_mentions_as_work_items(
            &pool,
            channel_id,
            None,
            message_id,
            None,
            "@reviewer please review this again",
            MentionDispatchOrigin::Agent {
                sender_agent_id: author_id,
                allow_channel_member_invite: false,
            },
        )
        .await?;

        let work_items = sqlx::query(
            r#"
            select agent_id, thread_root_id, source_message_id
            from agent_work_items
            where source_message_id = $1
            "#,
        )
        .bind(message_id)
        .fetch_all(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(work_items.len(), 1);
        assert_eq!(work_items[0].get::<Uuid, _>("agent_id"), reviewer_id);
        assert_ne!(work_items[0].get::<Uuid, _>("agent_id"), outsider_id);
        assert_eq!(
            work_items[0].get::<Option<Uuid>, _>("thread_root_id"),
            Some(message_id)
        );
        assert_eq!(
            work_items[0].get::<Option<Uuid>, _>("source_message_id"),
            Some(message_id)
        );
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn agent_mentions_do_not_cross_dispatch_from_dm() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let dm_agent_id = insert_test_agent(&pool, "dm-agent").await?;
        let reviewer_id = insert_test_agent(&pool, "dm-reviewer").await?;
        let dm_channel_id = Uuid::parse_str(&open_dm_with_agent_in_pool(&pool, dm_agent_id).await?)
            .map_err(|err| err.to_string())?;

        insert_agent_message(
            &pool,
            dm_agent_id,
            dm_channel_id,
            None,
            "@dm-reviewer please join this DM",
            false,
        )
        .await?;
        let work_items: i64 = sqlx::query_scalar(
            r#"
            select count(*)
            from agent_work_items
            where channel_id = $1 and agent_id = $2
            "#,
        )
        .bind(dm_channel_id)
        .bind(reviewer_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(work_items, 0);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn agent_collaboration_pauses_after_thread_limit() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let author_id = insert_test_agent(&pool, "loop-author").await?;
        let reviewer_id = insert_test_agent(&pool, "loop-reviewer").await?;
        let channel_id = insert_test_channel(&pool, "loop-guard").await?;
        sqlx::query(
            r#"
            insert into channel_members (channel_id, agent_id)
            values ($1, $2), ($1, $3)
            "#,
        )
        .bind(channel_id)
        .bind(author_id)
        .bind(reviewer_id)
        .execute(&pool)
        .await
        .map_err(|err| err.to_string())?;

        let root_id: Uuid = sqlx::query_scalar(
            r#"
            insert into messages (channel_id, sender_name, sender_role, body, is_task)
            values ($1, 'Dylan', 'owner', 'start collaboration', false)
            returning id
            "#,
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        for index in 0..10 {
            insert_agent_message(
                &pool,
                author_id,
                channel_id,
                Some(root_id),
                &format!("agent loop message {index}"),
                false,
            )
            .await?;
        }
        insert_agent_message(
            &pool,
            author_id,
            channel_id,
            Some(root_id),
            "@loop-reviewer continue the loop",
            false,
        )
        .await?;

        let work_items: i64 = sqlx::query_scalar(
            r#"
            select count(*)
            from agent_work_items
            where channel_id = $1 and agent_id = $2
            "#,
        )
        .bind(channel_id)
        .bind(reviewer_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(work_items, 0);
        let system_messages: i64 = sqlx::query_scalar(
            r#"
            select count(*)
            from messages
            where channel_id = $1
              and thread_root_id = $2
              and sender_role = 'system'
              and body like 'Inter-agent collaboration paused:%'
            "#,
        )
        .bind(channel_id)
        .bind(root_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(system_messages, 1);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}
