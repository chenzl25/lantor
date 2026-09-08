//! Web-only avatar delivery.
//!
//! Uploaded avatars are stored as `data:image/...;base64,...` strings. Inlining
//! them in every bootstrap / agents payload costs 100KB+ of incompressible
//! base64 per avatar on a phone, so the web surface rewrites them into
//! immutable `/api/avatars/...?v=<digest>` URLs and serves the decoded bytes
//! from here. The desktop surface keeps the data URLs untouched.

use axum::{
    body::Body,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::models::Bootstrap;

pub(crate) const AVATAR_URL_PREFIX: &str = "/api/avatars/";
const IMMUTABLE_CACHE: &str = "private, max-age=31536000, immutable";

pub(crate) struct DecodedAvatar {
    pub(crate) mime: String,
    pub(crate) bytes: Vec<u8>,
}

/// Only `data:image/<subtype>;base64,<payload>` avatars are rewritten; emoji,
/// initials, DiceBear specs and external URLs stay inline.
pub(crate) fn is_inline_image_avatar(avatar: &str) -> bool {
    parse_data_url(avatar).is_some()
}

/// Web clients echo the rewritten URL back when they save a profile form
/// without touching the avatar; the stored value must then be left alone.
pub(crate) fn is_avatar_url(avatar: &str) -> bool {
    avatar.trim_start().starts_with(AVATAR_URL_PREFIX)
}

pub(crate) fn avatar_version(avatar: &str) -> String {
    Sha256::digest(avatar.as_bytes())
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn owner_avatar_url(avatar: &str) -> String {
    format!("{AVATAR_URL_PREFIX}owner?v={}", avatar_version(avatar))
}

pub(crate) fn agent_avatar_url(agent_id: Uuid, avatar: &str) -> String {
    format!(
        "{AVATAR_URL_PREFIX}agents/{agent_id}?v={}",
        avatar_version(avatar)
    )
}

pub(crate) fn web_owner_avatar(avatar: &str) -> String {
    if is_inline_image_avatar(avatar) {
        owner_avatar_url(avatar)
    } else {
        avatar.to_owned()
    }
}

pub(crate) fn web_agent_avatar(agent_id: Uuid, avatar: &str) -> String {
    if is_inline_image_avatar(avatar) {
        agent_avatar_url(agent_id, avatar)
    } else {
        avatar.to_owned()
    }
}

pub(crate) fn rewrite_bootstrap(bootstrap: &mut Bootstrap) {
    let owner_avatar = web_owner_avatar(&bootstrap.owner_profile.avatar);
    bootstrap.owner_profile.avatar = owner_avatar;
    for agent in &mut bootstrap.agents {
        let avatar = web_agent_avatar(agent.id, &agent.avatar);
        agent.avatar = avatar;
    }
}

/// Rewrites `owner_profile`, `agents` and `agent` entries inside a serialized
/// payload (UI state patches, agent detail).
pub(crate) fn rewrite_avatars(value: &mut Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    if let Some(profile) = object.get_mut("owner_profile") {
        rewrite_owner(profile);
    }
    if let Some(agents) = object.get_mut("agents").and_then(Value::as_array_mut) {
        for agent in agents {
            rewrite_agent(agent);
        }
    }
    if let Some(agent) = object.get_mut("agent") {
        rewrite_agent(agent);
    }
}

fn rewrite_owner(profile: &mut Value) {
    if let Some(Value::String(avatar)) = profile.get_mut("avatar") {
        if is_inline_image_avatar(avatar) {
            *avatar = owner_avatar_url(avatar);
        }
    }
}

fn rewrite_agent(agent: &mut Value) {
    let Some(id) = agent
        .get("id")
        .and_then(Value::as_str)
        .and_then(|id| Uuid::parse_str(id).ok())
    else {
        return;
    };
    if let Some(Value::String(avatar)) = agent.get_mut("avatar") {
        if is_inline_image_avatar(avatar) {
            *avatar = agent_avatar_url(id, avatar);
        }
    }
}

fn parse_data_url(avatar: &str) -> Option<(&str, &str)> {
    let rest = avatar.strip_prefix("data:")?;
    let (meta, payload) = rest.split_once(',')?;
    let mut params = meta.split(';');
    let mime = params.next()?.trim();
    let valid_mime = mime.starts_with("image/")
        && mime.len() <= 64
        && mime
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'+' | b'-' | b'.'));
    if !valid_mime {
        return None;
    }
    params
        .any(|param| param.trim().eq_ignore_ascii_case("base64"))
        .then_some((mime, payload))
}

pub(crate) fn decode_inline_avatar(avatar: &str) -> Option<DecodedAvatar> {
    let (mime, payload) = parse_data_url(avatar)?;
    Some(DecodedAvatar {
        mime: mime.to_owned(),
        bytes: decode_base64(payload)?,
    })
}

// Accepts the standard and URL-safe alphabets, ignores whitespace and stops at
// padding; anything else means the stored value is not an image we can serve.
fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let mut output = Vec::with_capacity(input.len() / 4 * 3);
    let mut buffer = 0u32;
    let mut bits = 0u8;
    for byte in input.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(output)
}

fn not_found(message: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "ok": false, "message": message })),
    )
        .into_response()
}

/// Serves the decoded avatar bytes. The URL carries a content digest, so the
/// response is immutable and revalidates through the same digest as ETag.
pub(crate) fn avatar_response(avatar: &str, headers: &HeaderMap) -> Response {
    let Some(decoded) = decode_inline_avatar(avatar) else {
        return not_found("avatar is not an inline image");
    };
    let etag = format!("\"{}\"", avatar_version(avatar));
    let not_modified = headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .any(|value| value == "*" || value.trim_start_matches("W/") == etag);
    let mut response = Response::new(if not_modified {
        Body::empty()
    } else {
        Body::from(decoded.bytes)
    });
    if not_modified {
        *response.status_mut() = StatusCode::NOT_MODIFIED;
    }
    let response_headers = response.headers_mut();
    response_headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(IMMUTABLE_CACHE),
    );
    if let Ok(etag) = HeaderValue::from_str(&etag) {
        response_headers.insert(header::ETAG, etag);
    }
    response_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&decoded.mime)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    response_headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_AVATAR: &str = "data:image/png;base64,iVBORw0KGgo=";

    #[test]
    fn only_inline_base64_images_are_rewritten() {
        assert!(is_inline_image_avatar(PNG_AVATAR));
        assert!(is_inline_image_avatar(
            "data:image/jpeg;charset=utf-8;base64,/9j/4AA"
        ));
        assert!(!is_inline_image_avatar("data:image/svg+xml,%3Csvg%3E"));
        assert!(!is_inline_image_avatar("data:text/plain;base64,aGk="));
        assert!(!is_inline_image_avatar("dicebear:dylan:owner"));
        assert!(!is_inline_image_avatar("https://example.com/a.png"));
        assert!(!is_inline_image_avatar("M"));
    }

    #[test]
    fn urls_carry_a_stable_content_digest() {
        let agent_id = Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let owner = web_owner_avatar(PNG_AVATAR);
        let agent = web_agent_avatar(agent_id, PNG_AVATAR);
        assert_eq!(
            owner,
            format!("/api/avatars/owner?v={}", avatar_version(PNG_AVATAR))
        );
        assert_eq!(
            agent,
            format!(
                "/api/avatars/agents/{agent_id}?v={}",
                avatar_version(PNG_AVATAR)
            )
        );
        assert_eq!(avatar_version(PNG_AVATAR).len(), 16);
        assert_ne!(
            avatar_version(PNG_AVATAR),
            avatar_version("data:image/png;base64,iVBORw0KGgp=")
        );
        assert!(is_avatar_url(&owner));
        assert!(is_avatar_url(&agent));
        assert!(!is_avatar_url(PNG_AVATAR));
        assert_eq!(
            web_owner_avatar("dicebear:dylan:owner"),
            "dicebear:dylan:owner"
        );
        assert_eq!(web_agent_avatar(agent_id, "🤖"), "🤖");
    }

    #[test]
    fn decodes_standard_and_url_safe_base64() {
        let decoded = decode_inline_avatar(PNG_AVATAR).unwrap();
        assert_eq!(decoded.mime, "image/png");
        assert_eq!(
            decoded.bytes,
            [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']
        );
        assert_eq!(decode_base64("-_8=").unwrap(), [0xfb, 0xff]);
        assert_eq!(decode_base64("+/8=").unwrap(), [0xfb, 0xff]);
        assert_eq!(decode_base64("aGVs\nbG8").unwrap(), b"hello");
        assert!(decode_base64("aGV$").is_none());
        assert!(decode_inline_avatar("data:image/png;base64,")
            .unwrap()
            .bytes
            .is_empty());
    }

    #[test]
    fn rewrites_serialized_payload_entries() {
        let agent_id = Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let mut patch = json!({
            "owner_profile": { "display_name": "Me", "avatar": PNG_AVATAR },
            "agents": [
                { "id": agent_id.to_string(), "avatar": PNG_AVATAR },
                { "id": agent_id.to_string(), "avatar": "dicebear:dylan:x" },
                { "avatar": PNG_AVATAR }
            ],
            "agent": { "id": agent_id.to_string(), "avatar": PNG_AVATAR },
            "channels": [{ "avatar": PNG_AVATAR }]
        });
        rewrite_avatars(&mut patch);
        assert_eq!(
            patch["owner_profile"]["avatar"],
            owner_avatar_url(PNG_AVATAR)
        );
        assert_eq!(
            patch["agents"][0]["avatar"],
            agent_avatar_url(agent_id, PNG_AVATAR)
        );
        assert_eq!(patch["agents"][1]["avatar"], "dicebear:dylan:x");
        assert_eq!(patch["agents"][2]["avatar"], PNG_AVATAR);
        assert_eq!(
            patch["agent"]["avatar"],
            agent_avatar_url(agent_id, PNG_AVATAR)
        );
        assert_eq!(patch["channels"][0]["avatar"], PNG_AVATAR);
    }

    #[test]
    fn avatar_response_is_immutable_and_revalidates_by_digest() {
        let response = avatar_response(PNG_AVATAR, &HeaderMap::new());
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
        assert_eq!(response.headers()[header::CACHE_CONTROL], IMMUTABLE_CACHE);
        let etag = response.headers()[header::ETAG].clone();
        assert_eq!(etag, format!("\"{}\"", avatar_version(PNG_AVATAR)).as_str());

        let mut headers = HeaderMap::new();
        headers.insert(header::IF_NONE_MATCH, etag);
        let cached = avatar_response(PNG_AVATAR, &headers);
        assert_eq!(cached.status(), StatusCode::NOT_MODIFIED);

        let missing = avatar_response("dicebear:dylan:owner", &HeaderMap::new());
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }
}
