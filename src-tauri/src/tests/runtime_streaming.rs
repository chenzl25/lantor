use super::{
    adopt_streaming_agent_message_key, append_streaming_agent_message,
    append_streaming_agent_message_deferred_completion, consume_streaming_agent_control_lines,
    delete_intermediate_run_messages, dispatch_streaming_agent_message_mentions,
    ensure_streaming_agent_message, finish_streaming_agent_message,
    finish_streaming_agent_message_deferred_mentions, maybe_hide_silent_streaming_reply,
    streaming_message_body_is_empty, STREAMING_MESSAGE_BODY_LIMIT,
};
use crate::domain::reminders::load_reminders;
use crate::message_store::load_messages;
use crate::runtime::process::{load_runtime_thread_id, upsert_runtime_thread_id};
use crate::test_support::{drop_test_schema, insert_test_agent, insert_test_channel, test_pool};
use chrono::{Duration as ChronoDuration, Utc};
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

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
