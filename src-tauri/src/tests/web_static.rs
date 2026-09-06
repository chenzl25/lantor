use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    body::{to_bytes, Body},
    http::{header, Method, Request, StatusCode},
    response::Response,
    Router,
};
use sqlx::SqlitePool;
use tower::ServiceExt;
use uuid::Uuid;

use super::{web_router, WebState};
use crate::test_support::{drop_test_schema, test_pool};

struct StaticDir(PathBuf);

impl StaticDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("lantor-web-static-{}", Uuid::new_v4()));
        fs::create_dir(&path).expect("create static fixture");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for StaticDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn static_fixture() -> StaticDir {
    let dist = StaticDir::new();
    fs::create_dir(dist.path().join("assets")).unwrap();
    // Opaque sidecar bytes test representation selection; the build-script tests
    // separately verify actual gzip/Brotli round-trips against the source files.
    for (path, bytes) in [
        ("index.html", "<html>Lantor</html>"),
        ("index.html.gz", "gzip html representation"),
        ("index.html.br", "brotli html representation"),
        ("assets/main.js", "console.log('Lantor');"),
        ("assets/main.js.gz", "gzip js representation"),
        ("assets/main.js.br", "brotli js representation"),
        ("assets/plain.css", "body { color: white; }"),
        ("lantor-icon.png", "png representation"),
    ] {
        fs::write(dist.path().join(path), bytes).unwrap();
    }
    dist
}

fn fixture_router(dist: &StaticDir, pool: SqlitePool) -> Router {
    web_router(
        Arc::new(WebState {
            pool,
            db_url: "sqlite::memory:".to_owned(),
        }),
        dist.path().to_owned(),
    )
}

async fn request(app: &Router, method: Method, path: &str, encoding: Option<&str>) -> Response {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(encoding) = encoding {
        request = request.header(header::ACCEPT_ENCODING, encoding);
    }
    app.clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn web_static_negotiates_sidecars_and_preserves_cache_headers() {
    let dist = static_fixture();
    let app = fixture_router(&dist, SqlitePool::connect_lazy("sqlite::memory:").unwrap());
    for (accept, expected_encoding, expected_body) in [
        (Some("br,gzip"), Some("br"), "brotli js representation"),
        (Some("gzip"), Some("gzip"), "gzip js representation"),
        (
            Some("gzip;q=1,br;q=0.5"),
            Some("gzip"),
            "gzip js representation",
        ),
        (Some("br;q=0,gzip;q=0"), None, "console.log('Lantor');"),
        (Some("identity"), None, "console.log('Lantor');"),
        (None, None, "console.log('Lantor');"),
    ] {
        let response = request(&app, Method::GET, "/assets/main.js", accept).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_ENCODING)
                .map(|value| value.to_str().unwrap()),
            expected_encoding,
        );
        assert_eq!(response.headers()[header::VARY], "Accept-Encoding");
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "public, max-age=31536000, immutable"
        );
        assert!(response.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .contains("javascript"));
        assert_eq!(
            to_bytes(response.into_body(), 1024).await.unwrap().as_ref(),
            expected_body.as_bytes()
        );
    }

    let response = request(&app, Method::HEAD, "/assets/main.js", Some("br,gzip")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_ENCODING], "br");
    assert_eq!(
        response.headers()[header::CONTENT_LENGTH],
        "brotli js representation".len().to_string()
    );
    assert!(to_bytes(response.into_body(), 1024)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn web_static_compresses_spa_fallback_and_serves_files_without_sidecars() {
    let dist = static_fixture();
    let app = fixture_router(&dist, SqlitePool::connect_lazy("sqlite::memory:").unwrap());
    for path in ["/", "/channels/example"] {
        let response = request(&app, Method::GET, path, Some("br,gzip")).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_ENCODING], "br");
        assert_eq!(response.headers()[header::CONTENT_TYPE], "text/html");
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-cache");
        assert_eq!(
            to_bytes(response.into_body(), 1024).await.unwrap().as_ref(),
            b"brotli html representation"
        );
    }
    for (path, mime, body) in [
        ("/assets/plain.css", "text/css", "body { color: white; }"),
        ("/lantor-icon.png", "image/png", "png representation"),
    ] {
        let response = request(&app, Method::GET, path, Some("br,gzip")).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!response.headers().contains_key(header::CONTENT_ENCODING));
        assert_eq!(response.headers()[header::CONTENT_TYPE], mime);
        assert_eq!(response.headers()[header::VARY], "Accept-Encoding");
        assert_eq!(
            to_bytes(response.into_body(), 1024).await.unwrap().as_ref(),
            body.as_bytes()
        );
    }
}

#[tokio::test]
async fn web_static_compression_does_not_wrap_api_or_sse() {
    let (pool, schema) = test_pool().await.expect("initialize test database");
    let dist = static_fixture();
    let app = fixture_router(&dist, pool.clone());
    for (path, mime) in [
        ("/api/health", "application/json"),
        ("/api/events", "text/event-stream"),
    ] {
        let response = request(&app, Method::GET, path, Some("br,gzip")).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with(mime));
        assert!(!response.headers().contains_key(header::CONTENT_ENCODING));
        assert!(!response.headers().contains_key(header::VARY));
    }
    drop(app);
    drop_test_schema(pool, schema).await;
}
