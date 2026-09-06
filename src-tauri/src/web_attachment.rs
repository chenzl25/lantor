use std::path::Path;

use axum::{
    body::Body,
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::Response,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;
use uuid::Uuid;

const IMMUTABLE_CACHE: &str = "private, max-age=31536000, immutable";

// Unsupported units and multipart ranges fall back to the full representation.
// Malformed/unsatisfiable single byte ranges receive 416.
fn single_range(value: &str, size: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(value) = value.strip_prefix("bytes=") else {
        return Ok(None);
    };
    if value.contains(',') {
        return Ok(None);
    }
    let (start, end) = value.trim().split_once('-').ok_or(())?;
    let decimal = |value: &str| -> Result<u64, ()> {
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(());
        }
        value.parse().map_err(|_| ())
    };
    if size == 0 {
        return Err(());
    }
    if start.is_empty() {
        let suffix = decimal(end)?;
        if suffix == 0 {
            return Err(());
        }
        return Ok(Some((size.saturating_sub(suffix), size - 1)));
    }
    let start = decimal(start)?;
    let end = if end.is_empty() {
        size - 1
    } else {
        decimal(end)?.min(size - 1)
    };
    if start >= size || start > end {
        return Err(());
    }
    Ok(Some((start, end)))
}

pub(super) async fn serve_attachment(
    id: Uuid,
    original_name: &str,
    mime_type: &str,
    path: &Path,
    method: &Method,
    headers: &HeaderMap,
) -> Result<Response, Response> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|err| super::api_error(err.to_string()))?;
    let size = file
        .metadata()
        .await
        .map_err(|err| super::api_error(err.to_string()))?
        .len();
    let etag = format!("\"{id}\"");
    let mut response = Response::new(Body::empty());
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(IMMUTABLE_CACHE),
    );
    response
        .headers_mut()
        .insert(header::ETAG, HeaderValue::from_str(&etag).unwrap());
    response
        .headers_mut()
        .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .any(|value| value == "*" || value.trim_start_matches("W/") == etag)
    {
        *response.status_mut() = StatusCode::NOT_MODIFIED;
        return Ok(response);
    }
    let content_type = if mime_type.trim().is_empty() {
        mime_guess::from_path(path)
            .first_or_octet_stream()
            .to_string()
    } else {
        mime_type.to_owned()
    };
    // Preserve the existing download policy for active content in our origin.
    let lowered_type = content_type.to_ascii_lowercase();
    let disposition = if ["html", "svg", "xml", "javascript", "ecmascript"]
        .iter()
        .any(|marker| lowered_type.contains(marker))
    {
        "attachment"
    } else {
        "inline"
    };
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

    // Range applies only to GET. If-Range requires this representation's strong
    // validator; stale, weak, or date validators yield the entire file.
    let range = if method == Method::GET
        && headers
            .get(header::IF_RANGE)
            .is_none_or(|value| value.as_bytes() == etag.as_bytes())
    {
        headers
            .get(header::RANGE)
            .and_then(|value| value.to_str().ok())
            .map(|value| single_range(value, size))
            .transpose()
    } else {
        Ok(None)
    };
    let range = match range {
        Ok(range) => range.flatten(),
        Err(()) => {
            *response.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
            // Do not cache an error as the immutable attachment representation.
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{size}")).unwrap(),
            );
            response
                .headers_mut()
                .insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
            return Ok(response);
        }
    };
    let length = if let Some((start, end)) = range {
        file.seek(std::io::SeekFrom::Start(start))
            .await
            .map_err(|err| super::api_error(err.to_string()))?;
        *response.status_mut() = StatusCode::PARTIAL_CONTENT;
        response.headers_mut().insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end}/{size}")).unwrap(),
        );
        end - start + 1
    } else {
        size
    };
    response
        .headers_mut()
        .insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    if method != Method::HEAD {
        *response.body_mut() =
            Body::from_stream(ReaderStream::with_capacity(file.take(length), 64 * 1024));
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[tokio::test]
    async fn streamed_attachment_ranges_validators_and_disposition() {
        let path = std::env::temp_dir().join(format!("lantor-range-{}", Uuid::new_v4()));
        std::fs::write(&path, b"0123456789").unwrap();
        let id = Uuid::new_v4();
        let etag = format!("\"{id}\"");
        for (range, if_range, status, content_range, expected) in [
            (None, None, 200, None, "0123456789"),
            (Some("bytes=0-3"), None, 206, Some("bytes 0-3/10"), "0123"),
            (Some("bytes=7-"), None, 206, Some("bytes 7-9/10"), "789"),
            (Some("bytes=-3"), None, 206, Some("bytes 7-9/10"), "789"),
            (Some("bytes=8-999"), None, 206, Some("bytes 8-9/10"), "89"),
            (
                Some("bytes=-99"),
                None,
                206,
                Some("bytes 0-9/10"),
                "0123456789",
            ),
            (Some("bytes=10-"), None, 416, Some("bytes */10"), ""),
            (Some("bytes=4-2"), None, 416, Some("bytes */10"), ""),
            (Some("bytes=-0"), None, 416, Some("bytes */10"), ""),
            (Some("bytes=oops"), None, 416, Some("bytes */10"), ""),
            (Some("bytes=0-1,3-4"), None, 200, None, "0123456789"),
            (Some("items=0-1"), None, 200, None, "0123456789"),
            (
                Some("bytes=0-1"),
                Some("\"stale\""),
                200,
                None,
                "0123456789",
            ),
            (
                Some("bytes=0-1"),
                Some(etag.as_str()),
                206,
                Some("bytes 0-1/10"),
                "01",
            ),
        ] {
            let mut headers = HeaderMap::new();
            if let Some(range) = range {
                headers.insert(header::RANGE, HeaderValue::from_str(range).unwrap());
            }
            if let Some(value) = if_range {
                headers.insert(header::IF_RANGE, HeaderValue::from_str(value).unwrap());
            }
            let response =
                serve_attachment(id, "probe.html", "text/html", &path, &Method::GET, &headers)
                    .await
                    .unwrap();
            assert_eq!(response.status().as_u16(), status, "{range:?}");
            assert_eq!(
                response
                    .headers()
                    .get(header::CONTENT_RANGE)
                    .map(|value| value.to_str().unwrap()),
                content_range
            );
            assert_eq!(response.headers()[header::ETAG], etag);
            assert_eq!(response.headers()[header::ACCEPT_RANGES], "bytes");
            assert_eq!(
                response.headers()[header::CACHE_CONTROL],
                if status == 416 {
                    "no-store"
                } else {
                    IMMUTABLE_CACHE
                }
            );
            assert_eq!(
                response.headers()[header::CONTENT_DISPOSITION],
                "attachment; filename=\"probe.html\""
            );
            assert_eq!(to_bytes(response.into_body(), 100).await.unwrap(), expected);
        }
        let mut headers = HeaderMap::new();
        headers.insert(
            header::IF_NONE_MATCH,
            HeaderValue::from_str(&format!("\"other\", W/{etag}")).unwrap(),
        );
        let response = serve_attachment(id, "p.png", "image/png", &path, &Method::GET, &headers)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert!(to_bytes(response.into_body(), 100)
            .await
            .unwrap()
            .is_empty());
        headers.clear();
        headers.insert(header::RANGE, HeaderValue::from_static("bytes=0-1"));
        let response = serve_attachment(id, "p.png", "image/png", &path, &Method::HEAD, &headers)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_LENGTH], "10");
        assert_eq!(
            response.headers()[header::CONTENT_DISPOSITION],
            "inline; filename=\"p.png\""
        );
        assert!(to_bytes(response.into_body(), 100)
            .await
            .unwrap()
            .is_empty());
        std::fs::write(&path, b"").unwrap();
        let response = serve_attachment(id, "p.png", "image/png", &path, &Method::GET, &headers)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes */0");
        std::fs::remove_file(path).unwrap();
    }
}
