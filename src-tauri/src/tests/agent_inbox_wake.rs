use super::{build_steer_followup_prompt, inbox_wake_context, InboxWakeItem, InboxWakeSummary};
use crate::channels::open_dm_with_agent_in_pool;
use crate::context_tool::short_id;
use crate::message_store::send_owner_message_in_pool;
use crate::prompts::WORK_ITEM_FINISH_PROMPT;
use crate::test_support::{drop_test_schema, insert_test_agent, insert_test_channel, test_pool};
use crate::ui_notifications::notify_ui_work_item_changed;
use chrono::Utc;
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

#[test]
fn inbox_wake_context_includes_message_headers_and_other_active_summary() {
    let channel_id = Uuid::new_v4();
    let thread_root_id = Uuid::new_v4();
    let source_message_id = Uuid::new_v4();
    let inbox_id = Uuid::new_v4();
    let context = inbox_wake_context(
        &[InboxWakeItem {
            id: inbox_id,
            channel_id: Some(channel_id),
            channel_name: Some("support".to_owned()),
            channel_kind: Some("channel".to_owned()),
            thread_root_id: Some(thread_root_id),
            source_message_id: Some(source_message_id),
            task_id: None,
            kind: "owner_thread_followup".to_owned(),
            priority: 90,
            title: "Handle follow-up".to_owned(),
            body_preview: "please use the latest numbers\nand reply directly".to_owned(),
            payload: "{}".to_owned(),
            message_created_at: Some(Utc::now()),
            sender_name: Some("Dylan".to_owned()),
            sender_role: Some("owner".to_owned()),
        }],
        &[InboxWakeSummary {
            target: "dm:Hancock".to_owned(),
            count: 2,
        }],
    );

    assert!(context.contains("[target=#support:"));
    assert!(context.contains(&format!("msg={}", short_id(source_message_id))));
    assert!(context.contains("type=owner"));
    assert!(context.contains("Dylan: please use the latest numbers and reply directly"));
    assert!(context.contains("Warm-runtime guard"));
    assert!(context.contains("use history-read on the default reply target"));
    assert!(context.contains(&format!("inbox_id: {inbox_id}")));
    assert!(context.contains("Other active inbox targets:"));
    assert!(context.contains("- dm:Hancock: 2 active"));
}

#[test]
fn inbox_wake_context_tells_task_available_agents_to_claim_silently() {
    let context = inbox_wake_context(
        &[InboxWakeItem {
            id: Uuid::new_v4(),
            channel_id: Some(Uuid::new_v4()),
            channel_name: Some("builders".to_owned()),
            channel_kind: Some("channel".to_owned()),
            thread_root_id: Some(Uuid::new_v4()),
            source_message_id: Some(Uuid::new_v4()),
            task_id: Some(Uuid::new_v4()),
            kind: "task_available".to_owned(),
            priority: 70,
            title: "Implement queue behavior".to_owned(),
            body_preview: "Implement queue behavior".to_owned(),
            payload: "{}".to_owned(),
            message_created_at: Some(Utc::now()),
            sender_name: Some("Dylan".to_owned()),
            sender_role: Some("owner".to_owned()),
        }],
        &[],
    );

    assert!(context.contains("Task claim opportunity mode:"));
    assert!(context.contains("competitive, unassigned task opportunity"));
    assert!(context.contains(r#"LANTOR_EVENT {"type":"task_claim","task_number":...}"#));
    assert!(context.contains("LANTOR_SILENT_REPLY: claim attempted"));
    assert!(context.contains("do not emit activity events"));
    assert!(context.contains("task_assigned inbox turn"));
}

#[test]
fn steer_followup_prompt_uses_compact_inbox_headers() {
    let thread_root_id = Uuid::new_v4();
    let source_message_id = Uuid::new_v4();
    let inbox_id = Uuid::new_v4();
    let prompt = build_steer_followup_prompt(&[InboxWakeItem {
        id: inbox_id,
        channel_id: Some(Uuid::new_v4()),
        channel_name: Some("support".to_owned()),
        channel_kind: Some("channel".to_owned()),
        thread_root_id: Some(thread_root_id),
        source_message_id: Some(source_message_id),
        task_id: None,
        kind: "owner_thread_followup".to_owned(),
        priority: 90,
        title: "Handle follow-up".to_owned(),
        body_preview: "please use the latest numbers\nand reply directly".to_owned(),
        payload: "{}".to_owned(),
        message_created_at: Some(Utc::now()),
        sender_name: Some("Dylan".to_owned()),
        sender_role: Some("owner".to_owned()),
    }]);

    assert!(prompt.contains("Same-channel/thread live inbox follow-up."));
    assert!(prompt.contains("Default reply target for normal assistant text: #support:"));
    assert!(prompt.contains(&format!("msg={}", short_id(source_message_id))));
    assert!(prompt.contains(&format!("inbox_id: {inbox_id}")));
    assert!(prompt.contains("archived automatically"));
    assert!(!prompt.contains("inbox-archive --inbox-id <id>"));
    assert!(!prompt.contains("Current Lantor inbox processing turn:"));
    assert!(!prompt.contains("title: Handle follow-up"));
    assert!(!prompt.contains("kind: owner_thread_followup"));
    assert!(!prompt.contains(WORK_ITEM_FINISH_PROMPT));
}

#[test]
fn interrupted_action_context_exposes_payload_and_resolution_protocol() {
    let stream_key = format!("{}:item-1", Uuid::new_v4());
    let context = inbox_wake_context(
        &[InboxWakeItem {
            id: Uuid::new_v4(),
            channel_id: Some(Uuid::new_v4()),
            channel_name: Some("coordination".to_owned()),
            channel_kind: Some("channel".to_owned()),
            thread_root_id: Some(Uuid::new_v4()),
            source_message_id: None,
            task_id: None,
            kind: "interrupted_action".to_owned(),
            priority: 95,
            title: "Public reply held because the thread changed".to_owned(),
            body_preview: "stale_context".to_owned(),
            payload: json!({
                "reason": "stale_context",
                "stream_key": stream_key,
                "action_kind": "reply_text",
                "base_version": 1,
                "current_version": 2,
                "base_thread_version": 2,
                "draft_body": "old answer",
                "allowed_actions": ["revise", "yield", "force_send"],
                "held_visible_events": []
            })
            .to_string(),
            message_created_at: None,
            sender_name: None,
            sender_role: None,
        }],
        &[],
    );

    assert!(context.contains("interrupted_action: review the held public output"));
    assert!(context.contains("allowed_actions: [\"revise\",\"yield\",\"force_send\"]"));
    assert!(context.contains("draft_body: old answer"));
    assert!(context.contains("interrupted_action_resolve"));
    assert!(context.contains("then continue with the revised visible reply"));
}

#[tokio::test]
async fn inbox_wake_creates_work_items_without_serializing_unread_items() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "inbox-reschedule").await?;
        let dm_channel_id = Uuid::parse_str(&open_dm_with_agent_in_pool(&pool, agent_id).await?)
            .map_err(|err| err.to_string())?;

        send_owner_message_in_pool(
            &pool,
            dm_channel_id,
            None,
            "first inbox item",
            false,
            vec![],
        )
        .await?;
        send_owner_message_in_pool(
            &pool,
            dm_channel_id,
            None,
            "second inbox item",
            false,
            vec![],
        )
        .await?;

        let initial_work_items: Vec<Uuid> = sqlx::query_scalar(
            "select id from agent_work_items where agent_id = $1 order by created_at asc",
        )
        .bind(agent_id)
        .fetch_all(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(initial_work_items.len(), 2);

        let state_counts = sqlx::query(
            r#"
            select
                count(*) filter (where state = 'processing') as processing,
                count(*) filter (where state = 'unread') as unread
            from agent_inbox_items
            where agent_id = $1
            "#,
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(state_counts.get::<Option<i64>, _>("processing"), Some(2));
        assert_eq!(state_counts.get::<Option<i64>, _>("unread"), Some(0));

        sqlx::query("update agent_work_items set status = 'done' where id = $1")
            .bind(initial_work_items[0])
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        notify_ui_work_item_changed(&pool, initial_work_items[0], "test_done").await;

        let final_work_items: Vec<Uuid> = sqlx::query_scalar(
            "select id from agent_work_items where agent_id = $1 order by created_at asc",
        )
        .bind(agent_id)
        .fetch_all(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(final_work_items.len(), 2);
        let state_counts = sqlx::query(
            r#"
            select
                count(*) filter (where state = 'archived') as archived,
                count(*) filter (where state = 'processing') as processing,
                count(*) filter (where state = 'unread') as unread
            from agent_inbox_items
            where agent_id = $1
            "#,
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(state_counts.get::<Option<i64>, _>("archived"), Some(1));
        assert_eq!(state_counts.get::<Option<i64>, _>("processing"), Some(1));
        assert_eq!(state_counts.get::<Option<i64>, _>("unread"), Some(0));
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn inbox_wake_batches_unread_items_for_same_thread() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "inbox-batch").await?;
        let channel_id = insert_test_channel(&pool, "batch-thread").await?;
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
            values ($1, 'Dylan', 'owner', '@inbox-batch please investigate', false)
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
            "first pending follow-up",
            false,
            vec![],
        )
        .await?;
        send_owner_message_in_pool(
            &pool,
            channel_id,
            Some(root_id),
            "second pending follow-up",
            false,
            vec![],
        )
        .await?;

        let rows = sqlx::query(
            r#"
            select
                w.id,
                w.title,
                w.context,
                count(i.id) as inbox_count,
                count(*) filter (where i.state = 'processing') as processing_count
            from agent_work_items w
            join agent_inbox_items i on i.work_item_id = w.id
            where w.agent_id = $1
              and w.channel_id = $2
              and w.thread_root_id = $3
              and w.source_kind = 'inbox_wake'
            group by w.id, w.title, w.context
            "#,
        )
        .bind(agent_id)
        .bind(channel_id)
        .bind(root_id)
        .fetch_all(&pool)
        .await
        .map_err(|err| err.to_string())?;

        assert_eq!(rows.len(), 1);
        let work_item_id: Uuid = rows[0].get("id");
        assert_eq!(rows[0].get::<i64, _>("inbox_count"), 2);
        assert_eq!(rows[0].get::<i64, _>("processing_count"), 2);
        assert_eq!(
            rows[0].get::<String, _>("title"),
            "Process inbox: first pending follow-up (+1 more)"
        );
        let context: String = rows[0].get("context");
        assert!(context.contains("batches 2 inbox items"));
        assert!(context.contains("first pending follow-up"));
        assert!(context.contains("second pending follow-up"));

        let start_commands: i64 = sqlx::query_scalar(
            r#"
            select count(*)
            from supervisor_commands
            where agent_id = $1 and command_type = 'start_agent'
            "#,
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(start_commands, 1);

        sqlx::query("update agent_work_items set status = 'done' where id = $1")
            .bind(work_item_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        notify_ui_work_item_changed(&pool, work_item_id, "test_done").await;
        let archived: i64 = sqlx::query_scalar(
            r#"
            select count(*)
            from agent_inbox_items
            where agent_id = $1 and work_item_id = $2 and state = 'archived'
            "#,
        )
        .bind(agent_id)
        .bind(work_item_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(archived, 2);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}
