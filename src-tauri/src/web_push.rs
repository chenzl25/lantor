//! Web Push for the mobile web UI: when an agent needs the owner (an open
//! decision card, or a task moved to review), every subscribed browser gets a
//! system notification, even with the web app closed.
//!
//! The web process scans for new items instead of hooking every writer, so
//! cards created by the supervisor and tasks moved by any code path are picked
//! up the same way. Payloads are encrypted per RFC 8291 and signed with a VAPID
//! key (RFC 8292) using `ring`, which is already in the dependency tree;
//! delivery shells out to `curl`, like GitHub sync shells out to `gh`.

use std::{process::Stdio, time::Duration};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use ring::{
    aead, agreement, hkdf,
    rand::{SecureRandom, SystemRandom},
    signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Row, SqlitePool};
use tokio::{io::AsyncWriteExt, process::Command, time::sleep};
use uuid::Uuid;

use crate::app::{to_string, CommandResult};

const SCAN_INTERVAL: Duration = Duration::from_secs(4);
/// Items first seen later than this (after downtime, or created before the
/// owner subscribed) are recorded silently instead of announced.
const FRESH_WINDOW_SECS: i64 = 10 * 60;
const PUSH_TTL_SECS: u32 = 24 * 60 * 60;
const SEND_TIMEOUT_SECS: u64 = 20;
const RECORD_SIZE: u32 = 4096;
const VAPID_TOKEN_LIFETIME_SECS: i64 = 12 * 60 * 60;
/// Apple rejects VAPID tokens whose `sub` is not a real mailto:/https: URL.
const DEFAULT_VAPID_CONTACT: &str = "https://github.com/chenzl25/lantor";
/// Browsers' push services. Subscriptions elsewhere are refused so the
/// unauthenticated web API cannot be used to make this machine POST anywhere.
const PUSH_SERVICE_HOSTS: &[&str] = &[
    "fcm.googleapis.com",
    "android.googleapis.com",
    "push.apple.com",
    "push.services.mozilla.com",
    "notify.windows.com",
];
const TITLE_LIMIT: usize = 120;
const BODY_LIMIT: usize = 240;

// ---------------------------------------------------------------------------
// Encryption and VAPID (RFC 8291 / RFC 8292)
// ---------------------------------------------------------------------------

struct HkdfLen(usize);

impl hkdf::KeyType for HkdfLen {
    fn len(&self) -> usize {
        self.0
    }
}

fn hkdf_sha256(salt: &[u8], ikm: &[u8], info: &[&[u8]], len: usize) -> CommandResult<Vec<u8>> {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(ikm);
    let okm = prk
        .expand(info, HkdfLen(len))
        .map_err(|_| "push HKDF expand failed".to_owned())?;
    let mut out = vec![0; len];
    okm.fill(&mut out)
        .map_err(|_| "push HKDF expand failed".to_owned())?;
    Ok(out)
}

/// Single-record `aes128gcm` body (RFC 8188 framing) for an ECDH secret that
/// was already agreed between the one-off sender key and the subscription.
fn seal_payload(
    ecdh_secret: &[u8],
    ua_public: &[u8],
    as_public: &[u8],
    auth_secret: &[u8],
    salt: &[u8; 16],
    plaintext: &[u8],
) -> CommandResult<Vec<u8>> {
    let ikm = hkdf_sha256(
        auth_secret,
        ecdh_secret,
        &[b"WebPush: info\0", ua_public, as_public],
        32,
    )?;
    let cek = hkdf_sha256(salt, &ikm, &[b"Content-Encoding: aes128gcm\0"], 16)?;
    let nonce = hkdf_sha256(salt, &ikm, &[b"Content-Encoding: nonce\0"], 12)?;

    let mut record = Vec::with_capacity(plaintext.len() + 1 + aead::AES_128_GCM.tag_len());
    record.extend_from_slice(plaintext);
    record.push(0x02); // Last-record delimiter, no padding.
    let key = aead::UnboundKey::new(&aead::AES_128_GCM, &cek)
        .map(aead::LessSafeKey::new)
        .map_err(|_| "push content key rejected".to_owned())?;
    let nonce = aead::Nonce::try_assume_unique_for_key(&nonce)
        .map_err(|_| "push nonce rejected".to_owned())?;
    key.seal_in_place_append_tag(nonce, aead::Aad::empty(), &mut record)
        .map_err(|_| "push payload encryption failed".to_owned())?;

    let key_id_len =
        u8::try_from(as_public.len()).map_err(|_| "push sender key is too long".to_owned())?;
    let mut body = Vec::with_capacity(16 + 4 + 1 + as_public.len() + record.len());
    body.extend_from_slice(salt);
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(key_id_len);
    body.extend_from_slice(as_public);
    body.extend_from_slice(&record);
    Ok(body)
}

fn encrypt_payload(
    ua_public: &[u8],
    auth_secret: &[u8],
    plaintext: &[u8],
) -> CommandResult<Vec<u8>> {
    let rng = SystemRandom::new();
    let sender = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng)
        .map_err(|_| "push sender key generation failed".to_owned())?;
    let as_public = sender
        .compute_public_key()
        .map_err(|_| "push sender key generation failed".to_owned())?;
    let mut salt = [0; 16];
    rng.fill(&mut salt)
        .map_err(|_| "push salt generation failed".to_owned())?;
    let peer = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, ua_public);
    agreement::agree_ephemeral(sender, &peer, |secret| {
        seal_payload(
            secret,
            ua_public,
            as_public.as_ref(),
            auth_secret,
            &salt,
            plaintext,
        )
    })
    .map_err(|_| "push subscription key rejected".to_owned())?
}

struct VapidKey {
    pair: EcdsaKeyPair,
    public_key: String,
}

impl VapidKey {
    fn from_pkcs8(pkcs8: &[u8]) -> CommandResult<Self> {
        let pair = EcdsaKeyPair::from_pkcs8(
            &ECDSA_P256_SHA256_FIXED_SIGNING,
            pkcs8,
            &SystemRandom::new(),
        )
        .map_err(|err| format!("stored VAPID key is invalid: {err}"))?;
        let public_key = URL_SAFE_NO_PAD.encode(pair.public_key().as_ref());
        Ok(Self { pair, public_key })
    }

    fn authorization(&self, endpoint: &str, now: DateTime<Utc>) -> CommandResult<String> {
        let claims = json!({
            "aud": endpoint_origin(endpoint)?,
            "exp": now.timestamp() + VAPID_TOKEN_LIFETIME_SECS,
            "sub": vapid_contact(),
        });
        let signing_input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"ES256"}"#),
            URL_SAFE_NO_PAD.encode(claims.to_string()),
        );
        let signature = self
            .pair
            .sign(&SystemRandom::new(), signing_input.as_bytes())
            .map_err(|_| "VAPID signing failed".to_owned())?;
        Ok(format!(
            "vapid t={signing_input}.{}, k={}",
            URL_SAFE_NO_PAD.encode(signature.as_ref()),
            self.public_key
        ))
    }
}

fn vapid_contact() -> String {
    std::env::var("LANTOR_PUSH_CONTACT")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| value.starts_with("mailto:") || value.starts_with("https://"))
        .unwrap_or_else(|| DEFAULT_VAPID_CONTACT.to_owned())
}

fn decode_key(value: &str) -> CommandResult<Vec<u8>> {
    // Browsers emit unpadded base64url; tolerate padding and the standard alphabet.
    let normalized = value
        .trim()
        .trim_end_matches('=')
        .replace('+', "-")
        .replace('/', "_");
    URL_SAFE_NO_PAD
        .decode(normalized)
        .map_err(|_| "push subscription key is not base64url".to_owned())
}

// ---------------------------------------------------------------------------
// Endpoints
// ---------------------------------------------------------------------------

struct EndpointParts<'a> {
    scheme: &'a str,
    authority: &'a str,
    host: String,
}

fn endpoint_parts(endpoint: &str) -> CommandResult<EndpointParts<'_>> {
    let invalid = || "push endpoint is not a valid URL".to_owned();
    if endpoint.len() > 2048
        || endpoint
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(invalid());
    }
    let (scheme, rest) = endpoint.split_once("://").ok_or_else(invalid)?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return Err(invalid());
    }
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host,
        _ => authority,
    };
    Ok(EndpointParts {
        scheme,
        authority,
        host: host.to_ascii_lowercase(),
    })
}

fn endpoint_origin(endpoint: &str) -> CommandResult<String> {
    let parts = endpoint_parts(endpoint)?;
    Ok(format!("{}://{}", parts.scheme, parts.authority))
}

/// Loopback endpoints are only accepted when a test harness opts in.
fn loopback_endpoints_allowed() -> bool {
    std::env::var("LANTOR_PUSH_ALLOW_LOOPBACK").is_ok_and(|value| value == "1")
}

fn validate_endpoint(endpoint: &str) -> CommandResult<()> {
    let parts = endpoint_parts(endpoint)?;
    let known_service = PUSH_SERVICE_HOSTS
        .iter()
        .any(|service| parts.host == *service || parts.host.ends_with(&format!(".{service}")));
    if parts.scheme == "https" && known_service {
        return Ok(());
    }
    let loopback = matches!(parts.host.as_str(), "127.0.0.1" | "localhost");
    if loopback && matches!(parts.scheme, "http" | "https") && loopback_endpoints_allowed() {
        return Ok(());
    }
    Err(format!(
        "push endpoint host {} is not a known browser push service",
        parts.host
    ))
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

async fn load_or_create_vapid_key(pool: &SqlitePool) -> CommandResult<VapidKey> {
    async fn stored(pool: &SqlitePool) -> CommandResult<Option<String>> {
        sqlx::query_scalar("select private_key_pkcs8 from push_vapid_keys where id = 1")
            .fetch_optional(pool)
            .await
            .map_err(to_string)
    }
    if let Some(encoded) = stored(pool).await? {
        return VapidKey::from_pkcs8(&decode_key(&encoded)?);
    }
    let pkcs8 =
        EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &SystemRandom::new())
            .map_err(|_| "VAPID key generation failed".to_owned())?;
    let key = VapidKey::from_pkcs8(pkcs8.as_ref())?;
    // Another Lantor process on the same database may win the race; either
    // way every process then signs with the one stored key.
    sqlx::query(
        "insert or ignore into push_vapid_keys (id, private_key_pkcs8, public_key) values (1, $1, $2)",
    )
    .bind(URL_SAFE_NO_PAD.encode(pkcs8.as_ref()))
    .bind(&key.public_key)
    .execute(pool)
    .await
    .map_err(to_string)?;
    let encoded = stored(pool)
        .await?
        .ok_or_else(|| "VAPID key was not stored".to_owned())?;
    VapidKey::from_pkcs8(&decode_key(&encoded)?)
}

#[derive(Debug, Clone)]
struct StoredSubscription {
    endpoint: String,
    p256dh: String,
    auth: String,
}

async fn load_subscriptions(
    pool: &SqlitePool,
    endpoint: Option<&str>,
) -> CommandResult<Vec<StoredSubscription>> {
    let rows = sqlx::query(
        "select endpoint, p256dh, auth from push_subscriptions where $1 is null or endpoint = $1 order by created_at",
    )
    .bind(endpoint)
    .fetch_all(pool)
    .await
    .map_err(to_string)?;
    Ok(rows
        .into_iter()
        .map(|row| StoredSubscription {
            endpoint: row.get("endpoint"),
            p256dh: row.get("p256dh"),
            auth: row.get("auth"),
        })
        .collect())
}

async fn count_needs_you(pool: &SqlitePool) -> CommandResult<i64> {
    sqlx::query_scalar(
        r#"
        select
            (select count(*) from decisions where status = 'open')
            + (select count(*) from tasks where status = 'in_review')
        "#,
    )
    .fetch_one(pool)
    .await
    .map_err(to_string)
}

// ---------------------------------------------------------------------------
// Delivery
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct PushTarget {
    pub(crate) channel_id: Uuid,
    pub(crate) thread_root_id: Option<Uuid>,
    pub(crate) message_id: Uuid,
}

/// Shape read by the service worker's `push` handler.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct PushPayload {
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) tag: String,
    pub(crate) target: Option<PushTarget>,
    /// Needs-you count for the home-screen icon badge.
    pub(crate) badge: i64,
}

#[derive(Debug, PartialEq, Eq)]
enum Delivery {
    Delivered,
    /// The push service no longer knows this subscription; drop it.
    Gone,
    Failed(String),
}

fn truncate(value: &str, limit: usize) -> String {
    let value = value.trim();
    if value.chars().count() <= limit {
        return value.to_owned();
    }
    let mut clipped = value.chars().take(limit - 1).collect::<String>();
    clipped.push('…');
    clipped
}

async fn deliver(
    subscription: &StoredSubscription,
    vapid: &VapidKey,
    plaintext: &[u8],
) -> Delivery {
    match try_deliver(subscription, vapid, plaintext).await {
        Ok(delivery) => delivery,
        Err(err) => Delivery::Failed(err),
    }
}

async fn try_deliver(
    subscription: &StoredSubscription,
    vapid: &VapidKey,
    plaintext: &[u8],
) -> CommandResult<Delivery> {
    validate_endpoint(&subscription.endpoint)?;
    let body = encrypt_payload(
        &decode_key(&subscription.p256dh)?,
        &decode_key(&subscription.auth)?,
        plaintext,
    )?;
    let authorization = vapid.authorization(&subscription.endpoint, Utc::now())?;
    let protocols = if endpoint_parts(&subscription.endpoint)?.scheme == "https" {
        "=https"
    } else {
        "=http,https"
    };
    let mut child = Command::new("curl")
        .args(["--silent", "--show-error", "--request", "POST"])
        .args(["--proto", protocols, "--max-time"])
        .arg(SEND_TIMEOUT_SECS.to_string())
        .args(["--header", &format!("TTL: {PUSH_TTL_SECS}")])
        .args(["--header", "Urgency: high"])
        .args(["--header", "Content-Encoding: aes128gcm"])
        .args(["--header", "Content-Type: application/octet-stream"])
        .args(["--header", &format!("Authorization: {authorization}")])
        .args(["--data-binary", "@-", "--write-out", "\n%{http_code}"])
        .args(["--url", &subscription.endpoint])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| format!("failed to run curl for push delivery: {err}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(&body).await.map_err(to_string)?;
    }
    let output = tokio::time::timeout(
        Duration::from_secs(SEND_TIMEOUT_SECS + 5),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| "push delivery timed out".to_owned())?
    .map_err(to_string)?;
    if !output.status.success() {
        return Ok(Delivery::Failed(truncate(
            &String::from_utf8_lossy(&output.stderr),
            300,
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let (response, status) = stdout.rsplit_once('\n').unwrap_or(("", &stdout));
    Ok(match status.trim().parse::<u16>().unwrap_or(0) {
        200..=299 => Delivery::Delivered,
        404 | 410 => Delivery::Gone,
        code => Delivery::Failed(truncate(&format!("HTTP {code} {}", response.trim()), 300)),
    })
}

async fn record_delivery(
    pool: &SqlitePool,
    endpoint: &str,
    delivery: &Delivery,
) -> CommandResult<()> {
    let query = match delivery {
        Delivery::Delivered => sqlx::query(
            r#"
            update push_subscriptions
            set failure_count = 0, last_error = null,
                last_success_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
            where endpoint = $1
            "#,
        )
        .bind(endpoint),
        Delivery::Gone => {
            sqlx::query("delete from push_subscriptions where endpoint = $1").bind(endpoint)
        }
        Delivery::Failed(error) => sqlx::query(
            r#"
            update push_subscriptions
            set failure_count = failure_count + 1, last_error = $2
            where endpoint = $1
            "#,
        )
        .bind(endpoint)
        .bind(error.as_str()),
    };
    query.execute(pool).await.map_err(to_string)?;
    Ok(())
}

async fn send_to_subscriptions(
    pool: &SqlitePool,
    subscriptions: &[StoredSubscription],
    payload: &PushPayload,
) -> CommandResult<Vec<Delivery>> {
    if subscriptions.is_empty() {
        return Ok(Vec::new());
    }
    let vapid = load_or_create_vapid_key(pool).await?;
    let plaintext = serde_json::to_vec(payload).map_err(to_string)?;
    let mut deliveries = Vec::with_capacity(subscriptions.len());
    for subscription in subscriptions {
        let delivery = deliver(subscription, &vapid, &plaintext).await;
        if let Delivery::Failed(error) = &delivery {
            eprintln!("Lantor push delivery failed: {error}");
        }
        record_delivery(pool, &subscription.endpoint, &delivery).await?;
        deliveries.push(delivery);
    }
    Ok(deliveries)
}

// ---------------------------------------------------------------------------
// Needs-you scanner
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NeedsYouItem {
    pub(crate) key: String,
    pub(crate) since: String,
    pub(crate) payload: PushPayload,
}

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

/// Open decisions and in-review tasks that have not been announced yet.
async fn load_unannounced_items(pool: &SqlitePool, badge: i64) -> CommandResult<Vec<NeedsYouItem>> {
    let decisions = sqlx::query(
        r#"
        select d.id, d.message_id, d.channel_id, d.thread_root_id, d.title, d.created_at,
               c.name as channel_name, a.handle as requester_handle
        from decisions d
        join channels c on c.id = d.channel_id
        left join agents a on a.id = d.requester_agent_id
        where d.status = 'open'
          and not exists (
              select 1 from push_announced_items p
              where p.item_key = 'decision:' || lower(hex(d.id))
          )
        order by d.created_at
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(to_string)?;
    let tasks = sqlx::query(
        r#"
        select t.id, t.number, t.title, t.message_id, t.channel_id, t.updated_at,
               c.name as channel_name, a.handle as assignee_handle
        from tasks t
        join channels c on c.id = t.channel_id
        left join agents a on a.id = t.assignee_agent_id
        where t.status = 'in_review'
          and not exists (
              select 1 from push_announced_items p
              where p.item_key = 'task_review:' || lower(hex(t.id))
          )
        order by t.updated_at
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    let mut items = Vec::with_capacity(decisions.len() + tasks.len());
    for row in decisions {
        let id: Uuid = row.get("id");
        let channel_name: String = row.get("channel_name");
        let requester: Option<String> = row.get("requester_handle");
        let title = match requester {
            Some(handle) => format!("@{handle} needs your call"),
            None => "An agent needs your call".to_owned(),
        };
        items.push(NeedsYouItem {
            key: format!("decision:{}", id.simple()),
            since: row.get("created_at"),
            payload: PushPayload {
                title: truncate(&title, TITLE_LIMIT),
                body: truncate(
                    &format!("{}\n#{channel_name}", row.get::<String, _>("title")),
                    BODY_LIMIT,
                ),
                tag: format!("decision:{id}"),
                target: Some(PushTarget {
                    channel_id: row.get("channel_id"),
                    thread_root_id: row.get("thread_root_id"),
                    message_id: row.get("message_id"),
                }),
                badge,
            },
        });
    }
    for row in tasks {
        let id: Uuid = row.get("id");
        let number: i64 = row.get("number");
        let channel_name: String = row.get("channel_name");
        let assignee: Option<String> = row.get("assignee_handle");
        let message_id: Uuid = row.get("message_id");
        let owner = assignee.map_or_else(String::new, |handle| format!("@{handle} · "));
        items.push(NeedsYouItem {
            key: format!("task_review:{}", id.simple()),
            since: row.get("updated_at"),
            payload: PushPayload {
                title: format!("Task #{number} is ready for review"),
                body: truncate(
                    &format!("{}\n{owner}#{channel_name}", row.get::<String, _>("title")),
                    BODY_LIMIT,
                ),
                tag: format!("task:{id}"),
                target: Some(PushTarget {
                    channel_id: row.get("channel_id"),
                    thread_root_id: Some(message_id),
                    message_id,
                }),
                badge,
            },
        });
    }
    Ok(items)
}

/// Forget items that left the needs-you state, so a task that goes back to
/// review later is announced again.
async fn forget_resolved_items(pool: &SqlitePool) -> CommandResult<()> {
    sqlx::query(
        r#"
        delete from push_announced_items
        where item_key not in (
            select 'decision:' || lower(hex(id)) from decisions where status = 'open'
            union all
            select 'task_review:' || lower(hex(id)) from tasks where status = 'in_review'
        )
        "#,
    )
    .execute(pool)
    .await
    .map_err(to_string)?;
    Ok(())
}

/// Announce each new needs-you item once. Returns the items that were fresh
/// enough to send (whether or not any browser is subscribed).
pub(crate) async fn announce_new_items(
    pool: &SqlitePool,
    now: DateTime<Utc>,
) -> CommandResult<Vec<NeedsYouItem>> {
    forget_resolved_items(pool).await?;
    let badge = count_needs_you(pool).await?;
    let items = load_unannounced_items(pool, badge).await?;
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let subscriptions = load_subscriptions(pool, None).await?;
    let mut announced = Vec::new();
    for item in items {
        // Claim before sending: every Lantor process on this database scans.
        let claimed =
            sqlx::query("insert or ignore into push_announced_items (item_key) values ($1)")
                .bind(&item.key)
                .execute(pool)
                .await
                .map_err(to_string)?
                .rows_affected()
                == 1;
        let fresh = parse_timestamp(&item.since)
            .is_some_and(|since| (now - since).num_seconds() <= FRESH_WINDOW_SECS);
        if !claimed || !fresh {
            continue;
        }
        send_to_subscriptions(pool, &subscriptions, &item.payload).await?;
        announced.push(item);
    }
    Ok(announced)
}

pub(crate) fn spawn_push_worker(pool: SqlitePool) {
    tauri::async_runtime::spawn(async move {
        loop {
            if let Err(err) = announce_new_items(&pool, Utc::now()).await {
                eprintln!("Lantor push worker failed: {err}");
            }
            sleep(SCAN_INTERVAL).await;
        }
    });
}

// ---------------------------------------------------------------------------
// Web API
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub(crate) struct PushConfig {
    public_key: String,
    subscription_count: i64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PushSubscriptionKeys {
    p256dh: String,
    auth: String,
}

/// `PushSubscription.toJSON()`, plus an optional user agent label.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PushSubscribeRequest {
    endpoint: String,
    keys: PushSubscriptionKeys,
    #[serde(default)]
    user_agent: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PushEndpointRequest {
    endpoint: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct PushTestResult {
    delivered: bool,
    error: Option<String>,
}

pub(crate) async fn load_push_config(pool: &SqlitePool) -> CommandResult<PushConfig> {
    let vapid = load_or_create_vapid_key(pool).await?;
    let subscription_count = sqlx::query_scalar("select count(*) from push_subscriptions")
        .fetch_one(pool)
        .await
        .map_err(to_string)?;
    Ok(PushConfig {
        public_key: vapid.public_key,
        subscription_count,
    })
}

pub(crate) async fn save_push_subscription(
    pool: &SqlitePool,
    request: PushSubscribeRequest,
) -> CommandResult<()> {
    let endpoint = request.endpoint.trim();
    validate_endpoint(endpoint)?;
    let p256dh = decode_key(&request.keys.p256dh)?;
    if p256dh.len() != 65 || p256dh[0] != 0x04 {
        return Err("push subscription p256dh must be an uncompressed P-256 point".to_owned());
    }
    let auth = decode_key(&request.keys.auth)?;
    if auth.len() != 16 {
        return Err("push subscription auth secret must be 16 bytes".to_owned());
    }
    let user_agent = truncate(request.user_agent.as_deref().unwrap_or_default(), 200);
    sqlx::query(
        r#"
        insert into push_subscriptions (endpoint, p256dh, auth, user_agent)
        values ($1, $2, $3, $4)
        on conflict (endpoint) do update set
            p256dh = excluded.p256dh,
            auth = excluded.auth,
            user_agent = excluded.user_agent,
            failure_count = 0,
            last_error = null,
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        "#,
    )
    .bind(endpoint)
    .bind(URL_SAFE_NO_PAD.encode(p256dh))
    .bind(URL_SAFE_NO_PAD.encode(auth))
    .bind(user_agent)
    .execute(pool)
    .await
    .map_err(to_string)?;
    Ok(())
}

pub(crate) async fn delete_push_subscription(
    pool: &SqlitePool,
    request: PushEndpointRequest,
) -> CommandResult<()> {
    sqlx::query("delete from push_subscriptions where endpoint = $1")
        .bind(request.endpoint.trim())
        .execute(pool)
        .await
        .map_err(to_string)?;
    Ok(())
}

pub(crate) async fn send_test_push(
    pool: &SqlitePool,
    request: PushEndpointRequest,
) -> CommandResult<PushTestResult> {
    let subscriptions = load_subscriptions(pool, Some(request.endpoint.trim())).await?;
    if subscriptions.is_empty() {
        return Err("This device is not subscribed to notifications".to_owned());
    }
    let payload = PushPayload {
        title: "Lantor notifications are on".to_owned(),
        body:
            "You will be notified here when an agent needs your call or a task is ready for review."
                .to_owned(),
        tag: "lantor-test".to_owned(),
        target: None,
        badge: count_needs_you(pool).await?,
    };
    let delivery = send_to_subscriptions(pool, &subscriptions, &payload)
        .await?
        .pop()
        .unwrap_or(Delivery::Failed("nothing was sent".to_owned()));
    Ok(match delivery {
        Delivery::Delivered => PushTestResult {
            delivered: true,
            error: None,
        },
        Delivery::Gone => PushTestResult {
            delivered: false,
            error: Some(
                "The push service dropped this subscription; turn notifications on again"
                    .to_owned(),
            ),
        },
        Delivery::Failed(error) => PushTestResult {
            delivered: false,
            error: Some(error),
        },
    })
}

#[cfg(test)]
#[path = "tests/web_push.rs"]
mod tests;
