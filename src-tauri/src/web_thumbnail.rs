//! Bounded, on-demand derivatives. Originals and downloads retain their URL and
//! byte-range contract; no derivative files outlive attachment ownership.
use std::{
    collections::VecDeque,
    io::Cursor,
    path::Path,
    sync::{Mutex, OnceLock},
};

use axum::{
    body::{Body, Bytes},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::Response,
};
use image::{ImageDecoder, ImageReader};
use tokio::sync::Semaphore;
use uuid::Uuid;

const WIDTH: u32 = 480;
const MAX_SOURCE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_CACHE_BYTES: usize = 8 * 1024 * 1024;
type Cache = VecDeque<(Uuid, Bytes)>;
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
static DECODERS: Semaphore = Semaphore::const_new(2);

fn cached(id: Uuid) -> Option<Bytes> {
    let mut cache = CACHE.get_or_init(Default::default).lock().ok()?;
    let index = cache.iter().position(|(key, _)| *key == id)?;
    let entry = cache.remove(index)?;
    let bytes = entry.1.clone();
    cache.push_back(entry);
    Some(bytes)
}

fn remember(id: Uuid, bytes: Bytes) {
    let Ok(mut cache) = CACHE.get_or_init(Default::default).lock() else {
        return;
    };
    cache.retain(|(key, _)| *key != id);
    cache.push_back((id, bytes));
    while cache.len() > 32
        || cache.iter().map(|(_, bytes)| bytes.len()).sum::<usize>() > MAX_CACHE_BYTES
    {
        cache.pop_front();
    }
}

fn decode(path: &Path) -> Option<Bytes> {
    if path.metadata().ok()?.len() > MAX_SOURCE_BYTES {
        return None;
    }
    let mut reader = ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(96 * 1024 * 1024);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().ok()?;
    let (width, height) = decoder.dimensions();
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return None;
    }
    let orientation = decoder.orientation().ok()?;
    let mut image = image::DynamicImage::from_decoder(decoder).ok()?;
    image.apply_orientation(orientation);
    // Bound both dimensions, including very tall screenshots. Never upscale.
    let image = if image.width() > WIDTH || image.height() > WIDTH {
        image.thumbnail(WIDTH, WIDTH)
    } else {
        image
    };
    let mut output = Cursor::new(Vec::new());
    image.write_to(&mut output, image::ImageFormat::WebP).ok()?;
    let bytes = output.into_inner();
    (bytes.len() <= 1024 * 1024).then(|| Bytes::from(bytes))
}

pub(super) async fn serve_thumbnail(
    id: Uuid,
    mime: &str,
    path: &Path,
    method: &Method,
    headers: &HeaderMap,
) -> Option<Response> {
    // Keep GIF animation and never serve active SVG/HTML as an inline image.
    if !matches!(mime, "image/png" | "image/jpeg" | "image/webp") {
        return None;
    }
    let bytes = if let Some(bytes) = cached(id) {
        bytes
    } else {
        // On overload the caller streams the original, without allocating an
        // unbounded queue of expensive decodes or blocking async worker threads.
        let permit = DECODERS.try_acquire().ok()?;
        let path = path.to_owned();
        let bytes = tokio::task::spawn_blocking(move || {
            // Request cancellation must not release capacity while the CPU
            // worker continues decoding in the background.
            let _permit = permit;
            decode(&path)
        })
        .await
        .ok()??;
        remember(id, bytes.clone());
        bytes
    };
    let etag = format!("\"{id}-thumb-480-v1\"");
    let mut response = Response::new(Body::empty());
    let h = response.headers_mut();
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=31536000, immutable"),
    );
    h.insert(header::ETAG, HeaderValue::from_str(&etag).ok()?);
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/webp"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|h| h.to_str().ok())
        .flat_map(|h| h.split(','))
        .map(str::trim)
        .any(|h| h == "*" || h.trim_start_matches("W/") == etag)
    {
        *response.status_mut() = StatusCode::NOT_MODIFIED;
    } else {
        response
            .headers_mut()
            .insert(header::CONTENT_LENGTH, HeaderValue::from(bytes.len()));
        if method != Method::HEAD {
            *response.body_mut() = Body::from(bytes);
        }
    }
    Some(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_oversized_and_large_dimension_sources_do_not_decode() {
        let path =
            std::env::temp_dir().join(format!("lantor-thumbnail-bounds-{}.png", Uuid::new_v4()));
        std::fs::write(&path, b"not an image").unwrap();
        assert!(decode(&path).is_none());
        std::fs::File::create(&path)
            .unwrap()
            .set_len(MAX_SOURCE_BYTES + 1)
            .unwrap();
        assert!(decode(&path).is_none());
        image::RgbaImage::new(8193, 1).save(&path).unwrap();
        assert!(decode(&path).is_none());
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn thumbnail_bounds_cache_head_and_validator_are_distinct_from_original() {
        let id = Uuid::new_v4();
        let path = std::env::temp_dir().join(format!("lantor-thumbnail-{id}.png"));
        image::RgbaImage::from_pixel(1600, 800, image::Rgba([30, 50, 80, 255]))
            .save(&path)
            .unwrap();
        let response = serve_thumbnail(id, "image/png", &path, &Method::GET, &HeaderMap::new())
            .await
            .unwrap();
        let etag = response.headers()[header::ETAG].clone();
        assert_ne!(etag, format!("\"{id}\""));
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (480, 240));
        let head = serve_thumbnail(id, "image/png", &path, &Method::HEAD, &HeaderMap::new())
            .await
            .unwrap();
        assert_eq!(
            head.headers()[header::CONTENT_LENGTH],
            bytes.len().to_string()
        );
        assert!(axum::body::to_bytes(head.into_body(), 1)
            .await
            .unwrap()
            .is_empty());
        let mut headers = HeaderMap::new();
        headers.insert(header::IF_NONE_MATCH, etag);
        let response = serve_thumbnail(id, "image/png", &path, &Method::GET, &headers)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert!(
            serve_thumbnail(id, "image/svg+xml", &path, &Method::GET, &headers)
                .await
                .is_none()
        );
        assert!(
            serve_thumbnail(id, "image/gif", &path, &Method::GET, &headers)
                .await
                .is_none()
        );
        std::fs::remove_file(path).unwrap();
    }
}
