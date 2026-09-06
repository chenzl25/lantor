use super::{web_router, WebState};
use crate::test_support::{drop_test_schema, insert_test_channel, test_pool};
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc};
use tower::ServiceExt;

async fn post(app: &Router, command: &str, args: Value) -> Value {
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
    serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
}

#[tokio::test]
async fn web_mutations_replay_entities_and_read_only_affected_state() {
    let (pool, path) = test_pool().await.expect("SQLite fixture");
    let channel = insert_test_channel(&pool, "sync-test").await.unwrap();
    let app = web_router(
        Arc::new(WebState {
            pool: pool.clone(),
            db_url: "synthetic".into(),
        }),
        PathBuf::from("/nonexistent"),
    );
    let message = post(
        &app,
        "send_message",
        json!({"channelId": channel, "body": "Task body", "asTask": true}),
    )
    .await;
    let replay = post(&app, "replay_ui_events", json!({"cursor":0})).await;
    let events: Vec<Value> = replay["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|delivery| serde_json::from_str(delivery["event"].as_str().unwrap()).unwrap())
        .collect();
    assert!(events
        .iter()
        .any(|event| event["type"] == "message_upsert" && event["message"]["id"] == message["id"]));
    assert!(!events
        .iter()
        .any(|event| event["type"] == "refresh" && event["reason"] == "message"));
    let unchanged = post(
        &app,
        "replay_ui_events",
        json!({"cursor": replay["cursor"]}),
    )
    .await;
    assert_eq!(unchanged["events"], json!([]));
    let patch = post(&app, "load_ui_state", json!({"scopes":["tasks"]})).await;
    assert_eq!(patch.as_object().unwrap().len(), 1);
    let task = &patch["tasks"][0];
    post(
        &app,
        "update_task_status",
        json!({"taskId":task["id"], "status":"done"}),
    )
    .await;
    post(
        &app,
        "update_task_title",
        json!({"taskId":task["id"], "title":"Renamed task"}),
    )
    .await;
    post(&app, "mark_channel_read", json!({"channelId":channel})).await;
    let patch = post(
        &app,
        "load_ui_state",
        json!({"scopes":["tasks","channels"]}),
    )
    .await;
    assert_eq!(patch.as_object().unwrap().len(), 2);
    assert_eq!(patch["tasks"][0]["status"], "done");
    assert_eq!(patch["channels"][0]["unread_count"], 0);
    let replay = post(
        &app,
        "replay_ui_events",
        json!({"cursor": replay["cursor"]}),
    )
    .await;
    let events: Vec<Value> = replay["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|delivery| serde_json::from_str(delivery["event"].as_str().unwrap()).unwrap())
        .collect();
    assert!(events.iter().any(
        |event| event["type"] == "message_upsert" && event["message"]["body"] == "Renamed task"
    ));
    assert!(events
        .iter()
        .any(|event| event["reason"] == "task_status_updated"));
    assert!(events.iter().any(|event| event["reason"] == "channel_read"));
    let gap = post(&app, "replay_ui_events", json!({"cursor": 999999})).await;
    assert_eq!(gap["replayGap"], true);
    assert_eq!(gap["events"], json!([]));
    assert!(
        crate::ui_state::load_ui_state_in_pool(&pool, vec!["messages".into()])
            .await
            .is_err()
    );
    drop_test_schema(pool, path).await;
}
