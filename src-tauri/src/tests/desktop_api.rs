use axum::{
    body::{to_bytes, Body},
    extract::Multipart,
    http::Request,
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

use super::{forward, request_body_bytes};
use crate::test_support::{drop_test_schema, insert_test_agent, test_pool};
use crate::web::desktop_api_router;

const AVATAR: &str = "data:image/png;base64,iVBORw0KGgo=";

fn json_body(value: Value) -> Vec<u8> {
    value.to_string().into_bytes()
}

fn parse(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).expect("JSON response")
}

fn agent_avatar(payload: &Value) -> Option<Value> {
    payload["agents"]
        .as_array()?
        .iter()
        .find(|agent| agent["handle"] == "pictured")
        .map(|agent| agent["avatar"].clone())
}

#[tokio::test]
async fn desktop_requests_get_the_desktop_bootstrap_and_inline_avatars() {
    let Some((pool, database_path)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "pictured").await?;
        sqlx::query("update agents set avatar = $2 where id = $1")
            .bind(agent_id)
            .bind(AVATAR)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        let router = desktop_api_router(pool.clone(), "sqlite://test".to_owned());

        let bootstrap = parse(
            &forward(
                &router,
                "GET",
                "/api/bootstrap?currentChannelOnly=true",
                None,
                Vec::new(),
            )
            .await?,
        );
        assert_eq!(agent_avatar(&bootstrap), Some(json!(AVATAR)));
        // Browsers asking the same router get the avatar as a URL.
        let response = router
            .clone()
            .oneshot(
                Request::get("/api/bootstrap?currentChannelOnly=true")
                    .body(Body::empty())
                    .map_err(|err| err.to_string())?,
            )
            .await
            .map_err(|err| err.to_string())?;
        let web_bootstrap = parse(
            &to_bytes(response.into_body(), usize::MAX)
                .await
                .map_err(|err| err.to_string())?,
        );
        let web_avatar = agent_avatar(&web_bootstrap).ok_or("no web avatar")?;
        assert!(
            web_avatar
                .as_str()
                .is_some_and(|url| url.starts_with("/api/avatars/agents/")),
            "{web_avatar}"
        );

        let detail = parse(
            &forward(
                &router,
                "POST",
                "/api/load_agent_detail",
                Some("application/json"),
                json_body(json!({ "agentId": agent_id })),
            )
            .await?,
        );
        assert_eq!(detail["agent"]["avatar"], AVATAR);

        let patch = parse(
            &forward(
                &router,
                "POST",
                "/api/load_ui_state",
                Some("application/json"),
                json_body(json!({ "scopes": ["agents"] })),
            )
            .await?,
        );
        assert_eq!(agent_avatar(&patch), Some(json!(AVATAR)));
        Ok(())
    }
    .await;
    drop_test_schema(pool, database_path).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn desktop_requests_keep_multipart_bodies_intact() {
    // Echoes each multipart field, like the send_message upload parser reads
    // them, without writing attachment files.
    async fn echo(mut multipart: Multipart) -> Json<Vec<(String, Option<String>, String)>> {
        let mut fields = Vec::new();
        while let Some(field) = multipart.next_field().await.expect("multipart field") {
            let name = field.name().unwrap_or_default().to_owned();
            let file_name = field.file_name().map(str::to_owned);
            let text = field.text().await.expect("field text");
            fields.push((name, file_name, text));
        }
        Json(fields)
    }
    let router = Router::new().route("/api/echo", post(echo));
    let boundary = "lantor-test-boundary";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"request\"\r\n\r\n{{\"body\":\"hi\"}}\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"attachments\"; filename=\"note.txt\"\r\n\
         Content-Type: text/plain\r\n\r\nhello\r\n--{boundary}--\r\n"
    );
    let fields = parse(
        &forward(
            &router,
            "POST",
            "/api/echo",
            Some(&format!("multipart/form-data; boundary={boundary}")),
            body.into_bytes(),
        )
        .await
        .expect("multipart request"),
    );
    assert_eq!(
        fields,
        json!([
            ["request", null, "{\"body\":\"hi\"}"],
            ["attachments", "note.txt", "hello"]
        ])
    );
}

#[tokio::test]
async fn desktop_request_errors_carry_the_api_message() {
    let Some((pool, database_path)) = test_pool().await else {
        return;
    };
    let router = desktop_api_router(pool.clone(), "sqlite://test".to_owned());
    let missing = forward(
        &router,
        "POST",
        "/api/update_task_title",
        Some("application/json"),
        json_body(json!({ "taskId": Uuid::new_v4(), "title": "renamed" })),
    )
    .await;
    let not_api = forward(&router, "GET", "/index.html", None, Vec::new()).await;
    drop_test_schema(pool, database_path).await;

    let message = missing.expect_err("updating a missing task fails");
    assert!(!message.trim().is_empty());
    assert!(!message.starts_with('{'), "{message}");
    assert_eq!(not_api, Err("not an API path: /index.html".to_owned()));
}

#[test]
fn request_bodies_arrive_raw_or_as_a_byte_array() {
    use tauri::ipc::InvokeBody;

    assert_eq!(
        request_body_bytes(&InvokeBody::Raw(vec![0, 7, 255])),
        Ok(vec![0, 7, 255])
    );
    assert_eq!(
        request_body_bytes(&InvokeBody::Json(json!([0, 7, 255]))),
        Ok(vec![0, 7, 255])
    );
    assert_eq!(
        request_body_bytes(&InvokeBody::Json(json!([]))),
        Ok(Vec::new())
    );
    assert_eq!(
        request_body_bytes(&InvokeBody::Json(Value::Null)),
        Ok(Vec::new())
    );
    assert!(request_body_bytes(&InvokeBody::Json(json!([256]))).is_err());
    assert!(request_body_bytes(&InvokeBody::Json(json!({ "cursor": 1 }))).is_err());
}
