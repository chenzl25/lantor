use std::{
    convert::Infallible,
    env,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Path as AxumPath, State},
    http::{header, HeaderValue, StatusCode},
    response::{
        sse::{Event, KeepAlive},
        IntoResponse, Response, Sse,
    },
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Row, SqlitePool};
use tokio::{
    net::TcpListener,
    time::{sleep, Duration},
};
use tower_http::services::{ServeDir, ServeFile};
use uuid::Uuid;

use crate::agent_profile::{
    create_agent_in_pool, delete_agent_in_pool, update_agent_in_pool, update_owner_profile_in_pool,
};
use crate::agent_workspace::{agent_workspace_list_in_pool, agent_workspace_read_file_in_pool};
use crate::channels::{
    add_agent_to_channel, create_channel_in_pool, delete_channel_in_pool,
    open_dm_with_agent_in_pool, set_channel_agent_membership_in_pool, update_channel_in_pool,
};
use crate::domain::reminders::complete_reminder_in_pool;
use crate::launch_agent;
use crate::message_store::{load_artifact, send_owner_message_in_pool, set_message_saved_in_pool};
use crate::models::AttachmentUpload;
use crate::owner_inbox::{
    dismiss_inbox_items_in_pool, mark_all_owner_inbox_read_in_pool, mark_channel_read_in_pool,
    mark_inbox_items_read_in_pool,
};
use crate::task_store::{update_task_status_in_pool, update_task_title_in_pool};
use crate::ui_notifications::notify_ui_refresh;
use crate::{
    cancel_agent_work_in_pool, check_runtime_in_env, claim_task_in_pool, load_bootstrap,
    retry_agent_work_in_pool, to_string,
};

const WEB_SEND_MESSAGE_BODY_LIMIT: usize = 128 * 1024 * 1024;

#[derive(Clone)]
struct WebState {
    pool: SqlitePool,
    db_url: String,
}

#[derive(Serialize)]
struct ApiError {
    ok: bool,
    message: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendMessageRequest {
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    body: String,
    as_task: bool,
    attachments: Option<Vec<AttachmentUpload>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeCheckRequest {
    runtime: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChannelIdRequest {
    channel_id: Uuid,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateChannelRequest {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    agent_ids: Option<Vec<Uuid>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateChannelRequest {
    channel_id: Uuid,
    name: String,
    description: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetChannelAgentMembershipRequest {
    channel_id: Uuid,
    agent_id: Uuid,
    member: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReminderIdRequest {
    reminder_id: Uuid,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DismissInboxItemRequest {
    item_id: String,
    dismissed_until: DateTime<Utc>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DismissInboxItemsRequest {
    items: Vec<DismissInboxItemRequest>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArtifactReadRequest {
    artifact_id: Uuid,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetMessageSavedRequest {
    message_id: Uuid,
    saved: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentIdRequest {
    agent_id: Uuid,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkItemIdRequest {
    work_item_id: Uuid,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskStatusRequest {
    task_id: Uuid,
    status: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskTitleRequest {
    task_id: Uuid,
    title: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaimTaskRequest {
    task_id: Uuid,
    agent_id: Option<Uuid>,
    expected_version: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateAgentRequest {
    handle: String,
    display_name: String,
    role: Option<String>,
    runtime: String,
    model: String,
    reasoning_effort: Option<String>,
    service_tier: Option<String>,
    avatar: Option<String>,
    description: Option<String>,
    launch_command: String,
    working_directory: String,
    daily_budget_micros: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateAgentRequest {
    agent_id: Uuid,
    handle: String,
    display_name: String,
    role: Option<String>,
    runtime: String,
    model: String,
    reasoning_effort: Option<String>,
    service_tier: Option<String>,
    avatar: Option<String>,
    description: String,
    launch_command: String,
    working_directory: String,
    daily_budget_micros: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentWorkspaceRequest {
    agent_id: Uuid,
    path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OwnerProfileRequest {
    display_name: String,
    avatar: String,
    description: String,
}

pub(crate) const DEFAULT_LANTOR_WEB_BIND: &str = "0.0.0.0:8787";

pub(crate) fn resolve_web_bind() -> Option<String> {
    match env::var("LANTOR_WEB_BIND") {
        Ok(value) => {
            let trimmed = value.trim().to_owned();
            if trimmed.is_empty() {
                return Some(DEFAULT_LANTOR_WEB_BIND.to_owned());
            }
            if matches!(
                trimmed.to_ascii_lowercase().as_str(),
                "off" | "none" | "disabled" | "false" | "0"
            ) {
                return None;
            }
            Some(trimmed)
        }
        Err(_) => Some(DEFAULT_LANTOR_WEB_BIND.to_owned()),
    }
}

pub(crate) fn spawn_web_server_if_configured(pool: SqlitePool, db_url: String) {
    let Some(bind) = resolve_web_bind() else {
        return;
    };
    let Ok(addr) = bind.parse::<SocketAddr>() else {
        eprintln!("Lantor web access disabled: invalid LANTOR_WEB_BIND={bind}");
        return;
    };

    let dist_dir = web_dist_dir();
    tauri::async_runtime::spawn(async move {
        let state = Arc::new(WebState { pool, db_url });
        let app = web_router(state, dist_dir);
        match TcpListener::bind(addr).await {
            Ok(listener) => {
                eprintln!("Lantor web access listening on http://{addr}");
                if let Err(err) = axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .await
                {
                    eprintln!("Lantor web access stopped: {err}");
                }
            }
            Err(err) => {
                eprintln!("Lantor web access failed to bind {addr}: {err}");
            }
        }
    });
}

fn web_router(state: Arc<WebState>, dist_dir: PathBuf) -> Router {
    let index = dist_dir.join("index.html");
    let app = Router::new()
        .route("/api/health", get(api_health))
        .route("/api/bootstrap", get(api_bootstrap))
        .route("/api/check_runtime", post(api_check_runtime))
        .route("/api/events", get(api_events))
        .route("/api/attachments/{attachment_id}", get(api_attachment))
        .route(
            "/api/send_message",
            post(api_send_message).layer(DefaultBodyLimit::max(WEB_SEND_MESSAGE_BODY_LIMIT)),
        )
        .route("/api/create_channel", post(api_create_channel))
        .route("/api/update_channel", post(api_update_channel))
        .route("/api/delete_channel", post(api_delete_channel))
        .route("/api/create_agent", post(api_create_agent))
        .route("/api/update_agent", post(api_update_agent))
        .route("/api/delete_agent", post(api_delete_agent))
        .route(
            "/api/set_channel_agent_membership",
            post(api_set_channel_agent_membership),
        )
        .route("/api/set_message_saved", post(api_set_message_saved))
        .route("/api/update_owner_profile", post(api_update_owner_profile))
        .route("/api/dismiss_inbox_items", post(api_dismiss_inbox_items))
        .route(
            "/api/mark_inbox_items_read",
            post(api_mark_inbox_items_read),
        )
        .route("/api/mark_all_inbox_read", post(api_mark_all_inbox_read))
        .route("/api/mark_channel_read", post(api_mark_channel_read))
        .route("/api/complete_reminder", post(api_complete_reminder))
        .route("/api/update_task_status", post(api_update_task_status))
        .route("/api/update_task_title", post(api_update_task_title))
        .route("/api/claim_task", post(api_claim_task))
        .route("/api/cancel_agent_work", post(api_cancel_agent_work))
        .route("/api/retry_agent_work", post(api_retry_agent_work))
        .route(
            "/api/install_supervisor_service",
            post(api_install_supervisor_service),
        )
        .route(
            "/api/uninstall_supervisor_service",
            post(api_uninstall_supervisor_service),
        )
        .route("/api/artifact_read", post(api_artifact_read))
        .route("/api/open_dm_with_agent", post(api_open_dm_with_agent))
        .route("/api/agent_workspace_list", post(api_agent_workspace_list))
        .route(
            "/api/agent_workspace_read_file",
            post(api_agent_workspace_read_file),
        )
        .with_state(state);

    if index.is_file() {
        app.fallback_service(ServeDir::new(&dist_dir).fallback(ServeFile::new(index)))
    } else {
        app.fallback(get(move || missing_dist(dist_dir)))
    }
}

fn web_dist_dir() -> PathBuf {
    if let Ok(path) = env::var("LANTOR_WEB_DIST") {
        let path = PathBuf::from(path);
        if path.is_dir() {
            return path;
        }
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        manifest_dir.join("../dist"),
        env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("dist"),
    ];
    candidates
        .into_iter()
        .find(|path| path.join("index.html").is_file())
        .unwrap_or_else(|| manifest_dir.join("../dist"))
}

async fn missing_dist(dist_dir: PathBuf) -> impl IntoResponse {
    let body = format!(
        r#"<!doctype html>
<html>
  <head><title>Lantor Web</title></head>
  <body style="font-family: -apple-system, BlinkMacSystemFont, sans-serif; padding: 32px;">
    <h1>Lantor Web build not found</h1>
    <p>Expected <code>{}</code>.</p>
    <p>Run <code>npm run build</code>, then restart Lantor.</p>
  </body>
</html>"#,
        dist_dir.display()
    );
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        body,
    )
}

async fn api_health() -> impl IntoResponse {
    Json(json!({ "ok": true }))
}

async fn api_bootstrap(State(state): State<Arc<WebState>>) -> Result<impl IntoResponse, Response> {
    load_bootstrap(&state.pool, state.db_url.clone())
        .await
        .map(Json)
        .map_err(api_error)
}

async fn api_check_runtime(
    Json(request): Json<RuntimeCheckRequest>,
) -> Result<impl IntoResponse, Response> {
    check_runtime_in_env(request.runtime)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn api_send_message(
    State(state): State<Arc<WebState>>,
    Json(request): Json<SendMessageRequest>,
) -> Result<impl IntoResponse, Response> {
    send_owner_message_in_pool(
        &state.pool,
        request.channel_id,
        request.thread_root_id,
        &request.body,
        request.as_task,
        request.attachments.unwrap_or_default(),
    )
    .await
    .map(|_| Json(json!({ "ok": true })))
    .map_err(api_error)
}

async fn api_create_channel(
    State(state): State<Arc<WebState>>,
    Json(request): Json<CreateChannelRequest>,
) -> Result<impl IntoResponse, Response> {
    let description = request.description.unwrap_or_default();
    let channel_id = create_channel_in_pool(&state.pool, &request.name, &description)
        .await
        .map_err(api_error)?;
    if let Some(ids) = request.agent_ids {
        let mut seen = std::collections::HashSet::new();
        for agent_id in ids {
            if !seen.insert(agent_id) {
                continue;
            }
            add_agent_to_channel(&state.pool, channel_id, agent_id)
                .await
                .map_err(api_error)?;
        }
    }
    Ok(Json(json!({ "ok": true, "channelId": channel_id })))
}

async fn api_update_channel(
    State(state): State<Arc<WebState>>,
    Json(request): Json<UpdateChannelRequest>,
) -> Result<impl IntoResponse, Response> {
    update_channel_in_pool(
        &state.pool,
        request.channel_id,
        request.name,
        request.description,
    )
    .await
    .map(|_| Json(json!({ "ok": true })))
    .map_err(api_error)
}

async fn api_delete_channel(
    State(state): State<Arc<WebState>>,
    Json(request): Json<ChannelIdRequest>,
) -> Result<impl IntoResponse, Response> {
    delete_channel_in_pool(&state.pool, request.channel_id)
        .await
        .map(|_| Json(json!({ "ok": true })))
        .map_err(api_error)
}

async fn api_create_agent(
    State(state): State<Arc<WebState>>,
    Json(request): Json<CreateAgentRequest>,
) -> Result<impl IntoResponse, Response> {
    create_agent_in_pool(
        &state.pool,
        request.handle,
        request.display_name,
        request.role,
        request.runtime,
        request.model,
        request.reasoning_effort,
        request.service_tier,
        request.avatar,
        request.description,
        request.launch_command,
        request.working_directory,
        request.daily_budget_micros,
    )
    .await
    .map(|agent_id| Json(agent_id.to_string()))
    .map_err(api_error)
}

async fn api_update_agent(
    State(state): State<Arc<WebState>>,
    Json(request): Json<UpdateAgentRequest>,
) -> Result<impl IntoResponse, Response> {
    update_agent_in_pool(
        &state.pool,
        request.agent_id,
        request.handle,
        request.display_name,
        request.role,
        request.runtime,
        request.model,
        request.reasoning_effort,
        request.service_tier,
        request.avatar,
        request.description,
        request.launch_command,
        request.working_directory,
        request.daily_budget_micros,
    )
    .await
    .map(|_| Json(json!({ "ok": true })))
    .map_err(api_error)
}

async fn api_delete_agent(
    State(state): State<Arc<WebState>>,
    Json(request): Json<AgentIdRequest>,
) -> Result<impl IntoResponse, Response> {
    delete_agent_in_pool(&state.pool, request.agent_id)
        .await
        .map(|_| Json(json!({ "ok": true })))
        .map_err(api_error)
}

async fn api_set_channel_agent_membership(
    State(state): State<Arc<WebState>>,
    Json(request): Json<SetChannelAgentMembershipRequest>,
) -> Result<impl IntoResponse, Response> {
    set_channel_agent_membership_in_pool(
        &state.pool,
        request.channel_id,
        request.agent_id,
        request.member,
    )
    .await
    .map(|_| Json(json!({ "ok": true })))
    .map_err(api_error)
}

async fn api_update_owner_profile(
    State(state): State<Arc<WebState>>,
    Json(request): Json<OwnerProfileRequest>,
) -> Result<impl IntoResponse, Response> {
    update_owner_profile_in_pool(
        &state.pool,
        request.display_name,
        request.avatar,
        request.description,
    )
    .await
    .map(|_| Json(json!({ "ok": true })))
    .map_err(api_error)
}

async fn api_mark_channel_read(
    State(state): State<Arc<WebState>>,
    Json(request): Json<ChannelIdRequest>,
) -> Result<impl IntoResponse, Response> {
    mark_channel_read_in_pool(&state.pool, request.channel_id)
        .await
        .map(|_| Json(json!({ "ok": true })))
        .map_err(api_error)
}

async fn api_dismiss_inbox_items(
    State(state): State<Arc<WebState>>,
    Json(request): Json<DismissInboxItemsRequest>,
) -> Result<impl IntoResponse, Response> {
    dismiss_inbox_items_in_pool(
        &state.pool,
        request
            .items
            .into_iter()
            .map(|item| (item.item_id, item.dismissed_until)),
    )
    .await
    .map(|_| Json(json!({ "ok": true })))
    .map_err(api_error)
}

async fn api_mark_inbox_items_read(
    State(state): State<Arc<WebState>>,
    Json(request): Json<DismissInboxItemsRequest>,
) -> Result<impl IntoResponse, Response> {
    mark_inbox_items_read_in_pool(
        &state.pool,
        request
            .items
            .into_iter()
            .map(|item| (item.item_id, item.dismissed_until)),
    )
    .await
    .map(|_| Json(json!({ "ok": true })))
    .map_err(api_error)
}

async fn api_mark_all_inbox_read(
    State(state): State<Arc<WebState>>,
) -> Result<impl IntoResponse, Response> {
    mark_all_owner_inbox_read_in_pool(&state.pool)
        .await
        .map(|_| Json(json!({ "ok": true })))
        .map_err(api_error)
}

async fn api_set_message_saved(
    State(state): State<Arc<WebState>>,
    Json(request): Json<SetMessageSavedRequest>,
) -> Result<impl IntoResponse, Response> {
    set_message_saved_in_pool(&state.pool, request.message_id, request.saved)
        .await
        .map(|_| Json(json!({ "ok": true })))
        .map_err(api_error)
}

async fn api_complete_reminder(
    State(state): State<Arc<WebState>>,
    Json(request): Json<ReminderIdRequest>,
) -> Result<impl IntoResponse, Response> {
    complete_reminder_in_pool(&state.pool, request.reminder_id)
        .await
        .map(|_| Json(json!({ "ok": true })))
        .map_err(api_error)
}

async fn api_update_task_status(
    State(state): State<Arc<WebState>>,
    Json(request): Json<TaskStatusRequest>,
) -> Result<impl IntoResponse, Response> {
    update_task_status_in_pool(&state.pool, request.task_id, request.status)
        .await
        .map(|_| Json(json!({ "ok": true })))
        .map_err(api_error)
}

async fn api_update_task_title(
    State(state): State<Arc<WebState>>,
    Json(request): Json<TaskTitleRequest>,
) -> Result<impl IntoResponse, Response> {
    update_task_title_in_pool(&state.pool, request.task_id, request.title)
        .await
        .map(|_| Json(json!({ "ok": true })))
        .map_err(api_error)
}

async fn api_claim_task(
    State(state): State<Arc<WebState>>,
    Json(request): Json<ClaimTaskRequest>,
) -> Result<impl IntoResponse, Response> {
    claim_task_in_pool(
        &state.pool,
        request.task_id,
        request.agent_id,
        request.expected_version,
    )
    .await
    .map(|_| Json(json!({ "ok": true })))
    .map_err(api_error)
}

async fn api_cancel_agent_work(
    State(state): State<Arc<WebState>>,
    Json(request): Json<WorkItemIdRequest>,
) -> Result<impl IntoResponse, Response> {
    cancel_agent_work_in_pool(&state.pool, request.work_item_id)
        .await
        .map(|_| Json(json!({ "ok": true })))
        .map_err(api_error)
}

async fn api_retry_agent_work(
    State(state): State<Arc<WebState>>,
    Json(request): Json<WorkItemIdRequest>,
) -> Result<impl IntoResponse, Response> {
    retry_agent_work_in_pool(&state.pool, request.work_item_id)
        .await
        .map(|work_item_id| Json(json!({ "workItemId": work_item_id })))
        .map_err(api_error)
}

async fn api_install_supervisor_service(
    State(state): State<Arc<WebState>>,
) -> Result<impl IntoResponse, Response> {
    let status = launch_agent::install_supervisor_service(&state.db_url).map_err(api_error)?;
    let _ = notify_ui_refresh(&state.pool, "supervisor_service_installed").await;
    Ok(Json(status))
}

async fn api_uninstall_supervisor_service(
    State(state): State<Arc<WebState>>,
) -> Result<impl IntoResponse, Response> {
    let status = launch_agent::uninstall_supervisor_service().map_err(api_error)?;
    sqlx::query("update supervisor_state set status = 'offline', updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') where id = 1")
        .execute(&state.pool)
        .await
        .map_err(to_string)
        .map_err(api_error)?;
    let _ = notify_ui_refresh(&state.pool, "supervisor_service_uninstalled").await;
    Ok(Json(status))
}

async fn api_artifact_read(
    State(state): State<Arc<WebState>>,
    Json(request): Json<ArtifactReadRequest>,
) -> Result<impl IntoResponse, Response> {
    load_artifact(&state.pool, request.artifact_id)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn api_open_dm_with_agent(
    State(state): State<Arc<WebState>>,
    Json(request): Json<AgentIdRequest>,
) -> Result<impl IntoResponse, Response> {
    open_dm_with_agent_in_pool(&state.pool, request.agent_id)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn api_agent_workspace_list(
    State(state): State<Arc<WebState>>,
    Json(request): Json<AgentWorkspaceRequest>,
) -> Result<impl IntoResponse, Response> {
    agent_workspace_list_in_pool(&state.pool, request.agent_id, &request.path)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn api_agent_workspace_read_file(
    State(state): State<Arc<WebState>>,
    Json(request): Json<AgentWorkspaceRequest>,
) -> Result<impl IntoResponse, Response> {
    agent_workspace_read_file_in_pool(&state.pool, request.agent_id, &request.path)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn api_events(State(state): State<Arc<WebState>>) -> Result<impl IntoResponse, Response> {
    let pool = state.pool.clone();
    let stream = async_stream::stream! {
        let mut last_id: i64 = sqlx::query_scalar("select coalesce(max(id), 0) from ui_events")
            .fetch_one(&pool)
            .await
            .unwrap_or(0);
        loop {
            match sqlx::query(
                r#"
                select id, event_json
                from ui_events
                where id > $1
                order by id asc
                limit 80
                "#,
            )
            .bind(last_id)
            .fetch_all(&pool)
            .await {
                Ok(rows) if rows.is_empty() => {
                    sleep(Duration::from_millis(500)).await;
                }
                Ok(rows) => {
                    for row in rows {
                        last_id = row.get("id");
                        yield Ok::<Event, Infallible>(
                            Event::default().event("lantor").data(row.get::<String, _>("event_json"))
                        );
                    }
                },
                Err(err) => {
                    yield Ok(Event::default().event("error").data(err.to_string()));
                    sleep(Duration::from_secs(2)).await;
                },
            }
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

async fn api_attachment(
    State(state): State<Arc<WebState>>,
    AxumPath(attachment_id): AxumPath<Uuid>,
) -> Result<Response, Response> {
    let row = sqlx::query(
        r#"
        select original_name, mime_type, storage_path
        from message_attachments
        where id = $1
        "#,
    )
    .bind(attachment_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(to_string)
    .map_err(api_error)?
    .ok_or_else(|| api_error("attachment does not exist".to_owned()))?;

    let original_name: String = row.get("original_name");
    let mime_type: String = row.get("mime_type");
    let storage_path: String = row.get("storage_path");
    let bytes = tokio::fs::read(Path::new(&storage_path))
        .await
        .map_err(to_string)
        .map_err(api_error)?;
    let content_type = if mime_type.trim().is_empty() {
        mime_guess::from_path(&storage_path)
            .first_or_octet_stream()
            .to_string()
    } else {
        mime_type
    };
    let mut response = Response::new(Body::from(bytes));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&content_type)
            .unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!(
            "inline; filename=\"{}\"",
            original_name.replace('"', "")
        ))
        .unwrap_or(HeaderValue::from_static("inline")),
    );
    Ok(response)
}

fn api_error(message: String) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiError { ok: false, message }),
    )
        .into_response()
}
