use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};
use tower::ServiceExt;
use uuid::Uuid;

use super::{web_router, WebState};
use crate::{
    channels::load_channels,
    db::migrate,
    owner_inbox::mark_channel_read_in_pool,
    test_support::{drop_test_schema, insert_test_agent, insert_test_channel, test_pool},
};

pub(super) async fn legacy_unread(pool: &SqlitePool) -> BTreeMap<Uuid, i64> {
    sqlx::query(include_str!("fixtures/channel-unread-legacy.sql"))
        .fetch_all(pool)
        .await
        .unwrap()
        .into_iter()
        .map(|row| (row.get("id"), row.get("unread_count")))
        .collect()
}

async fn insert_message(
    pool: &SqlitePool,
    channel: Uuid,
    root: Option<Uuid>,
    body: &str,
    timestamp: &str,
) -> Uuid {
    sqlx::query_scalar("insert into messages (channel_id, thread_root_id, sender_name, sender_role, body, created_at) values ($1,$2,'fixture','agent',$3,$4) returning id")
        .bind(channel).bind(root).bind(body).bind(timestamp).fetch_one(pool).await.unwrap()
}

#[tokio::test]
async fn seq_read_migration_preserves_legacy_counts_and_filters() {
    let (pool, path) = test_pool().await.expect("fixture");
    let chronological = insert_test_channel(&pool, "chronological").await.unwrap();
    let imported = insert_test_channel(&pool, "imported").await.unwrap();
    for channel in [chronological, imported] {
        insert_message(&pool, channel, None, "read", "2026-01-01T08:00:00+08:00").await;
        insert_message(&pool, channel, None, "unread", "2026-01-01T00:00:02Z").await;
        // A later seq with an earlier timestamp cannot use one exact watermark.
        if channel == imported {
            insert_message(
                &pool,
                channel,
                None,
                "backdated read",
                "2026-01-01T00:00:00Z",
            )
            .await;
        }
        sqlx::query("insert into channel_read_state (channel_id,last_read_at) values ($1,'2026-01-01T00:00:01Z')")
            .bind(channel).execute(&pool).await.unwrap();
    }
    for (role, delivery, body, attachment) in [
        ("owner", "complete", "owner", false),
        ("agent", "streaming", "streaming", false),
        ("agent", "complete", "", false),
        ("system", "complete", "", false),
        ("agent", "complete", "", true),
    ] {
        let id = insert_message(&pool, chronological, None, body, "2026-01-01T00:00:03Z").await;
        sqlx::query(
            "update messages set sender_role=$2,delivery_state=$3,stream_key=$4 where id=$1",
        )
        .bind(id)
        .bind(role)
        .bind(delivery)
        .bind(format!("{}:stream", Uuid::new_v4()))
        .execute(&pool)
        .await
        .unwrap();
        if attachment {
            sqlx::query("insert into message_attachments (message_id,original_name,mime_type,size_bytes,storage_path) values ($1,'test.txt','text/plain',1,'/nonexistent')")
                .bind(id).execute(&pool).await.unwrap();
        }
    }
    let invalid = insert_test_channel(&pool, "invalid-time").await.unwrap();
    insert_message(&pool, invalid, None, "invalid timestamp", "not-a-date").await;
    sqlx::query("insert into channel_read_state (channel_id,last_read_at) values ($1,'2026-01-01T00:00:01Z')")
        .bind(invalid).execute(&pool).await.unwrap();
    let before = legacy_unread(&pool).await;
    assert_eq!(before[&chronological], 3);
    assert_eq!(before[&imported], 1);
    assert_eq!(before[&invalid], 0);
    migrate(&pool).await.unwrap();
    let after: BTreeMap<_, _> = load_channels(&pool)
        .await
        .unwrap()
        .into_iter()
        .map(|c| (c.id, i64::from(c.unread_count)))
        .collect();
    assert_eq!(before, after);
    let markers: Vec<Option<i64>> =
        sqlx::query_scalar("select last_read_seq from channel_read_state where channel_id=$1")
            .bind(imported)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        markers,
        vec![None],
        "irregular timestamps retain the exact legacy fallback"
    );
    for channel in [chronological, imported] {
        mark_channel_read_in_pool(&pool, channel).await.unwrap();
    }
    let marker: i64 =
        sqlx::query_scalar("select last_read_seq from channel_read_state where channel_id=$1")
            .bind(chronological)
            .fetch_one(&pool)
            .await
            .unwrap();
    let cursor = sqlx::query_scalar::<_, i64>("select coalesce(max(id),0) from ui_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    mark_channel_read_in_pool(&pool, chronological)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select coalesce(max(id),0) from ui_events")
            .fetch_one(&pool)
            .await
            .unwrap(),
        cursor,
        "duplicate read must not emit an event"
    );
    insert_message(
        &pool,
        imported,
        None,
        "other channel gap",
        "2026-01-01T00:00:04Z",
    )
    .await;
    insert_message(
        &pool,
        chronological,
        None,
        "new backdated arrival",
        "2025-12-31T00:00:00Z",
    )
    .await;
    let channels = load_channels(&pool).await.unwrap();
    assert_eq!(
        channels
            .iter()
            .find(|c| c.id == chronological)
            .unwrap()
            .unread_count,
        1
    );
    let latest: i64 = sqlx::query_scalar("select max(seq) from messages where channel_id=$1")
        .bind(chronological)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(latest - marker > 1, "global seq gaps are not unread counts");
    drop_test_schema(pool, path).await;
}

#[tokio::test]
async fn compact_bootstrap_defers_details_and_bounds_thread_previews() {
    let (pool, path) = test_pool().await.expect("fixture");
    let channel = insert_test_channel(&pool, "bootstrap-slim").await.unwrap();
    let agent = insert_test_agent(&pool, "slim-fixture").await.unwrap();
    let long = "large detail ".repeat(1000);
    sqlx::query(
        "update agents set launch_command=$2,environment_variables='FIXTURE=value' where id=$1",
    )
    .bind(agent)
    .bind(&long)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("insert into agent_work_items (agent_id,title,context) values ($1,'work',$2)")
        .bind(agent)
        .bind(&long)
        .execute(&pool)
        .await
        .unwrap();
    for _ in 0..6 {
        sqlx::query("insert into agent_activities (agent_id,kind,title,detail,metadata) values ($1,'thinking','activity',$2,$3)")
            .bind(agent).bind(&long).bind(json!({"input_tokens":12,"large":long}).to_string()).execute(&pool).await.unwrap();
    }
    let mut root = Uuid::nil();
    for i in 0..25 {
        root = insert_message(
            &pool,
            channel,
            None,
            &format!("root {i}"),
            &format!("2026-01-01T00:00:{i:02}Z"),
        )
        .await;
    }
    for i in 0..8 {
        insert_message(
            &pool,
            channel,
            Some(root),
            &format!("reply {i}"),
            &format!("2026-01-01T00:01:{i:02}Z"),
        )
        .await;
    }
    let app = web_router(
        Arc::new(WebState {
            pool: pool.clone(),
            db_url: "synthetic".into(),
        }),
        PathBuf::from("/nonexistent"),
    );
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/bootstrap?currentChannelOnly=true&channelId={channel}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    let messages = payload["messages"].as_array().unwrap();
    assert_eq!(
        messages
            .iter()
            .filter(|m| m["thread_root_id"].is_null())
            .count(),
        20
    );
    assert_eq!(
        messages
            .iter()
            .filter(|m| !m["thread_root_id"].is_null())
            .count(),
        2
    );
    assert_eq!(payload["agents"][0]["details_loaded"], false);
    assert_eq!(payload["agents"][0]["launch_command"], "");
    assert_eq!(payload["agents"][0]["environment_variables"], "");
    assert_eq!(payload["agents"][0]["workspace_entries"], json!([]));
    assert_eq!(payload["agent_work_items"][0]["context"], "");
    assert_eq!(payload["agent_activities"].as_array().unwrap().len(), 3);
    assert!(
        payload["agent_activities"][0]["detail"]
            .as_str()
            .unwrap()
            .len()
            <= 240
    );
    assert_eq!(
        payload["agent_activities"][0]["metadata"]["large"],
        Value::Null
    );
    assert_eq!(
        payload["agent_activities"][0]["metadata"]["input_tokens"],
        12
    );
    assert_eq!(payload["thread_activities"][0]["reply_count"], 8);
    for (command, args) in [
        ("load_agent_detail", json!({"agentId":agent})),
        ("load_thread_messages", json!({"threadRootId":root})),
        (
            "load_ui_state",
            json!({"scopes":["agents","agent_work_items"]}),
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/{command}"))
                    .header("content-type", "application/json")
                    .body(Body::from(args.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{command}");
        let value: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        match command {
            "load_agent_detail" => {
                assert_eq!(value["agent"]["launch_command"], long);
                assert_eq!(value["agent"]["details_loaded"], true);
                assert_eq!(value["agent_work_items"][0]["context"], long);
                assert_eq!(value["agent_activities"].as_array().unwrap().len(), 6);
            }
            "load_thread_messages" => assert_eq!(value.as_array().unwrap().len(), 9),
            _ => {
                assert_eq!(value["agents"][0]["launch_command"], long);
                assert_eq!(value["agent_work_items"][0]["context"], long);
            }
        }
    }
    drop_test_schema(pool, path).await;
}
