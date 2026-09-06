use crate::{
    db::db_connect_with_url,
    runtime::streaming::append_streaming_agent_message,
    test_support::{drop_test_schema, insert_test_agent, insert_test_channel, test_pool},
    ui_event_hub::UiEventSubscription,
};
use axum::{
    response::Html,
    routing::{get, post},
    Json,
};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

#[tokio::test]
#[ignore = "fresh supervisor-write-path child used by the browser integration test"]
async fn cross_process_writer() {
    let Ok(url) = std::env::var("LANTOR_SSE_TEST_DATABASE") else {
        return;
    };
    let pool = db_connect_with_url(&url, 1).await.unwrap();
    let agent = Uuid::parse_str(&std::env::var("LANTOR_SSE_TEST_AGENT").unwrap()).unwrap();
    let channel = Uuid::parse_str(&std::env::var("LANTOR_SSE_TEST_CHANNEL").unwrap()).unwrap();
    for index in 0..25 {
        let delta=json!({"sample":index,"sent_at_ms":SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis()}).to_string();
        // This is the same transaction + outbox writer called by the separately
        // running supervisor, with no app-process notification helper involved.
        append_streaming_agent_message(&pool, agent, channel, None, "task105-benchmark", &delta)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(65)).await;
    }
    pool.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real Chromium + fresh cross-process writer; run with npm run test:sse-push"]
async fn browser_cross_process_delivery_and_query_scaling() {
    let (pool, path) = test_pool().await.expect("isolated fixture");
    let channel = insert_test_channel(&pool, "sse-push").await.unwrap();
    let agent = insert_test_agent(&pool, "sse-push").await.unwrap();
    append_streaming_agent_message(&pool, agent, channel, None, "task105-benchmark", "start")
        .await
        .unwrap();
    // Exercise endpoint bounds against a realistic retained outbox, not an
    // almost-empty event table. None of these synthetic rows is replayed.
    sqlx::query("with recursive history(n) as (select 1 union all select n+1 from history where n<5000) insert into ui_events (event_json) select $1 from history")
        .bind(json!({"type":"refresh","reason":"retained-history","padding":"x".repeat(2048)}).to_string())
        .execute(&pool).await.unwrap();
    let observer = Arc::new(UiEventSubscription::connect(&pool, None).await.unwrap());
    let metrics = observer.clone();
    let writer_url = format!("sqlite://{path}");
    let prune_pool = pool.clone();
    let app=super::web_router(Arc::new(super::WebState {pool:pool.clone(),db_url:writer_url.clone()}),PathBuf::from("/nonexistent"))
        .route("/__test__/blank",get(|| async {Html("<!doctype html><title>SSE fixture</title>")}))
        .route("/__test__/metrics",get(move || {let metrics=metrics.clone(); async move {
            let (pragma_reads,event_reads)=metrics.observer_counts();
            Json(json!({"pragma_reads":pragma_reads,"event_read_batches":event_reads,"event_table_queries":event_reads*2}))
        }}))
        .route("/__test__/write",post(move || {let url=writer_url.clone(); async move {
            let output=tokio::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact","web::sse_push_tests::cross_process_writer","--ignored","--nocapture"])
                .env("LANTOR_SSE_TEST_DATABASE",url).env("LANTOR_SSE_TEST_AGENT",agent.to_string())
                .env("LANTOR_SSE_TEST_CHANNEL",channel.to_string()).output().await.unwrap();
            assert!(output.status.success(),"fresh writer failed: {}",String::from_utf8_lossy(&output.stderr));
            Json(json!({"written":25,"writer_process":"fresh test process using runtime::streaming"}))
        }}))
        .route("/__test__/prune",post(move || {let pool=prune_pool.clone(); async move {
            sqlx::query("delete from ui_events where id < (select max(id) from ui_events)").execute(&pool).await.unwrap();
            Json(json!({"ok":true}))
        }}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let output = tokio::process::Command::new("node")
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tests/sse-push.e2e.mjs"))
        .arg(format!("http://{address}"))
        .output()
        .await
        .unwrap();
    println!("{}", String::from_utf8_lossy(&output.stdout));
    server.abort();
    drop(observer);
    drop_test_schema(pool, path).await;
    assert!(
        output.status.success(),
        "browser integration failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
