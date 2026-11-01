use super::{
    claim_agent_event, extract_agent_event_json, handle_agent_event,
    handle_streaming_agent_event_json, parse_agent_event, AgentEvent,
};
use crate::attachments::AgentAttachmentFile;
use crate::channels::open_dm_with_agent_in_pool;
use crate::context_tool::agent_context_artifact_read_in_pool;
use crate::message_store::load_messages;
use crate::test_support::{drop_test_schema, insert_test_agent, insert_test_channel, test_pool};
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

#[tokio::test]
async fn artifact_create_event_persists_artifact_and_message_card() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "artifact-agent").await?;
        let channel_id = insert_test_channel(&pool, "artifacts").await?;
        let run_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_runs (agent_id, command, status)
            values ($1, 'codex app-server', 'running')
            returning id
            "#,
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;

        handle_agent_event(
            &pool,
            agent_id,
            run_id,
            AgentEvent::ArtifactCreate {
                channel: None,
                channel_id: Some(channel_id),
                thread_root_id: None,
                kind: "markdown".to_owned(),
                title: "Review report".to_owned(),
                summary: Some("Two findings and one follow-up.".to_owned()),
                content: "# Review report\n\n- finding".to_owned(),
                metadata: Some(json!({"source": "test"})),
            },
        )
        .await?;

        let artifact = sqlx::query(
            r#"
            select id, message_id, kind, title, summary, content, metadata
            from artifacts
            where channel_id = $1
            "#,
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let artifact_id: Uuid = artifact.get("id");
        let message_id: Uuid = artifact.get("message_id");
        assert_eq!(artifact.get::<String, _>("kind"), "markdown");
        assert_eq!(artifact.get::<String, _>("title"), "Review report");
        assert_eq!(
            artifact.get::<String, _>("summary"),
            "Two findings and one follow-up."
        );

        let messages = load_messages(&pool).await?;
        let message = messages
            .iter()
            .find(|message| message.id == message_id)
            .expect("artifact message should be loaded");
        assert!(message.body.contains("Created artifact: Review report"));
        assert_eq!(message.artifacts.len(), 1);
        assert_eq!(message.artifacts[0].id, artifact_id);

        let tool_output = agent_context_artifact_read_in_pool(
            &pool,
            &[
                "artifact-read".to_owned(),
                "--artifact-id".to_owned(),
                artifact_id.to_string(),
            ],
        )
        .await?;
        assert!(tool_output.contains("kind=markdown"));
        assert!(tool_output.contains("# Review report"));
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[test]
fn parse_agent_event_reads_channel_fields_by_their_form() {
    let id = Uuid::new_v4();
    let channel_of = |event: Value| match parse_agent_event(&event.to_string()) {
        Ok(AgentEvent::ChannelMessageCreate {
            channel,
            channel_id,
            ..
        }) => (channel, channel_id),
        other => panic!("unexpected parse result {other:?}"),
    };
    let message = |fields: Value| {
        let mut event = json!({"type": "channel_message_create", "body": "hello"});
        event
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        event
    };

    assert_eq!(channel_of(message(json!({"channel_id": ""}))), (None, None));
    assert_eq!(
        channel_of(message(json!({"channel_id": "#all"}))),
        (Some("#all".to_owned()), None)
    );
    assert_eq!(
        channel_of(message(json!({"channel": id.to_string()}))),
        (None, Some(id))
    );
    assert_eq!(
        channel_of(message(
            json!({"channel": "#all", "channel_id": id.to_string()})
        )),
        (Some("#all".to_owned()), Some(id))
    );
    // Two names leave the event as written, so it is still rejected.
    assert!(parse_agent_event(
        &message(json!({"channel": "#all", "channel_id": "#design"})).to_string()
    )
    .is_err());
}

#[tokio::test]
async fn artifact_with_a_blank_or_named_channel_still_lands() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "channel-guesser").await?;
        let turn_channel_id = insert_test_channel(&pool, "turn-channel").await?;
        let named_channel_id = insert_test_channel(&pool, "named-channel").await?;
        let root_id: Uuid = sqlx::query_scalar(
            "insert into messages (channel_id, sender_name, sender_role, body) values ($1, 'Dylan', 'owner', 'make a report') returning id",
        )
        .bind(turn_channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let work_item_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_work_items (
                agent_id, channel_id, thread_root_id, source_message_id,
                source_kind, title, context, status
            )
            values ($1, $2, $3, $3, 'mention', 'report', 'context', 'running')
            returning id
            "#,
        )
        .bind(agent_id)
        .bind(turn_channel_id)
        .bind(root_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let run_id: Uuid = sqlx::query_scalar(
            "insert into agent_runs (agent_id, work_item_id, command, status) values ($1, $2, 'claude', 'running') returning id",
        )
        .bind(agent_id)
        .bind(work_item_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;

        for (channel_id, title) in [("", "Turn report"), ("#named-channel", "Named report")] {
            let event = json!({
                "type": "artifact_create",
                "channel_id": channel_id,
                "kind": "markdown",
                "title": title,
                "content": format!("# {title}"),
            });
            handle_streaming_agent_event_json(&pool, agent_id, run_id, &event.to_string()).await?;
        }

        let placed: Vec<(String, Uuid, Option<Uuid>)> = sqlx::query(
            r#"
            select a.title, m.channel_id, m.thread_root_id
            from artifacts a
            join messages m on m.id = a.message_id
            order by a.title
            "#,
        )
        .fetch_all(&pool)
        .await
        .map_err(|err| err.to_string())?
        .into_iter()
        .map(|row| (row.get("title"), row.get("channel_id"), row.get("thread_root_id")))
        .collect();
        assert_eq!(
            placed,
            vec![
                ("Named report".to_owned(), named_channel_id, None),
                ("Turn report".to_owned(), turn_channel_id, Some(root_id)),
            ]
        );
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn attachment_create_event_imports_local_file_as_message_attachment() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "attachment-agent").await?;
        let channel_id = insert_test_channel(&pool, "attachments").await?;
        let run_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_runs (agent_id, command, status)
            values ($1, 'codex app-server', 'running')
            returning id
            "#,
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;

        let dir = std::env::temp_dir().join(format!("lantor-attachment-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
        let source_path = dir.join("generated.png");
        let source_bytes = b"\x89PNG\r\n\x1a\nlantor-test-image";
        std::fs::write(&source_path, source_bytes).map_err(|err| err.to_string())?;

        handle_agent_event(
            &pool,
            agent_id,
            run_id,
            AgentEvent::AttachmentCreate {
                channel: None,
                channel_id: Some(channel_id),
                thread_root_id: None,
                body: Some("Generated architecture diagram".to_owned()),
                files: vec![AgentAttachmentFile {
                    path: source_path.to_string_lossy().to_string(),
                    name: Some("architecture.png".to_owned()),
                    mime_type: Some("image/png".to_owned()),
                }],
            },
        )
        .await?;

        let messages = load_messages(&pool).await?;
        let message = messages
            .iter()
            .find(|message| message.body == "Generated architecture diagram")
            .expect("attachment message should be loaded");
        assert_eq!(message.attachments.len(), 1);
        let attachment = &message.attachments[0];
        assert_eq!(attachment.original_name, "architecture.png");
        assert_eq!(attachment.mime_type, "image/png");
        assert_eq!(attachment.size_bytes, source_bytes.len() as i64);
        assert_ne!(
            attachment.storage_path.as_str(),
            source_path.to_string_lossy().as_ref()
        );
        assert_eq!(
            std::fs::read(&attachment.storage_path).map_err(|err| err.to_string())?,
            source_bytes
        );

        let stored_path = std::path::PathBuf::from(&attachment.storage_path);
        let _ = std::fs::remove_file(&stored_path);
        if let Some(parent) = stored_path.parent() {
            let _ = std::fs::remove_dir(parent);
        }
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn attachment_create_event_accepts_dm_agent_id_as_channel_id() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "dm-attachment-agent").await?;
        let dm_channel_id = Uuid::parse_str(&open_dm_with_agent_in_pool(&pool, agent_id).await?)
            .map_err(|err| err.to_string())?;
        let run_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_runs (agent_id, command, status)
            values ($1, 'codex app-server', 'running')
            returning id
            "#,
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;

        let dir =
            std::env::temp_dir().join(format!("lantor-dm-attachment-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
        let source_path = dir.join("parked.patch");
        let source_bytes = b"diff --git a/file b/file\n";
        std::fs::write(&source_path, source_bytes).map_err(|err| err.to_string())?;

        handle_agent_event(
            &pool,
            agent_id,
            run_id,
            AgentEvent::AttachmentCreate {
                channel: None,
                channel_id: Some(agent_id),
                thread_root_id: None,
                body: Some("Parked supervisor patch".to_owned()),
                files: vec![AgentAttachmentFile {
                    path: source_path.to_string_lossy().to_string(),
                    name: Some("parked.patch".to_owned()),
                    mime_type: Some("text/plain".to_owned()),
                }],
            },
        )
        .await?;

        let row = sqlx::query(
            r#"
            select channel_id, thread_root_id
            from messages
            where body = 'Parked supervisor patch'
            "#,
        )
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(row.get::<Uuid, _>("channel_id"), dm_channel_id);
        assert_eq!(row.get::<Option<Uuid>, _>("thread_root_id"), None);

        let stored_path: String = sqlx::query_scalar(
            r#"
            select ma.storage_path
            from message_attachments ma
            join messages m on m.id = ma.message_id
            where m.body = 'Parked supervisor patch'
              and ma.original_name = 'parked.patch'
            "#,
        )
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert!(!stored_path.is_empty());

        let stored_path = std::path::PathBuf::from(stored_path);
        let _ = std::fs::remove_file(&stored_path);
        if let Some(parent) = stored_path.parent() {
            let _ = std::fs::remove_dir(parent);
        }
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn handoff_create_dispatches_target_agent_to_existing_thread() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let source_agent_id = insert_test_agent(&pool, "source").await?;
        let target_agent_id = insert_test_agent(&pool, "target").await?;
        let channel_id = insert_test_channel(&pool, "handoff").await?;
        sqlx::query(
            r#"
            insert into channel_members (channel_id, agent_id)
            values ($1, $2)
            "#,
        )
        .bind(channel_id)
        .bind(source_agent_id)
        .execute(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let root_message_id: Uuid = sqlx::query_scalar(
            r#"
            insert into messages (channel_id, sender_name, sender_role, body, is_task)
            values ($1, 'Dylan', 'owner', 'Please investigate this thread', false)
            returning id
            "#,
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let run_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_runs (agent_id, command, status)
            values ($1, 'codex app-server', 'running')
            returning id
            "#,
        )
        .bind(source_agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;

        handle_agent_event(
            &pool,
            source_agent_id,
            run_id,
            AgentEvent::HandoffCreate {
                target_agent: "@target".to_owned(),
                channel: None,
                channel_id: Some(channel_id),
                thread_root_id: root_message_id,
                reason: Some("User asked target to continue".to_owned()),
                body: "@target Please continue the implementation from this thread.".to_owned(),
            },
        )
        .await?;

        let handoff_message = sqlx::query(
            r#"
            select id, sender_agent_id, sender_name, sender_role, body
            from messages
            where channel_id = $1
              and thread_root_id = $2
              and sender_agent_id = $3
              and sender_role = 'agent'
            "#,
        )
        .bind(channel_id)
        .bind(root_message_id)
        .bind(source_agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let handoff_message_id: Uuid = handoff_message.get("id");
        let handoff_body: String = handoff_message.get("body");
        assert_eq!(handoff_message.get::<String, _>("sender_name"), "source");
        assert_eq!(
            handoff_body,
            "@target Please continue the implementation from this thread."
        );
        assert!(!handoff_body.contains("Reason:"));

        let target_is_member: bool = sqlx::query_scalar(
            r#"
            select exists (
                select 1 from channel_members
                where channel_id = $1 and agent_id = $2
            )
            "#,
        )
        .bind(channel_id)
        .bind(target_agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert!(target_is_member);

        let work_item = sqlx::query(
            r#"
            select
                w.agent_id,
                w.thread_root_id,
                w.source_message_id,
                w.source_kind,
                w.title,
                w.context,
                w.status,
                i.kind as inbox_kind,
                i.state as inbox_state
            from agent_work_items w
            join agent_inbox_items i on i.work_item_id = w.id
            where w.source_message_id = $1
            "#,
        )
        .bind(handoff_message_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(work_item.get::<Uuid, _>("agent_id"), target_agent_id);
        assert_eq!(
            work_item.get::<Option<Uuid>, _>("thread_root_id"),
            Some(root_message_id)
        );
        assert_eq!(
            work_item.get::<Option<Uuid>, _>("source_message_id"),
            Some(handoff_message_id)
        );
        assert_eq!(work_item.get::<String, _>("source_kind"), "inbox_wake");
        assert_eq!(work_item.get::<String, _>("status"), "queued");
        assert!(work_item
            .get::<String, _>("context")
            .contains("Lantor agent inbox wake"));
        assert_eq!(work_item.get::<String, _>("inbox_kind"), "handoff");
        assert_eq!(work_item.get::<String, _>("inbox_state"), "processing");
        let target_work_items: i64 =
            sqlx::query_scalar("select count(*) from agent_work_items where agent_id = $1")
                .bind(target_agent_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(target_work_items, 1);

        let pending_start: i64 = sqlx::query_scalar(
            r#"
            select count(*)
            from supervisor_commands
            where agent_id = $1 and work_item_id in (
                select id from agent_work_items where source_message_id = $2
            )
            "#,
        )
        .bind(target_agent_id)
        .bind(handoff_message_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(pending_start, 1);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn task_handoff_reassigns_task_and_wakes_new_assignee() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let source_agent_id = insert_test_agent(&pool, "task-source").await?;
        let target_agent_id = insert_test_agent(&pool, "task-target").await?;
        let channel_id = insert_test_channel(&pool, "task-handoff").await?;
        sqlx::query(
            r#"
            insert into channel_members (channel_id, agent_id)
            values ($1, $2)
            "#,
        )
        .bind(channel_id)
        .bind(source_agent_id)
        .execute(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let root_message_id: Uuid = sqlx::query_scalar(
            r#"
            insert into messages (channel_id, sender_name, sender_role, body, is_task)
            values ($1, 'Dylan', 'owner', 'Implement task handoff', true)
            returning id
            "#,
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let task_row = sqlx::query(
            r#"
            insert into tasks (message_id, channel_id, title, status, assignee_agent_id)
            values ($1, $2, 'Implement task handoff', 'in_progress', $3)
            returning id, number
            "#,
        )
        .bind(root_message_id)
        .bind(channel_id)
        .bind(source_agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let task_id: Uuid = task_row.get("id");
        let task_number: i64 = task_row.get("number");
        let run_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_runs (agent_id, command, status)
            values ($1, 'codex app-server', 'running')
            returning id
            "#,
        )
        .bind(source_agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        sqlx::query(
            r#"
            insert into agent_work_items (
                agent_id, channel_id, thread_root_id, task_id, source_kind, title, context, status, run_id
            )
            values ($1, $2, $3, $4, 'task', 'Implement task handoff', '', 'running', $5)
            "#,
        )
        .bind(source_agent_id)
        .bind(channel_id)
        .bind(root_message_id)
        .bind(task_id)
        .bind(run_id)
        .execute(&pool)
        .await
        .map_err(|err| err.to_string())?;

        handle_agent_event(
            &pool,
            source_agent_id,
            run_id,
            AgentEvent::TaskHandoff {
                target_agent: "@task-target".to_owned(),
                task_number: None,
                reason: "Target owns the UI side".to_owned(),
                body: Some("@task-target please continue the UI wiring.".to_owned()),
            },
        )
        .await?;

        let task = sqlx::query("select status, assignee_agent_id from tasks where id = $1")
            .bind(task_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert_eq!(task.get::<String, _>("status"), "in_progress");
        assert_eq!(
            task.get::<Option<Uuid>, _>("assignee_agent_id"),
            Some(target_agent_id)
        );

        let handoff_message_body: String = sqlx::query_scalar(
            r#"
            select body
            from messages
            where channel_id = $1
              and thread_root_id = $2
              and sender_agent_id = $3
            "#,
        )
        .bind(channel_id)
        .bind(root_message_id)
        .bind(source_agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(
            handoff_message_body,
            "@task-target please continue the UI wiring."
        );

        let inbox = sqlx::query(
            r#"
            select kind, task_id, payload->>'reason' as reason
            from agent_inbox_items
            where agent_id = $1
            order by created_at desc
            limit 1
            "#,
        )
        .bind(target_agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(inbox.get::<String, _>("kind"), "task_assigned");
        assert_eq!(inbox.get::<Option<Uuid>, _>("task_id"), Some(task_id));
        assert_eq!(
            inbox.get::<Option<String>, _>("reason").as_deref(),
            Some("Target owns the UI side")
        );
        let target_is_member: bool = sqlx::query_scalar(
            r#"
            select exists (
                select 1 from channel_members
                where channel_id = $1 and agent_id = $2
            )
            "#,
        )
        .bind(channel_id)
        .bind(target_agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert!(target_is_member);
        let target_work_items: i64 = sqlx::query_scalar(
            "select count(*) from agent_work_items where agent_id = $1 and task_id = $2",
        )
        .bind(target_agent_id)
        .bind(task_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(target_work_items, 1);
        let activity_count: i64 = sqlx::query_scalar(
            r#"
            select count(*)
            from agent_activities
            where agent_id = $1
              and title = $2
              and metadata->>'target_agent' = '@task-target'
            "#,
        )
        .bind(source_agent_id)
        .bind(format!("Task #{task_number} handed off"))
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(activity_count, 1);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn channel_message_create_posts_agent_message_and_dispatches_mentions() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let source_agent_id = insert_test_agent(&pool, "source").await?;
        let target_agent_id = insert_test_agent(&pool, "target").await?;
        let channel_id = insert_test_channel(&pool, "channel-message").await?;
        sqlx::query(
            r#"
            insert into channel_members (channel_id, agent_id)
            values ($1, $2)
            "#,
        )
        .bind(channel_id)
        .bind(source_agent_id)
        .execute(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let run_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_runs (agent_id, command, status)
            values ($1, 'codex app-server', 'running')
            returning id
            "#,
        )
        .bind(source_agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;

        handle_agent_event(
            &pool,
            source_agent_id,
            run_id,
            AgentEvent::ChannelMessageCreate {
                channel: None,
                channel_id: Some(channel_id),
                thread_root_id: None,
                body: "@target please start this task in this channel.".to_owned(),
            },
        )
        .await?;

        let message = sqlx::query(
            r#"
            select id, thread_root_id, sender_agent_id, sender_name, sender_role, body, is_task
            from messages
            where channel_id = $1 and sender_agent_id = $2
            "#,
        )
        .bind(channel_id)
        .bind(source_agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let message_id: Uuid = message.get("id");
        assert_eq!(message.get::<Option<Uuid>, _>("thread_root_id"), None);
        assert_eq!(
            message.get::<Option<Uuid>, _>("sender_agent_id"),
            Some(source_agent_id)
        );
        assert_eq!(message.get::<String, _>("sender_name"), "source");
        assert_eq!(message.get::<String, _>("sender_role"), "agent");
        assert_eq!(
            message.get::<String, _>("body"),
            "@target please start this task in this channel."
        );
        assert!(!message.get::<bool, _>("is_task"));

        let message_upsert_count: i64 = sqlx::query_scalar(
            r#"
            select count(*)
            from ui_events
            where json_extract(event_json, '$.type') = 'message_upsert'
              and json_extract(event_json, '$.message.id') = $1
              and json_extract(event_json, '$.message.thread_root_id') is null
            "#,
        )
        .bind(message_id.to_string())
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(message_upsert_count, 1);

        let target_is_member: bool = sqlx::query_scalar(
            r#"
            select exists (
                select 1 from channel_members
                where channel_id = $1 and agent_id = $2
            )
            "#,
        )
        .bind(channel_id)
        .bind(target_agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert!(target_is_member);

        let work_item = sqlx::query(
            r#"
            select w.agent_id, w.thread_root_id, w.source_message_id, w.source_kind, i.kind as inbox_kind
            from agent_work_items w
            join agent_inbox_items i on i.work_item_id = w.id
            where w.source_message_id = $1 and w.agent_id = $2
            "#,
        )
        .bind(message_id)
        .bind(target_agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(work_item.get::<Uuid, _>("agent_id"), target_agent_id);
        assert_eq!(
            work_item.get::<Option<Uuid>, _>("thread_root_id"),
            Some(message_id)
        );
        assert_eq!(
            work_item.get::<Option<Uuid>, _>("source_message_id"),
            Some(message_id)
        );
        assert_eq!(work_item.get::<String, _>("source_kind"), "inbox_wake");
        assert_eq!(work_item.get::<String, _>("inbox_kind"), "collaboration");
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn channel_message_create_to_other_channel_ignores_source_surface_staleness() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "cross-surface-source").await?;
        let source_channel_id = insert_test_channel(&pool, "cross-source").await?;
        let target_channel_id = insert_test_channel(&pool, "cross-target").await?;
        for channel_id in [source_channel_id, target_channel_id] {
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
        let source_root_id: Uuid = sqlx::query_scalar(
            r#"
            insert into messages (channel_id, sender_name, sender_role, body, is_task)
            values ($1, 'Dylan', 'owner', 'write an announcement', false)
            returning id
            "#,
        )
        .bind(source_channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let source_root_seq: i64 = sqlx::query_scalar("select seq from messages where id = $1")
            .bind(source_root_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        let work_item_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_work_items (
                agent_id, channel_id, thread_root_id, source_message_id,
                source_kind, title, context, context_max_seq, status
            )
            values ($1, $2, $3, $3, 'inbox_wake', 'announce', 'context', $4, 'running')
            returning id
            "#,
        )
        .bind(agent_id)
        .bind(source_channel_id)
        .bind(source_root_id)
        .bind(source_root_seq)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let run_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_runs (agent_id, work_item_id, command, status)
            values ($1, $2, 'codex app-server', 'running')
            returning id
            "#,
        )
        .bind(agent_id)
        .bind(work_item_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        sqlx::query(
            r#"
            insert into messages (channel_id, thread_root_id, sender_name, sender_role, body)
            values ($1, $2, 'Dylan', 'owner', 'new source-thread reply')
            "#,
        )
        .bind(source_channel_id)
        .bind(source_root_id)
        .execute(&pool)
        .await
        .map_err(|err| err.to_string())?;

        handle_agent_event(
            &pool,
            agent_id,
            run_id,
            AgentEvent::ChannelMessageCreate {
                channel: None,
                channel_id: Some(target_channel_id),
                thread_root_id: None,
                body: "Cross-channel announcement".to_owned(),
            },
        )
        .await?;

        let posted_count: i64 = sqlx::query_scalar(
            r#"
            select count(*)
            from messages
            where channel_id = $1
              and sender_agent_id = $2
              and body = 'Cross-channel announcement'
            "#,
        )
        .bind(target_channel_id)
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(posted_count, 1);
        let held_count: i64 =
            sqlx::query_scalar("select count(*) from agent_held_outputs where work_item_id = $1")
                .bind(work_item_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(held_count, 0);
        let retry_count: i64 = sqlx::query_scalar(
            "select count(*) from agent_work_items where source_kind = 'freshness_retry'",
        )
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(retry_count, 0);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn channel_message_create_requires_source_channel_membership() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let source_agent_id = insert_test_agent(&pool, "source").await?;
        let channel_id = insert_test_channel(&pool, "channel-message-denied").await?;
        let run_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_runs (agent_id, command, status)
            values ($1, 'codex app-server', 'running')
            returning id
            "#,
        )
        .bind(source_agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;

        let err = handle_agent_event(
            &pool,
            source_agent_id,
            run_id,
            AgentEvent::ChannelMessageCreate {
                channel: None,
                channel_id: Some(channel_id),
                thread_root_id: None,
                body: "I should not be posted.".to_owned(),
            },
        )
        .await
        .expect_err("non-member channel_message_create should be rejected");
        assert!(err.contains("channel_message_create requires source agent channel membership"));

        let message_count: i64 =
            sqlx::query_scalar("select count(*) from messages where channel_id = $1")
                .bind(channel_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(message_count, 0);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn agent_event_receipts_hash_large_control_events() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "large-event-agent").await?;
        let run_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_runs (agent_id, command, status)
            values ($1, 'codex app-server', 'running')
            returning id
            "#,
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let markdown = format!(
            "# Large report\n\n{}",
            "- large artifact payload\n".repeat(400)
        );
        let event = json!({
            "type": "artifact_create",
            "channel_id": Uuid::new_v4(),
            "kind": "markdown",
            "title": "Large markdown artifact",
            "content": markdown
        })
        .to_string();

        assert!(claim_agent_event(&pool, run_id, &event).await?);
        assert!(!claim_agent_event(&pool, run_id, &event).await?);
        let (count, max_len): (i64, i64) = sqlx::query_as(
            "select count(*), max(length(event_json)) from agent_event_receipts where run_id = $1",
        )
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(count, 1);
        assert!(max_len > 2704);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[test]
fn extracts_plain_agent_event_lines() {
    assert_eq!(
        extract_agent_event_json(r#"LANTOR_EVENT {"type":"message","body":"ok"}"#),
        Some(r#"{"type":"message","body":"ok"}"#)
    );
}

#[test]
fn extracts_codex_wrapped_agent_event_lines() {
    assert_eq!(
        extract_agent_event_json(
            r#"[stderr] LANTOR_EVENT {"type":"message","body":"from tool output"}"#
        ),
        Some(r#"{"type":"message","body":"from tool output"}"#)
    );
}

#[test]
fn extracts_stdout_wrapped_agent_event_lines() {
    assert_eq!(
        extract_agent_event_json(
            r#"[stdout] LANTOR_EVENT {"type":"message","body":"from final output"}"#
        ),
        Some(r#"{"type":"message","body":"from final output"}"#)
    );
}

#[test]
fn extracts_agent_event_json_when_json_starts_on_next_line() {
    assert_eq!(
        extract_agent_event_json(
            "LANTOR_EVENT\n{\"type\":\"message\",\"body\":\"from next line\"}"
        ),
        Some(r#"{"type":"message","body":"from next line"}"#)
    );
}

#[test]
fn extracts_agent_event_json_before_trailing_text() {
    assert_eq!(
        extract_agent_event_json(
            r#"LANTOR_EVENT {"type":"activity","title":"Done","detail":"ok"} ## Review"#
        ),
        Some(r#"{"type":"activity","title":"Done","detail":"ok"}"#)
    );
}

#[test]
fn extracts_agent_event_json_with_braces_inside_strings() {
    assert_eq!(
        extract_agent_event_json(
            r#"LANTOR_EVENT {"type":"message","body":"text with { braces } and \"quotes\""} trailing"#
        ),
        Some(r#"{"type":"message","body":"text with { braces } and \"quotes\""}"#)
    );
}

#[test]
fn ignores_event_examples_embedded_in_instructions() {
    assert!(extract_agent_event_json(
        r#"[stderr] Reply with: LANTOR_EVENT {"type":"message","body":"..."}"#
    )
    .is_none());
}

#[tokio::test]
async fn usage_event_updates_run_tokens_and_cost() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "usage-agent").await?;
        let run_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_runs (agent_id, command, status)
            values ($1, 'codex app-server', 'running')
            returning id
            "#,
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        handle_agent_event(
            &pool,
            agent_id,
            run_id,
            AgentEvent::Usage {
                input_tokens: Some(1000),
                output_tokens: Some(200),
                total_tokens: None,
                cost_micros: Some(1234),
                cost_usd: None,
            },
        )
        .await?;
        let row = sqlx::query(
            "select input_tokens, output_tokens, cost_micros from agent_runs where id = $1",
        )
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(row.get::<i64, _>("input_tokens"), 1000);
        assert_eq!(row.get::<i64, _>("output_tokens"), 200);
        assert_eq!(row.get::<i64, _>("cost_micros"), 1234);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn memory_events_append_and_compact_memory_file() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "memory-agent").await?;
        let run_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_runs (agent_id, command, status)
            values ($1, 'codex app-server', 'running')
            returning id
            "#,
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let dir = std::env::temp_dir().join(format!("lantor-memory-write-{}", Uuid::new_v4()));
        sqlx::query("update agents set working_directory = $2 where id = $1")
            .bind(agent_id)
            .bind(dir.to_string_lossy().to_string())
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        handle_agent_event(
            &pool,
            agent_id,
            run_id,
            AgentEvent::MemoryAppend {
                body: "Remember: concise replies.".to_owned(),
            },
        )
        .await?;
        let memory_path = dir.join("MEMORY.md");
        let memory = std::fs::read_to_string(&memory_path).map_err(|err| err.to_string())?;
        let work_log_path = dir.join("notes").join("work-log.md");
        let work_log = std::fs::read_to_string(&work_log_path).map_err(|err| err.to_string())?;
        assert!(memory.contains("notes/work-log.md"));
        assert!(memory.contains("## Memory Map"));
        assert!(!memory.contains("## Memory update"));
        assert!(work_log.contains("## Memory update"));
        assert!(work_log.contains("Remember: concise replies."));
        handle_agent_event(
            &pool,
            agent_id,
            run_id,
            AgentEvent::MemoryCompact {
                body: "# @memory-agent\n\n## Role\nCompact memory.\n".to_owned(),
            },
        )
        .await?;
        let memory = std::fs::read_to_string(&memory_path).map_err(|err| err.to_string())?;
        assert_eq!(memory, "# @memory-agent\n\n## Role\nCompact memory.\n");
        let _ = std::fs::remove_dir_all(dir);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn channel_events_create_channel_and_invite_agents() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "creator-agent").await?;
        let reviewer_id = insert_test_agent(&pool, "reviewer-agent").await?;
        let run_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_runs (agent_id, command, status)
            values ($1, 'codex app-server', 'running')
            returning id
            "#,
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        handle_agent_event(
            &pool,
            agent_id,
            run_id,
            AgentEvent::ChannelCreate {
                name: "Feature Room".to_owned(),
                description: Some("long-lived feature coordination".to_owned()),
                agent_handles: Some(vec!["@reviewer-agent".to_owned()]),
            },
        )
        .await?;
        let channel_id: Uuid =
            sqlx::query_scalar("select id from channels where name = 'feature-room'")
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        let members: i64 = sqlx::query_scalar(
            r#"
            select count(*)
            from channel_members
            where channel_id = $1 and agent_id in ($2, $3)
            "#,
        )
        .bind(channel_id)
        .bind(agent_id)
        .bind(reviewer_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(members, 2);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn profile_update_event_updates_agent_profile() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "profile-agent").await?;
        let run_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_runs (agent_id, command, status)
            values ($1, 'codex app-server', 'running')
            returning id
            "#,
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;

        handle_agent_event(
            &pool,
            agent_id,
            run_id,
            AgentEvent::ProfileUpdate {
                display_name: Some("Profile Agent".to_owned()),
                role: Some("vision reviewer".to_owned()),
                avatar: Some("P".to_owned()),
                description: Some("Reviews screenshots and agent handoffs.".to_owned()),
            },
        )
        .await?;

        let row =
            sqlx::query("select display_name, role, avatar, description from agents where id = $1")
                .bind(agent_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(row.get::<String, _>("display_name"), "Profile Agent");
        assert_eq!(row.get::<String, _>("role"), "vision reviewer");
        assert_eq!(row.get::<String, _>("avatar"), "P");
        assert_eq!(
            row.get::<String, _>("description"),
            "Reviews screenshots and agent handoffs."
        );
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn task_status_and_claim_events_do_not_insert_system_messages() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "task-noise-agent").await?;
        let channel_id = insert_test_channel(&pool, "task-noise").await?;
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
        let message_id: Uuid = sqlx::query_scalar(
            r#"
            insert into messages (channel_id, sender_name, sender_role, body, is_task)
            values ($1, 'Dylan', 'owner', 'Track this task', true)
            returning id
            "#,
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let task_number: i64 = sqlx::query_scalar(
            r#"
            insert into tasks (message_id, channel_id, title, status)
            values ($1, $2, 'Track this task', 'todo')
            returning number
            "#,
        )
        .bind(message_id)
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let run_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_runs (agent_id, command, status)
            values ($1, 'codex app-server', 'running')
            returning id
            "#,
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;

        handle_agent_event(
            &pool,
            agent_id,
            run_id,
            AgentEvent::TaskClaim {
                task_number,
                assignee_handle: None,
            },
        )
        .await?;
        handle_agent_event(
            &pool,
            agent_id,
            run_id,
            AgentEvent::TaskStatus {
                task_number,
                status: "done".to_owned(),
            },
        )
        .await?;

        let task = sqlx::query("select status, assignee_agent_id from tasks where number = $1")
            .bind(task_number)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert_eq!(task.get::<String, _>("status"), "done");
        assert_eq!(
            task.get::<Option<Uuid>, _>("assignee_agent_id"),
            Some(agent_id)
        );

        let system_messages: i64 = sqlx::query_scalar(
            r#"
            select count(*)
            from messages
            where channel_id = $1
              and sender_role = 'system'
              and (body like '%claimed task%' or body like 'Task #%moved%')
            "#,
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(system_messages, 0);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn task_create_event_creates_root_task_and_execution_thread() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "task-thread-agent").await?;
        let channel_id = insert_test_channel(&pool, "task-thread-api").await?;
        let run_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_runs (agent_id, command, working_directory, status)
            values ($1, 'test', '', 'running')
            returning id
            "#,
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;

        handle_agent_event(
            &pool,
            agent_id,
            run_id,
            AgentEvent::TaskCreate {
                channel: None,
                channel_id: Some(channel_id),
                title: "Investigate task thread API".to_owned(),
                body: Some("Track this as a durable task".to_owned()),
                thread_body: Some("Starting execution in the task thread".to_owned()),
                assign_self: Some(true),
                status: Some("in_progress".to_owned()),
            },
        )
        .await?;

        let task_row = sqlx::query(
            r#"
            select t.number, t.status, t.assignee_agent_id, m.id as message_id, m.body, m.thread_root_id
            from tasks t
            join messages m on m.id = t.message_id
            where t.channel_id = $1
            "#,
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let root_message_id: Uuid = task_row.get("message_id");
        assert_eq!(task_row.get::<String, _>("status"), "in_progress");
        assert_eq!(
            task_row.get::<Option<Uuid>, _>("assignee_agent_id"),
            Some(agent_id)
        );
        assert_eq!(task_row.get::<String, _>("body"), "Track this as a durable task");
        assert_eq!(task_row.get::<Option<Uuid>, _>("thread_root_id"), None);

        let reply_body: String = sqlx::query_scalar(
            "select body from messages where channel_id = $1 and thread_root_id = $2",
        )
        .bind(channel_id)
        .bind(root_message_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(reply_body, "Starting execution in the task thread");

        let subscribed: bool = sqlx::query_scalar(
            r#"
            select exists (
                select 1
                from agent_thread_subscriptions
                where agent_id = $1 and channel_id = $2 and thread_root_id = $3
            )
            "#,
        )
        .bind(agent_id)
        .bind(channel_id)
        .bind(root_message_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert!(subscribed);
        assert!(task_row.get::<i64, _>("number") > 0);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn activity_event_records_progress_without_chat_message() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "activity-agent").await?;
        let run_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_runs (agent_id, command, working_directory, status)
            values ($1, 'test', '', 'running')
            returning id
            "#,
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;

        handle_agent_event(
            &pool,
            agent_id,
            run_id,
            AgentEvent::Activity {
                kind: Some("running_command".to_owned()),
                title: "Running tests".to_owned(),
                detail: Some("cargo test".to_owned()),
            },
        )
        .await?;

        let row = sqlx::query(
            r#"
            select kind, phase, status, title, detail
            from agent_activities
            where agent_id = $1 and run_id = $2
            "#,
        )
        .bind(agent_id)
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(row.get::<String, _>("kind"), "command");
        assert_eq!(row.get::<String, _>("phase"), "command");
        assert_eq!(row.get::<String, _>("status"), "active");
        assert_eq!(row.get::<String, _>("title"), "Running tests");
        assert_eq!(row.get::<String, _>("detail"), "cargo test");
        let messages: i64 = sqlx::query_scalar("select count(*) from messages")
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert_eq!(messages, 0);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn reminder_create_event_with_interval_minutes_creates_a_loop() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "loop-event-agent").await?;
        let channel_id = insert_test_channel(&pool, "loop-events").await?;
        sqlx::query("insert into channel_members (channel_id, agent_id) values ($1, $2)")
            .bind(channel_id)
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        let run_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_runs (agent_id, command, status)
            values ($1, 'claude', 'running')
            returning id
            "#,
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let line = format!(
            r#"LANTOR_EVENT {{"type":"reminder_create","channel_id":"{channel_id}","title":"Forward #github-pulls","interval_minutes":5,"check_command":"./poll.sh","check_action":"post"}}"#
        );
        let json = extract_agent_event_json(&line).ok_or("event line should parse")?;
        let event: AgentEvent = serde_json::from_str(json).map_err(|err| err.to_string())?;
        handle_agent_event(&pool, agent_id, run_id, event).await?;

        let row = sqlx::query(
            r#"
            select recurrence, interval_seconds, check_command, check_action, status,
                   abs(julianday(due_at) - julianday('now')) * 86400.0 as due_delta
            from reminders
            where creator_agent_id = $1
            "#,
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(row.get::<String, _>("recurrence"), "interval");
        assert_eq!(row.get::<Option<i64>, _>("interval_seconds"), Some(300));
        assert_eq!(row.get::<String, _>("check_command"), "./poll.sh");
        assert_eq!(row.get::<String, _>("check_action"), "post");
        assert_eq!(row.get::<String, _>("status"), "scheduled");
        assert!(row.get::<f64, _>("due_delta") < 10.0, "first tick should be due now");
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}
