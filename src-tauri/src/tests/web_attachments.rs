use crate::test_support::{drop_test_schema, insert_test_channel, test_pool};
use axum::{
    body::{Body, Bytes},
    http::{header, Request, StatusCode},
    response::Html,
    routing::get,
};
use std::{path::PathBuf, sync::Arc};
use uuid::Uuid;

#[tokio::test]
async fn oversized_content_length_is_rejected_without_polling_body() {
    let state = Arc::new(super::WebState {
        pool: sqlx::SqlitePool::connect_lazy("sqlite::memory:").unwrap(),
        db_url: "sqlite::memory:".to_owned(),
    });
    let body = Body::from_stream(async_stream::stream! {
        panic!("oversized Content-Length must be rejected before reading the body");
        #[allow(unreachable_code)]
        { yield Ok::<_, std::io::Error>(Bytes::new()); }
    });
    let request = Request::post("/api/send_message")
        .header(header::CONTENT_TYPE, "multipart/form-data; boundary=test")
        .header(header::CONTENT_LENGTH, 100 * 1024 * 1024)
        .body(body)
        .unwrap();
    assert_eq!(
        super::extract_send_message_request(request, &state)
            .await
            .unwrap_err()
            .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real Chromium/cache + streamed 100MiB upload RSS; run npm run test:attachments"]
async fn browser_cache_range_and_upload_memory() {
    let (pool, database) = test_pool().await.expect("isolated fixture");
    let root = std::env::temp_dir().join(format!("lantor-attachments-e2e-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::env::set_var("LANTOR_ATTACHMENT_DIR", &root);
    let channel = insert_test_channel(&pool, "attachments-e2e").await.unwrap();
    let app = super::web_router(
        Arc::new(super::WebState {
            pool: pool.clone(),
            db_url: format!("sqlite://{database}"),
        }),
        PathBuf::from("/nonexistent"),
    )
    .route(
        "/__test__/blank",
        get(|| async { Html("<!doctype html><title>Attachment fixture</title>") }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let output = tokio::process::Command::new("node")
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tests/attachments.e2e.mjs"))
        .arg(format!("http://{address}"))
        .arg(std::process::id().to_string())
        .arg(channel.to_string())
        .arg(&root)
        .output()
        .await
        .unwrap();
    println!("{}", String::from_utf8_lossy(&output.stdout));
    server.abort();
    drop_test_schema(pool, database).await;
    std::fs::remove_dir_all(root).unwrap();
    assert!(
        output.status.success(),
        "attachment integration failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
