use std::path::Path;

use axum::{
    body::Body,
    extract::{multipart::MultipartError, Multipart, Request},
    http::StatusCode,
};
use http_body_util::BodyExt;
use tokio::io::AsyncWriteExt;

use crate::{
    application::messages::SendMessageRequest,
    attachments::{
        attachment_exceeds_size_limit, attachment_root_dir, StagedAttachment,
        ATTACHMENT_SIZE_LIMIT_MIB,
    },
    models::AttachmentUpload,
};

const REQUEST_METADATA_LIMIT: usize = 1024 * 1024;

pub(crate) fn bounded_multipart_body(request: Request) -> Request {
    let (parts, mut body) = request.into_parts();
    let stream = async_stream::stream! {
        while let Some(frame) = body.frame().await {
            let frame = match frame { Ok(frame) => frame, Err(error) => { yield Err(error); return; } };
            if let Ok(bytes) = frame.into_data() {
                for offset in (0..bytes.len()).step_by(64 * 1024) {
                    yield Ok(bytes.slice(offset..(offset + 64 * 1024).min(bytes.len())));
                    // Multer drains all immediately-ready body chunks into its
                    // buffer before exposing a field chunk. Yield between chunks
                    // so a fast sender cannot make it accumulate the whole file.
                    tokio::task::yield_now().await;
                }
            }
        }
    };
    Request::from_parts(parts, Body::from_stream(stream))
}

#[derive(Debug)]
pub(crate) struct UploadError {
    pub(crate) status: StatusCode,
    pub(crate) message: String,
}

impl UploadError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }
    fn too_large(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            message: message.into(),
        }
    }
    fn storage(message: impl ToString) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.to_string(),
        }
    }
}

impl From<MultipartError> for UploadError {
    fn from(error: MultipartError) -> Self {
        Self {
            status: error.status(),
            message: error.to_string(),
        }
    }
}

pub(crate) async fn parse_multipart_send_message(
    multipart: Multipart,
) -> Result<SendMessageRequest, UploadError> {
    let root = attachment_root_dir().map_err(UploadError::storage)?;
    parse_multipart_in(multipart, &root).await
}

async fn parse_multipart_in(
    mut multipart: Multipart,
    root: &Path,
) -> Result<SendMessageRequest, UploadError> {
    let mut request = None;
    let mut attachments = Vec::new();
    while let Some(mut field) = multipart.next_field().await? {
        let field_name = field.name().unwrap_or_default().to_owned();
        match field_name.as_str() {
            "request" => {
                if request.is_some() {
                    return Err(UploadError::invalid(
                        "multipart send_message contains duplicate request fields",
                    ));
                }
                let mut metadata = Vec::new();
                while let Some(chunk) = field.chunk().await? {
                    if chunk.len() > REQUEST_METADATA_LIMIT - metadata.len() {
                        return Err(UploadError::too_large("send_message metadata exceeds 1MiB"));
                    }
                    metadata.extend_from_slice(&chunk);
                }
                request = Some(
                    serde_json::from_slice::<SendMessageRequest>(&metadata).map_err(|err| {
                        UploadError::invalid(format!(
                            "invalid send_message request metadata: {err}"
                        ))
                    })?,
                );
            }
            "attachments" => {
                let original_name = field
                    .file_name()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or("attachment")
                    .to_owned();
                let mime_type = field
                    .content_type()
                    .map(ToString::to_string)
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| "application/octet-stream".to_owned());
                let (mut staged, mut file) = StagedAttachment::create(root)
                    .await
                    .map_err(UploadError::storage)?;
                while let Some(chunk) = field.chunk().await? {
                    staged.size_bytes += chunk.len() as u64;
                    if attachment_exceeds_size_limit(staged.size_bytes) {
                        return Err(UploadError::too_large(format!("attachment {original_name} is larger than {ATTACHMENT_SIZE_LIMIT_MIB}MB")));
                    }
                    file.write_all(&chunk).await.map_err(UploadError::storage)?;
                }
                // Complete buffered Tokio writes before the storage layer renames.
                file.flush().await.map_err(UploadError::storage)?;
                drop(file);
                attachments.push(AttachmentUpload {
                    original_name,
                    mime_type,
                    bytes: Vec::new(),
                    staged: Some(staged),
                });
            }
            _ => {
                return Err(UploadError::invalid(format!(
                    "multipart send_message contains unknown field {field_name}"
                )))
            }
        }
    }
    let mut request = request.ok_or_else(|| {
        UploadError::invalid("multipart send_message is missing the request metadata field")
    })?;
    if request
        .attachments
        .as_ref()
        .is_some_and(|attachments| !attachments.is_empty())
    {
        return Err(UploadError::invalid(
            "multipart send_message request metadata must not contain attachments",
        ));
    }
    request.attachments = Some(attachments);
    Ok(request)
}

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        extract::{FromRequest, Multipart},
        http::Request,
    };
    use uuid::Uuid;

    use super::parse_multipart_in;

    #[tokio::test]
    async fn multipart_send_message_preserves_binary_attachments() {
        let boundary = "lantor-attachment-boundary";
        let channel_id = Uuid::new_v4();
        let metadata = format!(
            r#"{{"channelId":"{channel_id}","threadRootId":null,"body":"hello","asTask":false}}"#
        );
        let mut body = format!(
            "--{boundary}\r\n\
             Content-Disposition: form-data; name=\"request\"\r\n\
             Content-Type: application/json\r\n\r\n\
             {metadata}\r\n\
             --{boundary}\r\n\
             Content-Disposition: form-data; name=\"attachments\"; filename=\"probe.bin\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n"
        )
        .into_bytes();
        body.extend_from_slice(&[0, 1, 254, 255]);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let request = Request::builder()
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .unwrap();

        let multipart = Multipart::from_request(request, &()).await.unwrap();
        let root = std::env::temp_dir().join(format!("lantor-upload-test-{}", Uuid::new_v4()));
        let request = parse_multipart_in(multipart, &root).await.unwrap();

        assert_eq!(request.channel_id, channel_id);
        assert_eq!(request.thread_root_id, None);
        assert_eq!(request.body, "hello");
        assert!(!request.as_task);
        let attachments = request.attachments.unwrap();
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].original_name, "probe.bin");
        assert_eq!(attachments[0].mime_type, "application/octet-stream");
        assert!(attachments[0].bytes.is_empty());
        let staged = attachments[0].staged.as_ref().unwrap().path.clone();
        assert_eq!(std::fs::read(&staged).unwrap(), [0, 1, 254, 255]);
        assert_eq!(attachments[0].size_bytes(), 4);
        drop(attachments);
        assert!(!staged.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod streaming_tests {
    use super::*;
    use axum::{
        body::{Body, Bytes},
        extract::DefaultBodyLimit,
        http::Request,
        response::{IntoResponse, Response},
        routing::post,
        Router,
    };
    use std::{
        path::PathBuf,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        time::Duration,
    };
    use tower::ServiceExt;
    use uuid::Uuid;

    fn prefix() -> String {
        "--test\r\nContent-Disposition: form-data; name=\"attachments\"; filename=\"probe.bin\"\r\nContent-Type: application/octet-stream\r\n\r\n".to_owned()
    }

    async fn parse_fixture(body: Body, root: PathBuf) -> Response {
        Router::new()
            .route(
                "/",
                post(move |request: axum::extract::Request| async move {
                    let multipart = <Multipart as axum::extract::FromRequest<()>>::from_request(
                        bounded_multipart_body(request),
                        &(),
                    )
                    .await
                    .unwrap();
                    match parse_multipart_in(multipart, &root).await {
                        Ok(request) => {
                            let attachments = request.attachments.unwrap();
                            assert!(attachments.iter().all(|item| item.bytes.is_empty()));
                            (
                                StatusCode::OK,
                                attachments
                                    .iter()
                                    .map(AttachmentUpload::size_bytes)
                                    .sum::<u64>()
                                    .to_string(),
                            )
                                .into_response()
                        }
                        Err(error) => (error.status, error.message).into_response(),
                    }
                }),
            )
            .layer(DefaultBodyLimit::max(128 * 1024 * 1024))
            .oneshot(
                Request::post("/")
                    .header("content-type", "multipart/form-data; boundary=test")
                    .body(body)
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    fn assert_clean(root: &Path) {
        assert_eq!(std::fs::read_dir(root.join(".tmp")).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn unknown_length_upload_is_bounded_and_cleans_up_at_limit() {
        let root = std::env::temp_dir().join(format!("lantor-upload-limit-{}", Uuid::new_v4()));
        let polled = Arc::new(AtomicUsize::new(0));
        let counter = polled.clone();
        let body = Body::from_stream(async_stream::stream! {
            yield Ok::<_, std::io::Error>(Bytes::from(prefix()));
            let chunk = Bytes::from(vec![1; 64 * 1024]);
            for _ in 0..1600 {
                counter.fetch_add(1, Ordering::SeqCst);
                yield Ok(chunk.clone());
            }
            yield Ok(Bytes::from_static(b"\r\n--test--\r\n"));
        });
        let response = parse_fixture(body, root.clone()).await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(
            polled.load(Ordering::SeqCst) <= 1026,
            "must stop near 64MiB instead of draining 100MiB"
        );
        assert_clean(&root);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn exact_limit_file_is_staged_without_buffering_and_metadata_errors_clean_up() {
        let root = std::env::temp_dir().join(format!("lantor-upload-exact-{}", Uuid::new_v4()));
        let body = Body::from_stream(async_stream::stream! {
            yield Ok::<_, std::io::Error>(Bytes::from(prefix()));
            let chunk = Bytes::from(vec![1; 64 * 1024]);
            for _ in 0..1024 { yield Ok(chunk.clone()); }
            yield Ok(Bytes::from(format!("\r\n--test\r\nContent-Disposition: form-data; name=\"request\"\r\n\r\n{{\"channelId\":\"{}\",\"body\":\"test\",\"asTask\":false}}\r\n--test--\r\n", Uuid::new_v4())));
        });
        let response = parse_fixture(body, root.clone()).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            axum::body::to_bytes(response.into_body(), 100)
                .await
                .unwrap(),
            "67108864"
        );
        assert_clean(&root);
        // A prior successful file must also be cleaned if trailing metadata is invalid.
        let body = Body::from(format!("{}data\r\n--test\r\nContent-Disposition: form-data; name=\"request\"\r\n\r\ninvalid json\r\n--test--\r\n", prefix()));
        assert_eq!(
            parse_fixture(body, root.clone()).await.status(),
            StatusCode::BAD_REQUEST
        );
        assert_clean(&root);
        // JSON cannot inject server-side paths into the internal staging field.
        let upload: AttachmentUpload = serde_json::from_value(serde_json::json!({
            "originalName":"x", "mimeType":"text/plain", "bytes":[],
            "staged":{"path":"/private/file", "size_bytes":10}
        }))
        .unwrap();
        assert!(upload.staged.is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn body_failure_and_cancelled_upload_remove_temporary_files() {
        let root = std::env::temp_dir().join(format!("lantor-upload-abort-{}", Uuid::new_v4()));
        let body = Body::from_stream(async_stream::stream! {
            yield Ok(Bytes::from(prefix()));
            yield Ok(Bytes::from(vec![1; 64 * 1024]));
            yield Err::<Bytes, _>(std::io::Error::new(std::io::ErrorKind::ConnectionReset, "client disconnected"));
        });
        assert_eq!(
            parse_fixture(body, root.clone()).await.status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_clean(&root);
        let body = Body::from_stream(async_stream::stream! {
            yield Ok::<_, std::io::Error>(Bytes::from(prefix()));
            yield Ok(Bytes::from(vec![1; 64 * 1024]));
            std::future::pending::<()>().await;
        });
        let task = tokio::spawn(parse_fixture(body, root.clone()));
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if std::fs::read_dir(root.join(".tmp"))
                    .unwrap()
                    .any(|entry| entry.unwrap().metadata().unwrap().len() > 0)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_clean(&root);
        std::fs::remove_dir_all(root).unwrap();
    }
}
