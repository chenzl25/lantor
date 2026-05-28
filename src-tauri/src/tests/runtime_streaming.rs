use super::{
    adopt_streaming_agent_message_key, append_streaming_agent_message,
    append_streaming_agent_message_deferred_completion, consume_streaming_agent_control_lines,
    delete_intermediate_run_messages, dispatch_streaming_agent_message_mentions,
    ensure_streaming_agent_message, finish_streaming_agent_message,
    finish_streaming_agent_message_deferred_mentions, maybe_hide_silent_streaming_reply,
    streaming_message_body_is_empty, STREAMING_MESSAGE_BODY_LIMIT,
};
use crate::domain::reminders::load_reminders;
use crate::events::control::handle_streaming_agent_event_json;
use crate::message_store::load_messages;
use crate::publish_guard::{bump_thread_version, current_thread_version};
use crate::runtime::process::{load_runtime_thread_id, upsert_runtime_thread_id};
use crate::test_support::{drop_test_schema, insert_test_agent, insert_test_channel, test_pool};
use chrono::{Duration as ChronoDuration, Utc};
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

async fn insert_publish_gate_work(
    pool: &sqlx::SqlitePool,
    agent_id: Uuid,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    base_thread_version: i64,
) -> Result<(Uuid, Uuid), String> {
    let inbox_item_id: Uuid = sqlx::query_scalar(
        r#"
        insert into agent_inbox_items (
            agent_id, channel_id, thread_root_id, kind, priority, state, title, payload
        )
        values ($1, $2, $3, 'mention', 80, 'processing', 'publish gate test', $4)
        returning id
        "#,
    )
    .bind(agent_id)
    .bind(channel_id)
    .bind(thread_root_id)
    .bind(json!({"base_thread_version": base_thread_version}).to_string())
    .fetch_one(pool)
    .await
    .map_err(|err| err.to_string())?;
    let work_item_id: Uuid = sqlx::query_scalar(
        r#"
        insert into agent_work_items (
            agent_id, channel_id, thread_root_id, inbox_item_id, source_kind, title, context, status
        )
        values ($1, $2, $3, $4, 'inbox_wake', 'publish gate test', 'context', 'running')
        returning id
        "#,
    )
    .bind(agent_id)
    .bind(channel_id)
    .bind(thread_root_id)
    .bind(inbox_item_id)
    .fetch_one(pool)
    .await
    .map_err(|err| err.to_string())?;
    sqlx::query("update agent_inbox_items set work_item_id = $2 where id = $1")
        .bind(inbox_item_id)
        .bind(work_item_id)
        .execute(pool)
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
    .fetch_one(pool)
    .await
    .map_err(|err| err.to_string())?;
    Ok((work_item_id, run_id))
}

#[tokio::test]
async fn streaming_agent_messages_append_and_finish() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "streamer").await?;
        let channel_id = insert_test_channel(&pool, "streaming").await?;
        let stream_key = "run-1:item-1";

        let message_id =
            append_streaming_agent_message(&pool, agent_id, channel_id, None, stream_key, "Hel")
                .await?;
        let same_message_id =
            append_streaming_agent_message(&pool, agent_id, channel_id, None, stream_key, "lo")
                .await?;
        assert_eq!(message_id, same_message_id);
        finish_streaming_agent_message(&pool, stream_key, "complete").await?;

        let messages = load_messages(&pool).await?;
        let message = messages
            .iter()
            .find(|message| message.id == message_id)
            .expect("streaming message should be visible in bootstrap payload");
        assert_eq!(message.body, "Hello");
        assert_eq!(message.delivery_state, "complete");
        assert_eq!(message.stream_key, stream_key);

        upsert_runtime_thread_id(&pool, agent_id, "codex", "thread-1", "idle").await?;
        assert_eq!(
            load_runtime_thread_id(&pool, agent_id, "codex").await?,
            Some("thread-1".to_owned())
        );
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn truncation_completion_bumps_thread_version() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "truncation-version-agent").await?;
        let channel_id = insert_test_channel(&pool, "truncation-version").await?;
        let stream_key = "truncation-version-stream";
        let body = "x".repeat(STREAMING_MESSAGE_BODY_LIMIT + 1024);

        append_streaming_agent_message(&pool, agent_id, channel_id, None, stream_key, &body)
            .await?;

        let version = current_thread_version(&pool, channel_id, None).await?;
        assert_eq!(version, 1);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn streaming_placeholder_is_reused_for_visible_reply() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "placeholder-agent").await?;
        let channel_id = insert_test_channel(&pool, "placeholder-channel").await?;
        let run_id = Uuid::new_v4();
        let pending_stream_key = format!("{run_id}:pending");
        let final_stream_key = format!("{run_id}:item-1");

        let placeholder_id =
            ensure_streaming_agent_message(&pool, agent_id, channel_id, None, &pending_stream_key)
                .await?;
        let messages = load_messages(&pool).await?;
        let placeholder = messages
            .iter()
            .find(|message| message.id == placeholder_id)
            .expect("placeholder should be visible in bootstrap payload");
        assert_eq!(placeholder.body, "");
        assert_eq!(placeholder.delivery_state, "streaming");
        assert_eq!(placeholder.stream_key, pending_stream_key);

        let adopted_id =
            adopt_streaming_agent_message_key(&pool, &pending_stream_key, &final_stream_key)
                .await?;
        assert_eq!(adopted_id, Some(placeholder_id));
        assert!(streaming_message_body_is_empty(&pool, &final_stream_key).await?);

        let message_id = append_streaming_agent_message(
            &pool,
            agent_id,
            channel_id,
            None,
            &final_stream_key,
            "Done",
        )
        .await?;
        assert_eq!(message_id, placeholder_id);
        finish_streaming_agent_message(&pool, &final_stream_key, "complete").await?;

        let messages = load_messages(&pool).await?;
        let message = messages
            .iter()
            .find(|message| message.id == placeholder_id)
            .expect("final reply should reuse placeholder message");
        assert_eq!(message.body, "Done");
        assert_eq!(message.delivery_state, "complete");
        assert_eq!(message.stream_key, final_stream_key);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn gate_blocks_before_placeholder_creation() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "held-placeholder-agent").await?;
        let channel_id = insert_test_channel(&pool, "held-placeholder-channel").await?;
        let (_work_item_id, run_id) =
            insert_publish_gate_work(&pool, agent_id, channel_id, None, 0).await?;
        bump_thread_version(&pool, channel_id, None).await?;
        let stream_key = format!("{run_id}:pending");

        let _buffer_id =
            ensure_streaming_agent_message(&pool, agent_id, channel_id, None, &stream_key).await?;

        let visible_rows: i64 = sqlx::query_scalar(
            "select count(*) from messages where stream_key = $1 and delivery_state in ('streaming', 'complete')",
        )
        .bind(&stream_key)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(visible_rows, 0);

        let interrupted: i64 = sqlx::query_scalar(
            "select count(*) from agent_inbox_items where agent_id = $1 and kind = 'interrupted_action' and state <> 'archived'",
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(interrupted, 1);
        let base_thread_version: i64 = sqlx::query_scalar(
            "select json_extract(payload, '$.base_thread_version') from agent_inbox_items where kind = 'interrupted_action' order by created_at desc limit 1",
        )
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(base_thread_version, 1);

        let status: String =
            sqlx::query_scalar("select status from agent_work_items where run_id is null limit 1")
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(status, "interrupted");
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn interrupted_action_revise_allows_revised_reply_on_same_stream_key() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "revise-held-agent").await?;
        let channel_id = insert_test_channel(&pool, "revise-held-channel").await?;
        let (_work_item_id, run_id) =
            insert_publish_gate_work(&pool, agent_id, channel_id, None, 0).await?;
        bump_thread_version(&pool, channel_id, None).await?;
        let stream_key = format!("{run_id}:claude-assistant");

        append_streaming_agent_message(&pool, agent_id, channel_id, None, &stream_key, "Old")
            .await?;
        crate::publish_guard::resolve_interrupted_action(
            &pool,
            agent_id,
            run_id,
            &stream_key,
            "revise",
        )
        .await?;

        let buffer_state: String =
            sqlx::query_scalar("select state from agent_output_buffers where stream_key = $1")
                .bind(&stream_key)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(buffer_state, "revised");
        let buffer_body: String =
            sqlx::query_scalar("select body from agent_output_buffers where stream_key = $1")
                .bind(&stream_key)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(buffer_body, "");

        let open_items: i64 = sqlx::query_scalar(
            "select count(*) from agent_inbox_items where kind = 'interrupted_action' and state <> 'archived'",
        )
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(open_items, 0);

        append_streaming_agent_message(
            &pool,
            agent_id,
            channel_id,
            None,
            &stream_key,
            "Revised answer",
        )
        .await?;
        finish_streaming_agent_message(&pool, &stream_key, "complete").await?;

        let messages = load_messages(&pool).await?;
        let message = messages
            .iter()
            .find(|message| message.stream_key == stream_key)
            .expect("revised reply should publish on the same stream key");
        assert_eq!(message.body, "Revised answer");
        assert_eq!(message.delivery_state, "complete");
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn control_only_interrupted_action_resolve_bypasses_reply_freshness_gate() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "control-resolve-agent").await?;
        let channel_id = insert_test_channel(&pool, "control-resolve-channel").await?;

        let (_held_work_item_id, held_run_id) =
            insert_publish_gate_work(&pool, agent_id, channel_id, None, 0).await?;
        bump_thread_version(&pool, channel_id, None).await?;
        let held_stream_key = format!("{held_run_id}:held-reply");

        append_streaming_agent_message(
            &pool,
            agent_id,
            channel_id,
            None,
            &held_stream_key,
            "stale visible reply",
        )
        .await?;

        let held_buffers: i64 = sqlx::query_scalar(
            "select count(*) from agent_output_buffers where stream_key = $1 and state = 'held'",
        )
        .bind(&held_stream_key)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(held_buffers, 1);

        let (_resolve_work_item_id, resolve_run_id) =
            insert_publish_gate_work(&pool, agent_id, channel_id, None, 1).await?;
        bump_thread_version(&pool, channel_id, None).await?;
        let resolve_stream_key = format!("{resolve_run_id}:resolve-control");
        let event = json!({
            "type": "interrupted_action_resolve",
            "stream_key": held_stream_key,
            "action": "yield"
        });
        let body = format!("LANTOR_EVENT {event}\n");

        append_streaming_agent_message(
            &pool,
            agent_id,
            channel_id,
            None,
            &resolve_stream_key,
            &body,
        )
        .await?;

        let held_state: String =
            sqlx::query_scalar("select state from agent_output_buffers where stream_key = $1")
                .bind(&held_stream_key)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(
            held_state, "yielded",
            "the control-only resolve event should execute instead of being held behind the reply gate"
        );

        let resolve_buffers: i64 =
            sqlx::query_scalar("select count(*) from agent_output_buffers where stream_key = $1")
                .bind(&resolve_stream_key)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(
            resolve_buffers, 0,
            "a control-only resolve event must not create a second held buffer"
        );

        let resolve_messages: i64 =
            sqlx::query_scalar("select count(*) from messages where stream_key = $1")
                .bind(&resolve_stream_key)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(
            resolve_messages, 0,
            "control-only output should not create a visible streaming message"
        );

        let active_interrupted_items: i64 = sqlx::query_scalar(
            "select count(*) from agent_inbox_items where kind = 'interrupted_action' and state <> 'archived'",
        )
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(active_interrupted_items, 0);

        let resolve_activity: i64 = sqlx::query_scalar(
            "select count(*) from agent_activities where run_id = $1 and title = 'Interrupted action resolved'",
        )
        .bind(resolve_run_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(resolve_activity, 1);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn control_only_activity_bypasses_reply_freshness_gate() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "control-activity-agent").await?;
        let channel_id = insert_test_channel(&pool, "control-activity-channel").await?;
        let (_work_item_id, run_id) =
            insert_publish_gate_work(&pool, agent_id, channel_id, None, 0).await?;
        bump_thread_version(&pool, channel_id, None).await?;
        let stream_key = format!("{run_id}:activity-control");
        let event = json!({
            "type": "activity",
            "kind": "thinking",
            "title": "Control-only activity",
            "detail": "should execute even when the reply surface is stale"
        });
        let body = format!("LANTOR_EVENT {event}\n");

        append_streaming_agent_message(&pool, agent_id, channel_id, None, &stream_key, &body)
            .await?;

        let activity_count: i64 = sqlx::query_scalar(
            "select count(*) from agent_activities where run_id = $1 and title = 'Control-only activity'",
        )
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(activity_count, 1);

        let buffers: i64 =
            sqlx::query_scalar("select count(*) from agent_output_buffers where stream_key = $1")
                .bind(&stream_key)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(
            buffers, 0,
            "non-visible control-only events should not be held as public replies"
        );

        let messages: i64 = sqlx::query_scalar("select count(*) from messages where stream_key = $1")
            .bind(&stream_key)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert_eq!(
            messages, 0,
            "non-visible control-only events should not create visible messages"
        );

        assert_eq!(
            current_thread_version(&pool, channel_id, None).await?,
            1,
            "executing non-visible control-only events must not bump thread freshness"
        );
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn streaming_placeholder_creation_does_not_bump_thread_version() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "placeholder-version-agent").await?;
        let channel_id = insert_test_channel(&pool, "placeholder-version-channel").await?;
        let stream_key = "placeholder-version-stream";

        ensure_streaming_agent_message(&pool, agent_id, channel_id, None, stream_key).await?;

        assert_eq!(
            current_thread_version(&pool, channel_id, None).await?,
            0,
            "streaming placeholder creation is framework state and must not advance publish freshness"
        );
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn system_message_does_not_bump_thread_version() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let channel_id = insert_test_channel(&pool, "system-version-channel").await?;

        crate::ui_notifications::insert_system_message(&pool, channel_id, None, "patrol reminder")
            .await?;

        assert_eq!(
            current_thread_version(&pool, channel_id, None).await?,
            0,
            "framework/system messages must not make an agent's active context stale"
        );
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn interrupted_action_is_redispatched_as_new_inbox_wake() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "redispatch-held-agent").await?;
        let channel_id = insert_test_channel(&pool, "redispatch-held-channel").await?;
        let (work_item_id, run_id) =
            insert_publish_gate_work(&pool, agent_id, channel_id, None, 0).await?;
        bump_thread_version(&pool, channel_id, None).await?;
        let stream_key = format!("{run_id}:claude-assistant");

        append_streaming_agent_message(&pool, agent_id, channel_id, None, &stream_key, "Held")
            .await?;

        let original_status: String =
            sqlx::query_scalar("select status from agent_work_items where id = $1")
                .bind(work_item_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(original_status, "interrupted");

        let (redispatch_work_item_id, inbox_state): (Uuid, String) = sqlx::query_as(
            r#"
            select work_item_id, state
            from agent_inbox_items
            where kind = 'interrupted_action'
              and json_extract(payload, '$.stream_key') = $1
              and state <> 'archived'
            "#,
        )
        .bind(&stream_key)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_ne!(
            redispatch_work_item_id, work_item_id,
            "a held output must be redispatched as a fresh inbox wake, not left attached to the interrupted work item"
        );
        assert_eq!(
            inbox_state, "processing",
            "the interrupted_action item should be claimed by the redispatch work item"
        );

        let start_commands: i64 = sqlx::query_scalar(
            r#"
            select count(*)
            from supervisor_commands
            where command_type = 'start_agent'
              and work_item_id = $1
              and status in ('pending', 'running')
            "#,
        )
        .bind(redispatch_work_item_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(
            start_commands, 1,
            "redispatched interrupted_action should wake the agent for re-decision"
        );
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn completed_reply_repins_base_version_before_followup_visible_event() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "self-publish-agent").await?;
        let channel_id = insert_test_channel(&pool, "self-publish-channel").await?;
        sqlx::query("insert into channel_members (channel_id, agent_id) values ($1, $2)")
            .bind(channel_id)
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        let (_work_item_id, run_id) =
            insert_publish_gate_work(&pool, agent_id, channel_id, None, 0).await?;
        let stream_key = format!("{run_id}:assistant-reply");

        append_streaming_agent_message(
            &pool,
            agent_id,
            channel_id,
            None,
            &stream_key,
            "Final answer",
        )
        .await?;
        finish_streaming_agent_message(&pool, &stream_key, "complete").await?;
        assert_eq!(
            current_thread_version(&pool, channel_id, None).await?,
            1,
            "completed visible reply should still advance thread freshness"
        );

        let event = json!({
            "type": "channel_message_create",
            "channel_id": channel_id,
            "body": "follow-up visible side effect"
        })
        .to_string();
        handle_streaming_agent_event_json(&pool, agent_id, run_id, &event).await?;

        let posted_side_effects: i64 = sqlx::query_scalar(
            "select count(*) from messages where channel_id = $1 and body = 'follow-up visible side effect'",
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(
            posted_side_effects, 1,
            "a run's own completed reply must not make its later visible output stale"
        );

        let interrupted_items: i64 = sqlx::query_scalar(
            "select count(*) from agent_inbox_items where kind = 'interrupted_action' and state <> 'archived'",
        )
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(interrupted_items, 0);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn held_visible_preserves_internal_control_events() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "held-internal-agent").await?;
        let channel_id = insert_test_channel(&pool, "held-internal-channel").await?;
        let (_work_item_id, run_id) =
            insert_publish_gate_work(&pool, agent_id, channel_id, None, 0).await?;
        bump_thread_version(&pool, channel_id, None).await?;
        let stream_key = format!("{run_id}:item-1");
        let body = "Visible text\nLANTOR_EVENT {\"type\":\"activity\",\"title\":\"Buffered internal\",\"detail\":\"kept\"}";

        append_streaming_agent_message(&pool, agent_id, channel_id, None, &stream_key, body)
            .await?;
        finish_streaming_agent_message(&pool, &stream_key, "complete").await?;

        let visible_rows: i64 = sqlx::query_scalar(
            "select count(*) from messages where stream_key = $1 and delivery_state in ('streaming', 'complete')",
        )
        .bind(&stream_key)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(visible_rows, 0);

        let activity_count: i64 = sqlx::query_scalar(
            "select count(*) from agent_activities where run_id = $1 and title = 'Buffered internal'",
        )
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(activity_count, 1);

        let draft_body: String = sqlx::query_scalar(
            "select json_extract(payload, '$.draft_body') from agent_inbox_items where kind = 'interrupted_action' order by created_at desc limit 1",
        )
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(draft_body, "Visible text");
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn held_visible_also_gates_visible_side_effects() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "held-side-effect-agent").await?;
        let channel_id = insert_test_channel(&pool, "held-side-effect-channel").await?;
        sqlx::query("insert into channel_members (channel_id, agent_id) values ($1, $2)")
            .bind(channel_id)
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        let (_work_item_id, run_id) =
            insert_publish_gate_work(&pool, agent_id, channel_id, None, 0).await?;
        bump_thread_version(&pool, channel_id, None).await?;
        let stream_key = format!("{run_id}:item-1");
        let event = json!({
            "type": "channel_message_create",
            "channel_id": channel_id,
            "body": "side effect body"
        });
        let body = format!("Visible text\nLANTOR_EVENT {event}");

        append_streaming_agent_message(&pool, agent_id, channel_id, None, &stream_key, &body)
            .await?;
        finish_streaming_agent_message(&pool, &stream_key, "complete").await?;

        let posted_side_effects: i64 =
            sqlx::query_scalar("select count(*) from messages where body = 'side effect body'")
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(posted_side_effects, 0);

        let held_events: String = sqlx::query_scalar(
            "select held_visible_events from agent_output_buffers where stream_key = $1",
        )
        .bind(&stream_key)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert!(held_events.contains("channel_message_create"));
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn streaming_event_only_buffer_advertises_side_effect_only_actions() {
    // Follow-up regression: when the streaming path produces a buffer whose
    // only content is held visible side effects (no draft reply body), the
    // interrupted_action payload must label itself `visible_control_event`
    // with `allowed_actions = ["yield", "force_send"]`, not `public_reply`
    // with revise. This mirrors `hold_visible_control_event` and prevents
    // payload/prompt vs. backend `revise`-guard drift in
    // `hold_streaming_public_output`.
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "stream-event-only-agent").await?;
        let channel_id = insert_test_channel(&pool, "stream-event-only-channel").await?;
        sqlx::query("insert into channel_members (channel_id, agent_id) values ($1, $2)")
            .bind(channel_id)
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        let (work_item_id, run_id) =
            insert_publish_gate_work(&pool, agent_id, channel_id, None, 0).await?;
        bump_thread_version(&pool, channel_id, None).await?;
        let stream_key = format!("{run_id}:event-only");
        let event = json!({
            "type": "channel_message_create",
            "channel_id": channel_id,
            "body": "event-only side effect"
        });
        // Streaming body contains ONLY a visible side-effect event line and
        // no surrounding prose; visible_body parses to empty.
        let body = format!("LANTOR_EVENT {event}");

        append_streaming_agent_message(&pool, agent_id, channel_id, None, &stream_key, &body)
            .await?;
        finish_streaming_agent_message(&pool, &stream_key, "complete").await?;

        let (buffer_body, held_events): (String, String) = sqlx::query_as(
            "select body, held_visible_events from agent_output_buffers where stream_key = $1",
        )
        .bind(&stream_key)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(
            buffer_body, "",
            "streaming event-only buffer should have empty body, got: {buffer_body:?}"
        );
        assert!(
            held_events.contains("event-only side effect"),
            "held_visible_events should carry the queued side effect: {held_events}"
        );

        let (interrupted_kind, allowed_actions): (String, String) = sqlx::query_as(
            "select json_extract(payload, '$.interrupted_action'), json_extract(payload, '$.allowed_actions') from agent_inbox_items where kind = 'interrupted_action' and json_extract(payload, '$.stream_key') = $1",
        )
        .bind(&stream_key)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(
            interrupted_kind, "visible_control_event",
            "streaming event-only buffer must advertise visible_control_event, not public_reply"
        );
        assert_eq!(
            allowed_actions, r#"["yield","force_send"]"#,
            "streaming event-only buffer must not advertise revise"
        );

        // Backend must also reject `revise` for this buffer (defense in depth
        // against any caller that ignores allowed_actions).
        let resolve_result = crate::publish_guard::resolve_interrupted_action(
            &pool,
            agent_id,
            run_id,
            &stream_key,
            "revise",
        )
        .await;
        assert!(
            resolve_result.is_err(),
            "revise on a streaming event-only buffer must be rejected, got: {resolve_result:?}"
        );

        // Buffer / work item / inbox item state stays intact after rejection.
        let (state_after, body_after, held_after): (String, String, String) = sqlx::query_as(
            "select state, body, held_visible_events from agent_output_buffers where stream_key = $1",
        )
        .bind(&stream_key)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(state_after, "held");
        assert_eq!(body_after, "");
        assert_eq!(held_after, held_events);
        let work_status: String =
            sqlx::query_scalar("select status from agent_work_items where id = $1")
                .bind(work_item_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(work_status, "interrupted");
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn held_visible_side_effects_use_source_surface_freshness() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "held-cross-surface-agent").await?;
        let source_channel_id = insert_test_channel(&pool, "source-surface").await?;
        let target_channel_id = insert_test_channel(&pool, "target-surface").await?;
        sqlx::query("insert into channel_members (channel_id, agent_id) values ($1, $2)")
            .bind(target_channel_id)
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        let (_work_item_id, run_id) =
            insert_publish_gate_work(&pool, agent_id, source_channel_id, None, 0).await?;
        bump_thread_version(&pool, source_channel_id, None).await?;
        let event = json!({
            "type": "channel_message_create",
            "channel_id": target_channel_id,
            "body": "cross surface side effect"
        })
        .to_string();

        handle_streaming_agent_event_json(&pool, agent_id, run_id, &event).await?;

        let posted_side_effects: i64 = sqlx::query_scalar(
            "select count(*) from messages where channel_id = $1 and body = 'cross surface side effect'",
        )
        .bind(target_channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(posted_side_effects, 0);

        let held_surface: (Uuid, Option<Uuid>) = sqlx::query_as(
            "select channel_id, thread_root_id from agent_output_buffers where run_id = $1",
        )
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(held_surface, (source_channel_id, None));
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn target_surface_change_does_not_hold_cross_surface_side_effect() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "allowed-cross-surface-agent").await?;
        let source_channel_id = insert_test_channel(&pool, "fresh-source-surface").await?;
        let target_channel_id = insert_test_channel(&pool, "changed-target-surface").await?;
        sqlx::query("insert into channel_members (channel_id, agent_id) values ($1, $2)")
            .bind(target_channel_id)
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        let (_work_item_id, run_id) =
            insert_publish_gate_work(&pool, agent_id, source_channel_id, None, 0).await?;
        bump_thread_version(&pool, target_channel_id, None).await?;
        let event = json!({
            "type": "channel_message_create",
            "channel_id": target_channel_id,
            "body": "allowed cross surface side effect"
        })
        .to_string();

        handle_streaming_agent_event_json(&pool, agent_id, run_id, &event).await?;

        let posted_side_effects: i64 = sqlx::query_scalar(
            "select count(*) from messages where channel_id = $1 and body = 'allowed cross surface side effect'",
        )
        .bind(target_channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(posted_side_effects, 1);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn repeated_held_visible_events_are_aggregated_into_one_interruption() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "aggregate-held-agent").await?;
        let channel_id = insert_test_channel(&pool, "aggregate-held-channel").await?;
        sqlx::query("insert into channel_members (channel_id, agent_id) values ($1, $2)")
            .bind(channel_id)
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        let (_work_item_id, run_id) =
            insert_publish_gate_work(&pool, agent_id, channel_id, None, 0).await?;
        bump_thread_version(&pool, channel_id, None).await?;
        for body in ["first held event", "second held event"] {
            let event = json!({
                "type": "channel_message_create",
                "channel_id": channel_id,
                "body": body
            })
            .to_string();
            handle_streaming_agent_event_json(&pool, agent_id, run_id, &event).await?;
        }

        let buffer_count: i64 =
            sqlx::query_scalar("select count(*) from agent_output_buffers where run_id = $1")
                .bind(run_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(buffer_count, 1);
        let item_count: i64 = sqlx::query_scalar(
            "select count(*) from agent_inbox_items where kind = 'interrupted_action'",
        )
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(item_count, 1);
        let held_events: String = sqlx::query_scalar(
            "select held_visible_events from agent_output_buffers where run_id = $1",
        )
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert!(held_events.contains("first held event"));
        assert!(held_events.contains("second held event"));
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn force_send_replays_held_visible_side_effects() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "force-side-effect-agent").await?;
        let channel_id = insert_test_channel(&pool, "force-side-effect-channel").await?;
        sqlx::query("insert into channel_members (channel_id, agent_id) values ($1, $2)")
            .bind(channel_id)
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        let (_work_item_id, run_id) =
            insert_publish_gate_work(&pool, agent_id, channel_id, None, 0).await?;
        bump_thread_version(&pool, channel_id, None).await?;
        let stream_key = format!("{run_id}:item-1");
        let event = json!({
            "type": "channel_message_create",
            "channel_id": channel_id,
            "body": "forced side effect"
        });
        let body = format!("Visible text\nLANTOR_EVENT {event}");

        append_streaming_agent_message(&pool, agent_id, channel_id, None, &stream_key, &body)
            .await?;
        finish_streaming_agent_message(&pool, &stream_key, "complete").await?;

        crate::publish_guard::resolve_interrupted_action(
            &pool,
            agent_id,
            run_id,
            &stream_key,
            "force_send",
        )
        .await?;

        let forced_visible_text: i64 =
            sqlx::query_scalar("select count(*) from messages where body = 'Visible text'")
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(forced_visible_text, 1);

        let forced_side_effects: i64 =
            sqlx::query_scalar("select count(*) from messages where body = 'forced side effect'")
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(forced_side_effects, 1);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn side_effect_only_buffer_rejects_revise_and_preserves_held_events() {
    // Regression for issue #96: a held buffer with no draft body must not be
    // resolved via `revise`, otherwise the resolve path would clear
    // `held_visible_events` and silently drop the queued side effect(s).
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "side-effect-only-agent").await?;
        let channel_id = insert_test_channel(&pool, "side-effect-only-channel").await?;
        sqlx::query("insert into channel_members (channel_id, agent_id) values ($1, $2)")
            .bind(channel_id)
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        let (work_item_id, run_id) =
            insert_publish_gate_work(&pool, agent_id, channel_id, None, 0).await?;
        // Make the work item's pinned base_thread_version stale relative to
        // the channel so the side-effect dispatch is held by the publish gate.
        bump_thread_version(&pool, channel_id, None).await?;

        let event = json!({
            "type": "channel_message_create",
            "channel_id": channel_id,
            "body": "side effect that must not be silently dropped"
        })
        .to_string();
        handle_streaming_agent_event_json(&pool, agent_id, run_id, &event).await?;

        let (stream_key, body, held_before): (String, String, String) = sqlx::query_as(
            "select stream_key, body, held_visible_events from agent_output_buffers where run_id = $1 and state = 'held'",
        )
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert!(body.is_empty(), "side-effect-only buffer should have empty body, got: {body:?}");
        assert!(
            held_before.contains("side effect that must not be silently dropped"),
            "held_visible_events should contain the queued side effect before resolve: {held_before}"
        );
        let interrupted_kind: String = sqlx::query_scalar(
            "select json_extract(payload, '$.interrupted_action') from agent_inbox_items where kind = 'interrupted_action' and json_extract(payload, '$.stream_key') = $1",
        )
        .bind(&stream_key)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(interrupted_kind, "visible_control_event");

        // Backend must reject `revise` for this side-effect-only buffer.
        let resolve_result = crate::publish_guard::resolve_interrupted_action(
            &pool,
            agent_id,
            run_id,
            &stream_key,
            "revise",
        )
        .await;
        assert!(
            resolve_result.is_err(),
            "revise on a side-effect-only buffer must be rejected, got: {resolve_result:?}"
        );
        let err_text = resolve_result.unwrap_err();
        assert!(
            err_text.contains("not allowed") && err_text.contains("visible_control_event"),
            "error message should explain the disallowed action: {err_text}"
        );

        // Buffer must remain held with its side effects intact.
        let (state_after, body_after, held_after): (String, String, String) = sqlx::query_as(
            "select state, body, held_visible_events from agent_output_buffers where stream_key = $1",
        )
        .bind(&stream_key)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(state_after, "held");
        assert!(body_after.is_empty());
        assert_eq!(held_after, held_before, "held_visible_events must be preserved verbatim");

        // Work item must stay interrupted (not flipped to 'done' / 'revised').
        let work_status: String =
            sqlx::query_scalar("select status from agent_work_items where id = $1")
                .bind(work_item_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(work_status, "interrupted");

        // The interrupted_action inbox item must still be open for re-decision.
        let open_items: i64 = sqlx::query_scalar(
            "select count(*) from agent_inbox_items where kind = 'interrupted_action' and json_extract(payload, '$.stream_key') = $1 and state <> 'archived'",
        )
        .bind(&stream_key)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(open_items, 1);

        // Sanity: `yield` is still accepted for this side-effect-only buffer.
        crate::publish_guard::resolve_interrupted_action(
            &pool,
            agent_id,
            run_id,
            &stream_key,
            "yield",
        )
        .await?;
        let final_state: String =
            sqlx::query_scalar("select state from agent_output_buffers where stream_key = $1")
                .bind(&stream_key)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(final_state, "yielded");
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn streaming_intermediate_reply_is_deleted_when_run_continues() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "draft-agent").await?;
        let channel_id = insert_test_channel(&pool, "draft-channel").await?;
        let run_id = Uuid::new_v4();
        let stream_key = format!("{run_id}:draft-message");

        let message_id = append_streaming_agent_message(
            &pool,
            agent_id,
            channel_id,
            None,
            &stream_key,
            "Need get migrations output.",
        )
        .await?;
        delete_intermediate_run_messages(&pool, agent_id, channel_id, None, run_id).await?;

        let remaining: i64 = sqlx::query_scalar("select count(*) from messages where id = $1")
            .bind(message_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert_eq!(remaining, 0);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn deleted_streaming_intermediate_reply_does_not_dispatch_mentions() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "draft-agent").await?;
        let target_agent_id = insert_test_agent(&pool, "target-agent").await?;
        let channel_id = insert_test_channel(&pool, "mention-draft-channel").await?;
        sqlx::query("insert into channel_members (channel_id, agent_id) values ($1, $2), ($1, $3)")
            .bind(channel_id)
            .bind(agent_id)
            .bind(target_agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        let run_id = Uuid::new_v4();
        let stream_key = format!("{run_id}:draft-message");

        append_streaming_agent_message(
            &pool,
            agent_id,
            channel_id,
            None,
            &stream_key,
            "Need @target-agent migrations output.",
        )
        .await?;

        delete_intermediate_run_messages(&pool, agent_id, channel_id, None, run_id).await?;

        let work_items: i64 =
            sqlx::query_scalar("select count(*) from agent_work_items where agent_id = $1")
                .bind(target_agent_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        let inbox_items: i64 =
            sqlx::query_scalar("select count(*) from agent_inbox_items where agent_id = $1")
                .bind(target_agent_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(work_items, 0);
        assert_eq!(inbox_items, 0);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn truncated_deferred_intermediate_reply_stays_streaming_until_deleted() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "long-draft-agent").await?;
        let target_agent_id = insert_test_agent(&pool, "long-target-agent").await?;
        let channel_id = insert_test_channel(&pool, "long-mention-draft-channel").await?;
        sqlx::query("insert into channel_members (channel_id, agent_id) values ($1, $2), ($1, $3)")
            .bind(channel_id)
            .bind(agent_id)
            .bind(target_agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        let run_id = Uuid::new_v4();
        let stream_key = format!("{run_id}:long-draft-message");
        let body = format!(
            "Need @long-target-agent migrations output. {}",
            "x".repeat(STREAMING_MESSAGE_BODY_LIMIT)
        );

        let message_id = append_streaming_agent_message_deferred_completion(
            &pool,
            agent_id,
            channel_id,
            None,
            &stream_key,
            &body,
        )
        .await?;

        let delivery_state: String =
            sqlx::query_scalar("select delivery_state from messages where id = $1")
                .bind(message_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(delivery_state, "streaming");

        delete_intermediate_run_messages(&pool, agent_id, channel_id, None, run_id).await?;

        let remaining: i64 = sqlx::query_scalar("select count(*) from messages where id = $1")
            .bind(message_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        let work_items: i64 =
            sqlx::query_scalar("select count(*) from agent_work_items where agent_id = $1")
                .bind(target_agent_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        let inbox_items: i64 =
            sqlx::query_scalar("select count(*) from agent_inbox_items where agent_id = $1")
                .bind(target_agent_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(remaining, 0);
        assert_eq!(work_items, 0);
        assert_eq!(inbox_items, 0);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn completed_deferred_final_reply_survives_later_turn_error() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "final-agent").await?;
        let target_agent_id = insert_test_agent(&pool, "final-target-agent").await?;
        let channel_id = insert_test_channel(&pool, "final-mention-channel").await?;
        sqlx::query("insert into channel_members (channel_id, agent_id) values ($1, $2), ($1, $3)")
            .bind(channel_id)
            .bind(agent_id)
            .bind(target_agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        let run_id = Uuid::new_v4();
        let stream_key = format!("{run_id}:final-message");

        let message_id = append_streaming_agent_message_deferred_completion(
            &pool,
            agent_id,
            channel_id,
            None,
            &stream_key,
            "Done, @final-target-agent should see this.",
        )
        .await?;
        finish_streaming_agent_message_deferred_mentions(&pool, &stream_key, "complete").await?;

        let work_items_before: i64 =
            sqlx::query_scalar("select count(*) from agent_work_items where agent_id = $1")
                .bind(target_agent_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(work_items_before, 0);

        finish_streaming_agent_message(&pool, &stream_key, "error").await?;
        dispatch_streaming_agent_message_mentions(&pool, &stream_key).await?;

        let delivery_state: String =
            sqlx::query_scalar("select delivery_state from messages where id = $1")
                .bind(message_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        let work_items_after: i64 =
            sqlx::query_scalar("select count(*) from agent_work_items where agent_id = $1")
                .bind(target_agent_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        let inbox_items_after: i64 =
            sqlx::query_scalar("select count(*) from agent_inbox_items where agent_id = $1")
                .bind(target_agent_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(delivery_state, "complete");
        assert_eq!(work_items_after, 1);
        assert_eq!(inbox_items_after, 1);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn activity_only_streaming_reply_keeps_status_message() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "activity-only-agent").await?;
        let channel_id = insert_test_channel(&pool, "activity-only-channel").await?;
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
        let stream_key = format!("{run_id}:item-activity");
        let event = json!({
            "type": "activity",
            "kind": "thinking",
            "title": "Checking source",
            "detail": "Tracing the code path"
        });

        let message_id = append_streaming_agent_message(
            &pool,
            agent_id,
            channel_id,
            None,
            &stream_key,
            &format!("LANTOR_EVENT {event}\n"),
        )
        .await?;
        finish_streaming_agent_message(&pool, &stream_key, "complete").await?;

        let row = sqlx::query("select body, delivery_state from messages where id = $1")
            .bind(message_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert_eq!(row.get::<String, _>("body"), "");
        assert_eq!(row.get::<String, _>("delivery_state"), "complete");

        let activity_count: i64 = sqlx::query_scalar(
            "select count(*) from agent_activities where run_id = $1 and title = 'Checking source'",
        )
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(activity_count, 1);

        let leaked_messages: i64 =
            sqlx::query_scalar("select count(*) from messages where body like '%LANTOR_EVENT%'")
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(leaked_messages, 0);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn streaming_control_event_split_after_marker_is_consumed() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "split-control-agent").await?;
        let channel_id = insert_test_channel(&pool, "split-control-channel").await?;
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
        let stream_key = format!("{run_id}:split-control");
        let event = json!({
            "type": "activity",
            "kind": "thinking",
            "title": "Split marker",
            "detail": "The marker and JSON arrived separately"
        });

        append_streaming_agent_message(
            &pool,
            agent_id,
            channel_id,
            None,
            &stream_key,
            "LANTOR_EVENT\n",
        )
        .await?;
        let message_id = append_streaming_agent_message(
            &pool,
            agent_id,
            channel_id,
            None,
            &stream_key,
            &event.to_string(),
        )
        .await?;
        finish_streaming_agent_message(&pool, &stream_key, "complete").await?;

        let body: String = sqlx::query_scalar("select body from messages where id = $1")
            .bind(message_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert_eq!(body, "");

        let activity_count: i64 = sqlx::query_scalar(
            "select count(*) from agent_activities where run_id = $1 and title = 'Split marker'",
        )
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(activity_count, 1);

        let leaked_messages: i64 =
            sqlx::query_scalar("select count(*) from messages where body like '%LANTOR_EVENT%'")
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(leaked_messages, 0);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn streaming_error_finish_drops_incomplete_control_fragment() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "partial-control-agent").await?;
        let channel_id = insert_test_channel(&pool, "partial-control-channel").await?;
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
        let stream_key = format!("{run_id}:partial-control");
        let message_id = append_streaming_agent_message(
            &pool,
            agent_id,
            channel_id,
            None,
            &stream_key,
            "Visible update\nLANTOR_EVENT {\"type\":\"activity\"",
        )
        .await?;

        finish_streaming_agent_message(&pool, &stream_key, "error").await?;

        let row = sqlx::query("select body, delivery_state from messages where id = $1")
            .bind(message_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert_eq!(row.get::<String, _>("body"), "Visible update");
        assert_eq!(row.get::<String, _>("delivery_state"), "error");

        let leaked_messages: i64 =
            sqlx::query_scalar("select count(*) from messages where body like '%LANTOR_EVENT%'")
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(leaked_messages, 0);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn visible_reply_replaces_prior_activity_only_status_message() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "visible-after-progress-agent").await?;
        let channel_id = insert_test_channel(&pool, "visible-after-progress-channel").await?;
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
        let progress_stream_key = format!("{run_id}:item-progress");
        let final_stream_key = format!("{run_id}:item-final");
        let event = json!({
            "type": "activity",
            "kind": "thinking",
            "title": "Checking source",
            "detail": "Tracing the code path"
        });

        let progress_message_id = append_streaming_agent_message(
            &pool,
            agent_id,
            channel_id,
            None,
            &progress_stream_key,
            &format!("LANTOR_EVENT {event}\n"),
        )
        .await?;
        finish_streaming_agent_message(&pool, &progress_stream_key, "complete").await?;

        let final_message_id = append_streaming_agent_message(
            &pool,
            agent_id,
            channel_id,
            None,
            &final_stream_key,
            "Done",
        )
        .await?;
        finish_streaming_agent_message(&pool, &final_stream_key, "complete").await?;

        assert_ne!(progress_message_id, final_message_id);
        let progress_message_count: i64 =
            sqlx::query_scalar("select count(*) from messages where id = $1")
                .bind(progress_message_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(progress_message_count, 0);

        let messages = load_messages(&pool).await?;
        let final_message = messages
            .iter()
            .find(|message| message.id == final_message_id)
            .expect("final reply should remain visible");
        assert_eq!(final_message.body, "Done");
        assert_eq!(final_message.delivery_state, "complete");
        assert_eq!(final_message.stream_key, final_stream_key);
        assert_eq!(
            messages
                .iter()
                .filter(|message| message.stream_key.starts_with(&run_id.to_string()))
                .count(),
            1
        );
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn multiline_control_event_is_consumed_during_streaming() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "multiline-stream-control").await?;
        let channel_id = insert_test_channel(&pool, "multiline-stream-channel").await?;
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
        let stream_key = format!("{run_id}:item-multiline");
        let event = json!({
            "type": "activity",
            "kind": "thinking",
            "title": "Streaming control",
            "detail": "json starts on the next line"
        });

        let message_id = append_streaming_agent_message(
            &pool,
            agent_id,
            channel_id,
            None,
            &stream_key,
            &format!("LANTOR_EVENT\n{event}\n"),
        )
        .await?;

        let body: String = sqlx::query_scalar("select body from messages where id = $1")
            .bind(message_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert_eq!(body, "");

        let activity_count: i64 = sqlx::query_scalar(
            "select count(*) from agent_activities where run_id = $1 and title = 'Streaming control'",
        )
        .bind(run_id)
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
async fn failed_streaming_message_strips_incomplete_control_tail() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "failed-stream-control").await?;
        let channel_id = insert_test_channel(&pool, "failed-stream-channel").await?;
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
        let stream_key = format!("{run_id}:item-error");

        let message_id = append_streaming_agent_message(
            &pool,
            agent_id,
            channel_id,
            None,
            &stream_key,
            "Working patch.\nLANTOR_EVENT\n{\"type\":\"activity\",\"title\":\"Step\"",
        )
        .await?;
        finish_streaming_agent_message(&pool, &stream_key, "error").await?;

        let row = sqlx::query("select body, delivery_state from messages where id = $1")
            .bind(message_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert_eq!(row.get::<String, _>("body"), "Working patch.");
        assert_eq!(row.get::<String, _>("delivery_state"), "error");

        let leaked_messages: i64 =
            sqlx::query_scalar("select count(*) from messages where body like '%LANTOR_EVENT%'")
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(leaked_messages, 0);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn silent_streaming_reply_hides_message_and_marks_work_item() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "silent-agent").await?;
        let channel_id = insert_test_channel(&pool, "silent-channel").await?;
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
        let work_item_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_work_items (agent_id, channel_id, title, context, status, run_id)
            values ($1, $2, 'hello', 'hi', 'running', $3)
            returning id
            "#,
        )
        .bind(agent_id)
        .bind(channel_id)
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let inbox_item_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_inbox_items (
                agent_id, channel_id, kind, state, title, body_preview, work_item_id
            )
            values ($1, $2, 'dm', 'processing', 'hello', 'hi', $3)
            returning id
            "#,
        )
        .bind(agent_id)
        .bind(channel_id)
        .bind(work_item_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let stream_key = "silent-run:item-1";
        let message_id = append_streaming_agent_message(
            &pool,
            agent_id,
            channel_id,
            None,
            stream_key,
            "LANTOR_SILENT_REPLY: greeting only",
        )
        .await?;
        assert!(
            maybe_hide_silent_streaming_reply(
                &pool,
                agent_id,
                run_id,
                Some(work_item_id),
                stream_key,
            )
            .await?
        );

        let remaining: i64 = sqlx::query_scalar("select count(*) from messages where id = $1")
            .bind(message_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert_eq!(remaining, 0);
        let status: String =
            sqlx::query_scalar("select status from agent_work_items where id = $1")
                .bind(work_item_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(status, "silent");
        let inbox_state: String =
            sqlx::query_scalar("select state from agent_inbox_items where id = $1")
                .bind(inbox_item_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(inbox_state, "archived");
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn streaming_reminder_control_line_is_consumed() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "reminder-agent").await?;
        let channel_id = insert_test_channel(&pool, "reminder-control").await?;
        let root_id: Uuid = sqlx::query_scalar(
            r#"
            insert into messages (channel_id, sender_name, sender_role, body, is_task)
            values ($1, 'Dylan', 'owner', 'remind me later', false)
            returning id
            "#,
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let work_item_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_work_items (
                agent_id, channel_id, thread_root_id, source_message_id, title, context, status
            )
            values ($1, $2, $3, $3, 'set reminder', 'remind me later', 'running')
            returning id
            "#,
        )
        .bind(agent_id)
        .bind(channel_id)
        .bind(root_id)
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
        let due_at = (Utc::now() + ChronoDuration::minutes(30)).to_rfc3339();
        let event = json!({
            "type": "reminder_create",
            "when": due_at,
            "title": "Check PR",
            "note": "Look at CI"
        });
        let stream_key = "reminder-run:item-1";
        let body = format!("I'll remind you.\nLANTOR_EVENT {event}");
        let message_id = append_streaming_agent_message(
            &pool,
            agent_id,
            channel_id,
            Some(root_id),
            stream_key,
            &body,
        )
        .await?;

        let hidden = consume_streaming_agent_control_lines(
            &pool,
            agent_id,
            run_id,
            Some(work_item_id),
            stream_key,
        )
        .await?;
        assert!(!hidden);

        let visible_body: String = sqlx::query_scalar("select body from messages where id = $1")
            .bind(message_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert_eq!(visible_body, "I'll remind you.");

        let reminders = load_reminders(&pool).await?;
        assert_eq!(reminders.len(), 1);
        assert_eq!(reminders[0].title, "Check PR");
        assert_eq!(reminders[0].note, "Look at CI");
        assert_eq!(reminders[0].creator_agent_id, Some(agent_id));
        assert_eq!(reminders[0].channel_id, Some(channel_id));
        assert_eq!(reminders[0].thread_root_id, Some(root_id));
        assert_eq!(reminders[0].message_id, Some(root_id));
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn streaming_artifact_control_line_is_consumed_and_hidden() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "artifact-stream-agent").await?;
        let channel_id = insert_test_channel(&pool, "artifact-stream-control").await?;
        let root_id: Uuid = sqlx::query_scalar(
            r#"
            insert into messages (channel_id, sender_name, sender_role, body, is_task)
            values ($1, 'Dylan', 'owner', 'make an architecture artifact', false)
            returning id
            "#,
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let work_item_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_work_items (
                agent_id, channel_id, thread_root_id, source_message_id, title, context, status
            )
            values ($1, $2, $3, $3, 'create artifact', 'make an architecture artifact', 'running')
            returning id
            "#,
        )
        .bind(agent_id)
        .bind(channel_id)
        .bind(root_id)
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
        let event = json!({
            "type": "artifact_create",
            "channel_id": channel_id,
            "thread_root_id": root_id,
            "kind": "markdown",
            "title": "Architecture report",
            "summary": "Markdown architecture summary.",
            "content": "# Architecture\n\n- UI\n- Backend\n- Postgres"
        });
        let stream_key = "artifact-run:item-1";
        let raw_control_message_id = append_streaming_agent_message(
            &pool,
            agent_id,
            channel_id,
            Some(root_id),
            stream_key,
            &format!("LANTOR_EVENT {event}"),
        )
        .await?;

        let hidden = consume_streaming_agent_control_lines(
            &pool,
            agent_id,
            run_id,
            Some(work_item_id),
            stream_key,
        )
        .await?;
        assert!(hidden);

        let raw_remaining: i64 = sqlx::query_scalar("select count(*) from messages where id = $1")
            .bind(raw_control_message_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert_eq!(raw_remaining, 0);

        let artifact = sqlx::query(
            r#"
            select kind, title, content
            from artifacts
            where channel_id = $1 and thread_root_id = $2
            "#,
        )
        .bind(channel_id)
        .bind(root_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(artifact.get::<String, _>("kind"), "markdown");
        assert_eq!(artifact.get::<String, _>("title"), "Architecture report");
        assert!(artifact
            .get::<String, _>("content")
            .contains("# Architecture"));

        let visible_messages = load_messages(&pool).await?;
        assert!(!visible_messages
            .iter()
            .any(|message| message.body.contains("LANTOR_EVENT")));
        assert!(visible_messages.iter().any(|message| message
            .body
            .contains("Created artifact: Architecture report")));
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn streaming_finish_consumes_channel_create_control_line() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "creator-agent").await?;
        let reviewer_id = insert_test_agent(&pool, "Hancock").await?;
        let source_channel_id = insert_test_channel(&pool, "source-channel").await?;
        let run_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_runs (agent_id, command, status)
            values ($1, 'claude stream-json', 'running')
            returning id
            "#,
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let event = json!({
            "type": "channel_create",
            "name": "lantor-ui-design",
            "description": "讨论 Lantor UI 设计后续工作",
            "agent_handles": ["hancock"]
        });
        let stream_key = format!("{run_id}:claude-assistant");
        let message_id = append_streaming_agent_message(
            &pool,
            agent_id,
            source_channel_id,
            None,
            &stream_key,
            &format!("好的，我来创建。\n\nLANTOR_EVENT {event}"),
        )
        .await?;

        finish_streaming_agent_message(&pool, &stream_key, "complete").await?;

        let visible_body: String = sqlx::query_scalar("select body from messages where id = $1")
            .bind(message_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert_eq!(visible_body, "好的，我来创建。");

        let channel_id: Uuid =
            sqlx::query_scalar("select id from channels where name = 'lantor-ui-design'")
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        let member_count: i64 = sqlx::query_scalar(
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
        assert_eq!(member_count, 2);

        let leaked_messages: i64 =
            sqlx::query_scalar("select count(*) from messages where body like '%LANTOR_EVENT%'")
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(leaked_messages, 0);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn streaming_unsupported_artifact_control_line_keeps_status_message() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "unsupported-artifact-agent").await?;
        let channel_id = insert_test_channel(&pool, "unsupported-artifact-control").await?;
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
        let event = json!({
            "type": "artifact_create",
            "channel_id": channel_id,
            "kind": "html",
            "title": "Unsupported HTML",
            "content": "<main>not supported</main>"
        });
        let stream_key = "unsupported-artifact-run:item-1";
        let raw_control_message_id = append_streaming_agent_message(
            &pool,
            agent_id,
            channel_id,
            None,
            stream_key,
            &format!("LANTOR_EVENT {event}"),
        )
        .await?;

        let hidden =
            consume_streaming_agent_control_lines(&pool, agent_id, run_id, None, stream_key)
                .await?;
        assert!(!hidden);

        let raw_remaining: i64 = sqlx::query_scalar("select count(*) from messages where id = $1")
            .bind(raw_control_message_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert_eq!(raw_remaining, 1);

        finish_streaming_agent_message(&pool, stream_key, "complete").await?;
        let raw_body: String = sqlx::query_scalar("select body from messages where id = $1")
            .bind(raw_control_message_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert_eq!(raw_body, "");

        let artifact_count: i64 =
            sqlx::query_scalar("select count(*) from artifacts where channel_id = $1")
                .bind(channel_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(artifact_count, 0);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}
