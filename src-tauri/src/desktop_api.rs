//! The desktop window's API transport.
//!
//! The desktop window calls the same `/api` routes as browsers. Instead of a
//! socket, each request travels over Tauri IPC as raw bytes and is served in
//! process by the web server's router, marked with
//! [`DESKTOP_SURFACE_HEADER`]. Adding an API therefore needs a route in
//! `web.rs` and nothing here.

use axum::{
    body::{to_bytes, Body},
    http::{header, Method, Request},
    Router,
};
use tauri::{
    ipc::{InvokeBody, Response},
    State,
};
use tower::ServiceExt;

use crate::app::{to_string, CommandResult};
use crate::web::DESKTOP_SURFACE_HEADER;

/// Headers carrying the method, path and content type of a forwarded
/// request. Tauri sets the IPC request's own `Content-Type` to describe the
/// raw body, so the API request's content type needs its own header.
const METHOD_HEADER: &str = "lantor-method";
const PATH_HEADER: &str = "lantor-path";
const CONTENT_TYPE_HEADER: &str = "lantor-content-type";

/// The `/api` router the desktop window's requests are served by.
pub(crate) struct DesktopApi(pub(crate) Router);

/// Serves one API request from the desktop window and returns the response
/// body. A failed request returns its error message.
#[tauri::command]
pub(crate) async fn api_request(
    request: tauri::ipc::Request<'_>,
    api: State<'_, DesktopApi>,
) -> CommandResult<Response> {
    let header = |name: &str| {
        request
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
    };
    let method = header(METHOD_HEADER).unwrap_or("POST").to_owned();
    let path = header(PATH_HEADER)
        .ok_or_else(|| format!("missing {PATH_HEADER} header"))?
        .to_owned();
    let content_type = header(CONTENT_TYPE_HEADER).map(str::to_owned);
    let body = request_body_bytes(request.body())?;
    forward(&api.0, &method, &path, content_type.as_deref(), body)
        .await
        .map(Response::new)
}

/// The raw request body. When the webview cannot use Tauri's IPC protocol,
/// its postMessage fallback delivers the bytes as a JSON array of numbers.
fn request_body_bytes(body: &InvokeBody) -> CommandResult<Vec<u8>> {
    match body {
        InvokeBody::Raw(bytes) => Ok(bytes.clone()),
        InvokeBody::Json(serde_json::Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_u64()
                    .and_then(|byte| u8::try_from(byte).ok())
                    .ok_or_else(|| "request body must be bytes".to_owned())
            })
            .collect(),
        InvokeBody::Json(serde_json::Value::Null) => Ok(Vec::new()),
        InvokeBody::Json(_) => Err("request body must be bytes".to_owned()),
    }
}

/// Runs a request through `router` as the desktop surface. Only `/api` paths
/// are accepted.
pub(crate) async fn forward(
    router: &Router,
    method: &str,
    path: &str,
    content_type: Option<&str>,
    body: Vec<u8>,
) -> CommandResult<Vec<u8>> {
    if !path.starts_with("/api/") {
        return Err(format!("not an API path: {path}"));
    }
    let method = Method::from_bytes(method.as_bytes()).map_err(to_string)?;
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(DESKTOP_SURFACE_HEADER, "desktop")
        .header(header::CONTENT_LENGTH, body.len());
    if let Some(content_type) = content_type.filter(|value| !value.is_empty()) {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    let request = builder.body(Body::from(body)).map_err(to_string)?;
    let response = router.clone().oneshot(request).await.map_err(to_string)?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .map_err(to_string)?;
    if status.is_success() {
        return Ok(bytes.to_vec());
    }
    Err(error_message(&bytes).unwrap_or_else(|| status.to_string()))
}

/// The `message` of an API error body, or the body itself when it is text.
fn error_message(body: &[u8]) -> Option<String> {
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) {
        if let Some(message) = value.get("message").and_then(|message| message.as_str()) {
            return Some(message.to_owned());
        }
    }
    let text = String::from_utf8_lossy(body).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
#[path = "tests/desktop_api.rs"]
mod tests;
