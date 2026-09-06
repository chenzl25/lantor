use std::{
    convert::Infallible,
    env,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    body::Body,
    extract::{DefaultBodyLimit, FromRequest, Multipart, Path as AxumPath, Query, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{from_fn, Next},
    response::{
        sse::{Event, KeepAlive},
        IntoResponse, Response, Sse,
    },
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Row, SqlitePool};
use tokio::{
    net::TcpListener,
    time::{sleep, Duration},
};
use tower_http::{
    compression::CompressionLayer,
    services::{ServeDir, ServeFile},
};
use uuid::Uuid;

use crate::agent_workspace::{agent_workspace_list_in_pool, agent_workspace_read_file_in_pool};
use crate::application::{
    agents::{self as agent_commands, CreateAgentRequest, OwnerProfileRequest, UpdateAgentRequest},
    artifacts::{self as artifact_commands, ArtifactReadRequest},
    bootstrap::{
        self as bootstrap_command, BootstrapRequest as ApplicationBootstrapRequest,
        BootstrapSurface,
    },
    channels::{
        self as channel_commands, ChannelIdRequest, CreateChannelRequest,
        SetChannelAgentMembershipRequest, UpdateChannelRequest,
    },
    github::{
        self as github_commands, BindGithubRepositoryRequest, CreateGithubIssueTaskRequest,
        CreateGithubReviewTaskRequest, GithubChannelRequest, GithubIssueRequest,
        RereviewGithubPullRequestRequest,
    },
    inbox::{self as inbox_commands, InboxItemsRequest, MarkChannelReadRequest},
    messages::{
        self as message_commands, LoadActivityMessagesRequest, LoadChannelMessagesRequest,
        LoadOlderChannelMessagesRequest, MessageIdRequest, SearchMessagesRequest,
        SendMessageRequest, SetMessageSavedRequest,
    },
    tasks::{self as task_commands, UpdateTaskStatusRequest, UpdateTaskTitleRequest},
    wiki::{
        self as wiki_commands, LoadChannelWikiRequest, PublishChannelWikiRequest,
        SearchChannelWikisRequest,
    },
    AgentIdRequest,
};
use crate::domain::reminders::complete_reminder_in_pool;
use crate::launch_agent;
use crate::lifecycle_commands::start_agent_in_pool;
use crate::system_commands::check_runtime_in_env;
use crate::ui_notifications::{enqueue_ui_event, enqueue_ui_event_in_tx, UiEvent};
use crate::web_upload::parse_multipart_send_message;
use crate::{
    app::{to_string, CommandResult},
    cancel_agent_work_in_pool, claim_task_in_pool, retry_agent_work_in_pool,
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

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BootstrapQuery {
    channel_id: Option<String>,
    #[serde(default)]
    current_channel_only: bool,
}

#[derive(Default, Deserialize)]
struct EventsQuery {
    cursor: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeCheckRequest {
    runtime: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReminderIdRequest {
    reminder_id: Uuid,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkItemIdRequest {
    work_item_id: Uuid,
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
struct AgentWorkspaceRequest {
    agent_id: Uuid,
    path: String,
}

pub(crate) const DEFAULT_LANTOR_WEB_BIND: &str = "127.0.0.1:8787";

fn resolve_web_bind_value(configured: Option<&str>) -> Option<String> {
    let trimmed = configured.unwrap_or_default().trim();
    if trimmed.is_empty() {
        return Some(DEFAULT_LANTOR_WEB_BIND.to_owned());
    }
    if matches!(
        trimmed.to_ascii_lowercase().as_str(),
        "off" | "none" | "disabled" | "false" | "0"
    ) {
        return None;
    }
    Some(trimmed.to_owned())
}

pub(crate) fn resolve_web_bind() -> Option<String> {
    let configured = env::var("LANTOR_WEB_BIND").ok();
    resolve_web_bind_value(configured.as_deref())
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
        .route(
            "/api/bootstrap",
            get(api_bootstrap).layer(CompressionLayer::new()),
        )
        .route("/api/check_runtime", post(api_check_runtime))
        .route("/api/events", get(api_events))
        .route("/api/attachments/{attachment_id}", get(api_attachment))
        .route(
            "/api/send_message",
            post(api_send_message).layer(DefaultBodyLimit::max(WEB_SEND_MESSAGE_BODY_LIMIT)),
        )
        .route(
            "/api/load_older_channel_messages",
            post(api_load_older_channel_messages).layer(CompressionLayer::new()),
        )
        .route(
            "/api/load_channel_messages",
            post(api_load_channel_messages).layer(CompressionLayer::new()),
        )
        .route(
            "/api/load_channel_previews",
            post(api_load_channel_previews).layer(CompressionLayer::new()),
        )
        .route(
            "/api/load_activity_messages",
            post(api_load_activity_messages).layer(CompressionLayer::new()),
        )
        .route(
            "/api/search_messages",
            post(api_search_messages).layer(CompressionLayer::new()),
        )
        .route(
            "/api/load_message",
            post(api_load_message).layer(CompressionLayer::new()),
        )
        .route("/api/create_channel", post(api_create_channel))
        .route("/api/update_channel", post(api_update_channel))
        .route("/api/delete_channel", post(api_delete_channel))
        .route(
            "/api/load_channel_wiki",
            post(api_load_channel_wiki).layer(CompressionLayer::new()),
        )
        .route("/api/publish_channel_wiki", post(api_publish_channel_wiki))
        .route(
            "/api/search_channel_wikis",
            post(api_search_channel_wikis).layer(CompressionLayer::new()),
        )
        .route(
            "/api/load_github_review_comparisons",
            post(api_load_github_review_comparisons),
        )
        .route(
            "/api/load_github_review_queue",
            post(api_load_github_review_queue),
        )
        .route(
            "/api/refresh_github_review_queue",
            post(api_refresh_github_review_queue),
        )
        .route(
            "/api/refresh_github_issue_queue",
            post(api_refresh_github_issue_queue),
        )
        .route(
            "/api/mark_github_review_attention_read",
            post(api_mark_github_review_attention_read),
        )
        .route(
            "/api/load_github_issue_detail",
            post(api_load_github_issue_detail),
        )
        .route(
            "/api/bind_github_repository",
            post(api_bind_github_repository),
        )
        .route(
            "/api/create_github_review_task",
            post(api_create_github_review_task),
        )
        .route(
            "/api/rereview_github_pull_request",
            post(api_rereview_github_pull_request),
        )
        .route(
            "/api/create_github_issue_task",
            post(api_create_github_issue_task),
        )
        .route("/api/create_agent", post(api_create_agent))
        .route("/api/update_agent", post(api_update_agent))
        .route("/api/delete_agent", post(api_delete_agent))
        .route("/api/start_agent", post(api_start_agent))
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

    let app = if index.is_file() {
        // Compression is generated once by the web build, not per request.
        // Keep it on the static fallback so API responses (especially SSE) are untouched.
        app.fallback_service(
            ServeDir::new(&dist_dir)
                .precompressed_gzip()
                .precompressed_br()
                .fallback(ServeFile::new(index).precompressed_gzip().precompressed_br()),
        )
    } else {
        app.fallback(get(move || missing_dist(dist_dir)))
    };
    app.layer(from_fn(static_cache_control))
}

/// ServeDir emits no Cache-Control header, so browsers fall back to heuristic
/// caching keyed off Last-Modified and can keep showing a stale UI for hours
/// after `npm run build`. Hashed /assets/ bundles are immutable by
/// construction; everything else (notably index.html) must revalidate on
/// every load. API responses are left untouched.
async fn static_cache_control(request: Request, next: Next) -> Response {
    let path = request.uri().path();
    let is_api = path.starts_with("/api/");
    let is_hashed_asset = path.starts_with("/assets/");
    let mut response = next.run(request).await;
    if !is_api {
        let value = if is_hashed_asset {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        };
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static(value));
        // ServeDir negotiates precompressed files but does not emit Vary. Include
        // identity responses too, so caches cannot reuse one encoding for another.
        response
            .headers_mut()
            .append(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    }
    response
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
  <body style="font-family: -apple-system, BlinkMacSystemFont, &quot;Segoe UI&quot;, system-ui, sans-serif; padding: 32px;">
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

async fn api_bootstrap(
    State(state): State<Arc<WebState>>,
    Query(query): Query<BootstrapQuery>,
) -> Result<impl IntoResponse, Response> {
    let current_channel_only = query.current_channel_only
        || query
            .channel_id
            .as_deref()
            .is_some_and(|channel_id| !channel_id.trim().is_empty());
    let channel_id = query
        .channel_id
        .as_deref()
        .and_then(|channel_id| Uuid::parse_str(channel_id).ok());
    bootstrap_command::bootstrap(
        &state.pool,
        state.db_url.clone(),
        BootstrapSurface::Web,
        ApplicationBootstrapRequest {
            channel_id,
            current_channel_only,
        },
    )
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

fn is_multipart_request(request: &Request) -> bool {
    request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .to_ascii_lowercase()
                .starts_with("multipart/form-data;")
        })
}

async fn extract_send_message_request(
    request: Request,
    state: &Arc<WebState>,
) -> Result<SendMessageRequest, Response> {
    if is_multipart_request(&request) {
        let multipart = Multipart::from_request(request, state)
            .await
            .map_err(|rejection| rejection.into_response())?;
        return parse_multipart_send_message(multipart)
            .await
            .map_err(api_error);
    }

    Json::<SendMessageRequest>::from_request(request, state)
        .await
        .map(|Json(request)| request)
        .map_err(|rejection| rejection.into_response())
}

async fn api_send_message(
    State(state): State<Arc<WebState>>,
    request: Request,
) -> Result<impl IntoResponse, Response> {
    let request = extract_send_message_request(request, &state).await?;
    message_commands::send_message(&state.pool, request)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn api_load_older_channel_messages(
    State(state): State<Arc<WebState>>,
    Json(request): Json<LoadOlderChannelMessagesRequest>,
) -> Result<impl IntoResponse, Response> {
    message_commands::load_older_channel_messages(&state.pool, request)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn api_load_channel_messages(
    State(state): State<Arc<WebState>>,
    Json(request): Json<LoadChannelMessagesRequest>,
) -> Result<impl IntoResponse, Response> {
    message_commands::load_channel_messages(&state.pool, request)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn api_load_channel_previews(
    State(state): State<Arc<WebState>>,
) -> Result<impl IntoResponse, Response> {
    message_commands::load_channel_previews(&state.pool)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn api_load_activity_messages(
    State(state): State<Arc<WebState>>,
    Json(request): Json<LoadActivityMessagesRequest>,
) -> Result<impl IntoResponse, Response> {
    message_commands::load_activity_messages(&state.pool, request)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn api_search_messages(
    State(state): State<Arc<WebState>>,
    Json(request): Json<SearchMessagesRequest>,
) -> Result<impl IntoResponse, Response> {
    message_commands::search_messages(&state.pool, request)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn api_load_message(
    State(state): State<Arc<WebState>>,
    Json(request): Json<MessageIdRequest>,
) -> Result<impl IntoResponse, Response> {
    message_commands::load_message(&state.pool, request)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn api_create_channel(
    State(state): State<Arc<WebState>>,
    Json(request): Json<CreateChannelRequest>,
) -> Result<impl IntoResponse, Response> {
    let result = channel_commands::create_channel(&state.pool, request)
        .await
        .map_err(api_error)?;
    Ok(Json(json!({ "ok": true, "channelId": result.channel_id })))
}

async fn api_update_channel(
    State(state): State<Arc<WebState>>,
    Json(request): Json<UpdateChannelRequest>,
) -> Result<impl IntoResponse, Response> {
    channel_commands::update_channel(&state.pool, request)
        .await
        .map(|_| Json(json!({ "ok": true })))
        .map_err(api_error)
}

async fn api_delete_channel(
    State(state): State<Arc<WebState>>,
    Json(request): Json<ChannelIdRequest>,
) -> Result<impl IntoResponse, Response> {
    channel_commands::delete_channel(&state.pool, request)
        .await
        .map(|_| Json(json!({ "ok": true })))
        .map_err(api_error)
}

async fn api_load_channel_wiki(
    State(state): State<Arc<WebState>>,
    Json(request): Json<LoadChannelWikiRequest>,
) -> Result<impl IntoResponse, Response> {
    wiki_commands::load_channel_wiki(&state.pool, request)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn api_publish_channel_wiki(
    State(state): State<Arc<WebState>>,
    Json(request): Json<PublishChannelWikiRequest>,
) -> Result<impl IntoResponse, Response> {
    wiki_commands::publish_channel_wiki(&state.pool, request)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn api_search_channel_wikis(
    State(state): State<Arc<WebState>>,
    Json(request): Json<SearchChannelWikisRequest>,
) -> Result<impl IntoResponse, Response> {
    wiki_commands::search_channel_wikis(&state.pool, request)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn api_load_github_review_comparisons(
    State(state): State<Arc<WebState>>,
    Json(request): Json<GithubChannelRequest>,
) -> Result<impl IntoResponse, Response> {
    github_commands::load_github_review_comparisons(&state.pool, request)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn api_load_github_review_queue(
    State(state): State<Arc<WebState>>,
    Json(request): Json<GithubChannelRequest>,
) -> Result<impl IntoResponse, Response> {
    github_commands::load_github_review_queue(&state.pool, request)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn api_refresh_github_review_queue(
    State(state): State<Arc<WebState>>,
    Json(request): Json<GithubChannelRequest>,
) -> Result<impl IntoResponse, Response> {
    github_commands::refresh_github_review_queue(&state.pool, request)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn api_refresh_github_issue_queue(
    State(state): State<Arc<WebState>>,
    Json(request): Json<GithubChannelRequest>,
) -> Result<impl IntoResponse, Response> {
    github_commands::refresh_github_issue_queue(&state.pool, request)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn api_mark_github_review_attention_read(
    State(state): State<Arc<WebState>>,
    Json(request): Json<GithubChannelRequest>,
) -> Result<impl IntoResponse, Response> {
    github_commands::mark_github_review_attention_read(&state.pool, request)
        .await
        .map(|_| Json(json!({ "ok": true })))
        .map_err(api_error)
}

async fn api_load_github_issue_detail(
    State(state): State<Arc<WebState>>,
    Json(request): Json<GithubIssueRequest>,
) -> Result<impl IntoResponse, Response> {
    github_commands::load_github_issue_detail(&state.pool, request)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn api_bind_github_repository(
    State(state): State<Arc<WebState>>,
    Json(request): Json<BindGithubRepositoryRequest>,
) -> Result<impl IntoResponse, Response> {
    github_commands::bind_github_repository(&state.pool, request)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn api_create_github_review_task(
    State(state): State<Arc<WebState>>,
    Json(request): Json<CreateGithubReviewTaskRequest>,
) -> Result<impl IntoResponse, Response> {
    github_commands::create_github_review_task(&state.pool, request)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn api_rereview_github_pull_request(
    State(state): State<Arc<WebState>>,
    Json(request): Json<RereviewGithubPullRequestRequest>,
) -> Result<impl IntoResponse, Response> {
    github_commands::rereview_github_pull_request(&state.pool, request)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn api_create_github_issue_task(
    State(state): State<Arc<WebState>>,
    Json(request): Json<CreateGithubIssueTaskRequest>,
) -> Result<impl IntoResponse, Response> {
    github_commands::create_github_issue_task(&state.pool, request)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn api_create_agent(
    State(state): State<Arc<WebState>>,
    Json(request): Json<CreateAgentRequest>,
) -> Result<impl IntoResponse, Response> {
    agent_commands::create_agent(&state.pool, request)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn api_update_agent(
    State(state): State<Arc<WebState>>,
    Json(request): Json<UpdateAgentRequest>,
) -> Result<impl IntoResponse, Response> {
    agent_commands::update_agent(&state.pool, request)
        .await
        .map(|_| Json(json!({ "ok": true })))
        .map_err(api_error)
}

async fn api_delete_agent(
    State(state): State<Arc<WebState>>,
    Json(request): Json<AgentIdRequest>,
) -> Result<impl IntoResponse, Response> {
    agent_commands::delete_agent(&state.pool, request)
        .await
        .map(|_| Json(json!({ "ok": true })))
        .map_err(api_error)
}

async fn api_start_agent(
    State(state): State<Arc<WebState>>,
    Json(request): Json<AgentIdRequest>,
) -> Result<impl IntoResponse, Response> {
    start_agent_in_pool(&state.pool, request.agent_id)
        .await
        .map(|_| Json(json!({ "ok": true })))
        .map_err(api_error)
}

async fn api_set_channel_agent_membership(
    State(state): State<Arc<WebState>>,
    Json(request): Json<SetChannelAgentMembershipRequest>,
) -> Result<impl IntoResponse, Response> {
    channel_commands::set_channel_agent_membership(&state.pool, request)
        .await
        .map(|_| Json(json!({ "ok": true })))
        .map_err(api_error)
}

async fn api_update_owner_profile(
    State(state): State<Arc<WebState>>,
    Json(request): Json<OwnerProfileRequest>,
) -> Result<impl IntoResponse, Response> {
    agent_commands::update_owner_profile(&state.pool, request)
        .await
        .map(|_| Json(json!({ "ok": true })))
        .map_err(api_error)
}

async fn api_mark_channel_read(
    State(state): State<Arc<WebState>>,
    Json(request): Json<MarkChannelReadRequest>,
) -> Result<impl IntoResponse, Response> {
    inbox_commands::mark_channel_read(&state.pool, request)
        .await
        .map(|_| Json(json!({ "ok": true })))
        .map_err(api_error)
}

async fn api_dismiss_inbox_items(
    State(state): State<Arc<WebState>>,
    Json(request): Json<InboxItemsRequest>,
) -> Result<impl IntoResponse, Response> {
    inbox_commands::dismiss_inbox_items(&state.pool, request)
        .await
        .map(|_| Json(json!({ "ok": true })))
        .map_err(api_error)
}

async fn api_mark_inbox_items_read(
    State(state): State<Arc<WebState>>,
    Json(request): Json<InboxItemsRequest>,
) -> Result<impl IntoResponse, Response> {
    inbox_commands::mark_inbox_items_read(&state.pool, request)
        .await
        .map(|_| Json(json!({ "ok": true })))
        .map_err(api_error)
}

async fn api_mark_all_inbox_read(
    State(state): State<Arc<WebState>>,
) -> Result<impl IntoResponse, Response> {
    inbox_commands::mark_all_inbox_read(&state.pool)
        .await
        .map(|_| Json(json!({ "ok": true })))
        .map_err(api_error)
}

async fn api_set_message_saved(
    State(state): State<Arc<WebState>>,
    Json(request): Json<SetMessageSavedRequest>,
) -> Result<impl IntoResponse, Response> {
    message_commands::set_message_saved(&state.pool, request)
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
    Json(request): Json<UpdateTaskStatusRequest>,
) -> Result<impl IntoResponse, Response> {
    task_commands::update_task_status(&state.pool, request)
        .await
        .map(|_| Json(json!({ "ok": true })))
        .map_err(api_error)
}

async fn api_update_task_title(
    State(state): State<Arc<WebState>>,
    Json(request): Json<UpdateTaskTitleRequest>,
) -> Result<impl IntoResponse, Response> {
    task_commands::update_task_title(&state.pool, request)
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
    let _ = enqueue_ui_event(
        &state.pool,
        &UiEvent::Refresh {
            reason: "supervisor_service_installed",
        },
    )
    .await;
    Ok(Json(status))
}

async fn api_uninstall_supervisor_service(
    State(state): State<Arc<WebState>>,
) -> Result<impl IntoResponse, Response> {
    let status = launch_agent::uninstall_supervisor_service().map_err(api_error)?;
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(to_string)
        .map_err(api_error)?;
    sqlx::query("update supervisor_state set status = 'offline', updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') where id = 1")
        .execute(&mut *transaction)
        .await
        .map_err(to_string)
        .map_err(api_error)?;
    enqueue_ui_event_in_tx(
        &mut transaction,
        &UiEvent::Refresh {
            reason: "supervisor_service_uninstalled",
        },
    )
    .await
    .map_err(api_error)?;
    transaction
        .commit()
        .await
        .map_err(to_string)
        .map_err(api_error)?;
    Ok(Json(status))
}

async fn api_artifact_read(
    State(state): State<Arc<WebState>>,
    Json(request): Json<ArtifactReadRequest>,
) -> Result<impl IntoResponse, Response> {
    artifact_commands::artifact_read(&state.pool, request)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn api_open_dm_with_agent(
    State(state): State<Arc<WebState>>,
    Json(request): Json<AgentIdRequest>,
) -> Result<impl IntoResponse, Response> {
    channel_commands::open_dm_with_agent(&state.pool, request)
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

fn requested_event_cursor(headers: &HeaderMap, query: &EventsQuery) -> Option<i64> {
    headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|cursor| *cursor >= 0)
        .or(query.cursor.filter(|cursor| *cursor >= 0))
}

async fn event_stream_start(
    pool: &SqlitePool,
    requested_cursor: Option<i64>,
) -> CommandResult<(i64, bool)> {
    let row = sqlx::query(
        "select coalesce(min(id), 0) as min_id, coalesce(max(id), 0) as max_id from ui_events",
    )
    .fetch_one(pool)
    .await
    .map_err(to_string)?;
    let min_id: i64 = row.get("min_id");
    let max_id: i64 = row.get("max_id");
    let Some(cursor) = requested_cursor else {
        return Ok((max_id, false));
    };
    let cursor_fell_behind = min_id > 0 && cursor < min_id.saturating_sub(1);
    let cursor_is_from_another_database = cursor > max_id;
    if cursor_fell_behind || cursor_is_from_another_database {
        Ok((max_id, true))
    } else {
        Ok((cursor, false))
    }
}

async fn api_events(
    State(state): State<Arc<WebState>>,
    Query(query): Query<EventsQuery>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, Response> {
    let (initial_last_id, replay_gap) =
        event_stream_start(&state.pool, requested_event_cursor(&headers, &query))
            .await
            .map_err(api_error)?;
    let pool = state.pool.clone();
    let stream = async_stream::stream! {
        let mut last_id = initial_last_id;
        if replay_gap {
            yield Ok::<Event, Infallible>(
                Event::default()
                    .id(last_id.to_string())
                    .event("lantor")
                    .data(json!({
                        "type": "refresh",
                        "reason": "event_replay_gap"
                    }).to_string())
            );
        }
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
                        yield Ok(
                            Event::default()
                                .id(last_id.to_string())
                                .event("lantor")
                                .data(row.get::<String, _>("event_json"))
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
    // Active content served inline would execute scripts in the web origin;
    // force those types to download instead of rendering.
    let lowered_type = content_type.to_ascii_lowercase();
    let disposition = if ["html", "svg", "xml", "javascript", "ecmascript"]
        .iter()
        .any(|marker| lowered_type.contains(marker))
    {
        "attachment"
    } else {
        "inline"
    };
    let mut response = Response::new(Body::from(bytes));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&content_type)
            .unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!(
            "{disposition}; filename=\"{}\"",
            original_name.replace('"', "")
        ))
        .unwrap_or(HeaderValue::from_static("attachment")),
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

#[cfg(test)]
#[path = "tests/web_static.rs"]
mod static_tests;

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};

    use super::{
        event_stream_start, requested_event_cursor, resolve_web_bind_value, EventsQuery,
        DEFAULT_LANTOR_WEB_BIND,
    };
    use crate::test_support::{drop_test_schema, test_pool};

    #[test]
    fn web_bind_defaults_to_loopback_and_honors_overrides() {
        assert_eq!(
            resolve_web_bind_value(None).as_deref(),
            Some(DEFAULT_LANTOR_WEB_BIND)
        );
        assert_eq!(
            resolve_web_bind_value(Some("  ")).as_deref(),
            Some(DEFAULT_LANTOR_WEB_BIND)
        );
        assert_eq!(
            resolve_web_bind_value(Some("0.0.0.0:9000")).as_deref(),
            Some("0.0.0.0:9000")
        );
        assert_eq!(resolve_web_bind_value(Some("off")), None);
    }

    #[test]
    fn last_event_id_header_takes_precedence_over_query_cursor() {
        let mut headers = HeaderMap::new();
        headers.insert("last-event-id", HeaderValue::from_static("12"));
        let query = EventsQuery { cursor: Some(7) };

        assert_eq!(requested_event_cursor(&headers, &query), Some(12));
    }

    #[tokio::test]
    async fn event_stream_detects_a_pruned_replay_gap() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            for index in 1..=3 {
                sqlx::query("insert into ui_events (event_json) values ($1)")
                    .bind(format!(r#"{{"type":"refresh","reason":"event-{index}"}}"#))
                    .execute(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            }
            let first_id: i64 = sqlx::query_scalar("select min(id) from ui_events")
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
            sqlx::query("delete from ui_events where id <= $1")
                .bind(first_id + 1)
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;

            let (last_id, replay_gap) = event_stream_start(&pool, Some(first_id)).await?;
            let max_id: i64 = sqlx::query_scalar("select max(id) from ui_events")
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
            assert!(replay_gap);
            assert_eq!(last_id, max_id);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }
}
