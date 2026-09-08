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
async fn thumbnail_route_keeps_original_and_checks_ownership_before_cache() {
    use tower::ServiceExt;
    let (pool, database) = test_pool().await.expect("isolated database");
    let channel = insert_test_channel(&pool, "thumbnail-route").await.unwrap();
    let message: Uuid = sqlx::query_scalar("insert into messages(channel_id,sender_name,sender_role,body) values($1,'owner','owner','image') returning id")
        .bind(channel).fetch_one(&pool).await.unwrap();
    let id = Uuid::new_v4();
    let path = std::env::temp_dir().join(format!("lantor-thumbnail-route-{id}.png"));
    image::RgbaImage::new(800, 600).save(&path).unwrap();
    sqlx::query("insert into message_attachments(id,message_id,original_name,mime_type,size_bytes,storage_path) values($1,$2,'fixture.png','image/png',$3,$4)")
        .bind(id).bind(message).bind(path.metadata().unwrap().len() as i64).bind(path.to_str().unwrap())
        .execute(&pool).await.unwrap();
    let app = super::web_router(
        Arc::new(super::WebState {
            pool: pool.clone(),
            db_url: format!("sqlite://{database}"),
        }),
        PathBuf::from("/nonexistent"),
    );
    let original = app
        .clone()
        .oneshot(
            Request::get(format!("/api/attachments/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(original.status(), StatusCode::OK);
    assert_eq!(original.headers()[header::CONTENT_TYPE], "image/png");
    let thumbnail = app
        .clone()
        .oneshot(
            Request::get(format!("/api/attachments/{id}?w=480"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(thumbnail.status(), StatusCode::OK);
    assert_eq!(thumbnail.headers()[header::CONTENT_TYPE], "image/webp");
    assert_ne!(
        thumbnail.headers()[header::ETAG],
        original.headers()[header::ETAG]
    );
    sqlx::query("delete from message_attachments where id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let deleted = app
        .oneshot(
            Request::get(format!("/api/attachments/{id}?w=480"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        !deleted.status().is_success(),
        "cached derivative cannot bypass attachment ownership"
    );
    std::fs::remove_file(path).unwrap();
    drop_test_schema(pool, database).await;
}

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
