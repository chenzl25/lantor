use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{Duration, Utc};
use ring::{
    aead, agreement,
    rand::SystemRandom,
    signature::{UnparsedPublicKey, ECDSA_P256_SHA256_FIXED},
};
use serde_json::Value;
use sqlx::SqlitePool;
use uuid::Uuid;

use super::{
    announce_new_items, encrypt_payload, hkdf_sha256, load_or_create_vapid_key,
    save_push_subscription, seal_payload, validate_endpoint, PushSubscribeRequest,
    PushSubscriptionKeys,
};
use crate::test_support::{drop_test_schema, insert_test_agent, insert_test_channel, test_pool};

fn b64(value: &str) -> Vec<u8> {
    URL_SAFE_NO_PAD
        .decode(value)
        .expect("test vector is base64url")
}

#[test]
fn seal_matches_the_rfc_8291_example() {
    // RFC 8291 Appendix A, starting from the documented ECDH secret.
    let salt: [u8; 16] = b64("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap();
    let body = seal_payload(
        &b64("kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs"),
        &b64("BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4"),
        &b64("BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8"),
        &b64("BTBZMqHH6r4Tts7J_aSIgg"),
        &salt,
        b"When I grow up, I want to be a watermelon",
    )
    .unwrap();
    assert_eq!(
        URL_SAFE_NO_PAD.encode(body),
        "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
    );
}

#[test]
fn encrypted_payload_opens_with_the_subscription_key() {
    let rng = SystemRandom::new();
    let ua_private = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng).unwrap();
    let ua_public = ua_private.compute_public_key().unwrap();
    let auth = [7u8; 16];
    let plaintext = br#"{"title":"@Vegapunk needs your call"}"#;

    let body = encrypt_payload(ua_public.as_ref(), &auth, plaintext).unwrap();
    let salt = &body[..16];
    assert_eq!(&body[16..20], &4096u32.to_be_bytes());
    let key_len = body[20] as usize;
    let as_public = &body[21..21 + key_len];
    let mut record = body[21 + key_len..].to_vec();

    let peer = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, as_public);
    let (cek, nonce) = agreement::agree_ephemeral(ua_private, &peer, |secret| {
        let ikm = hkdf_sha256(
            &auth,
            secret,
            &[b"WebPush: info\0", ua_public.as_ref(), as_public],
            32,
        )
        .unwrap();
        (
            hkdf_sha256(salt, &ikm, &[b"Content-Encoding: aes128gcm\0"], 16).unwrap(),
            hkdf_sha256(salt, &ikm, &[b"Content-Encoding: nonce\0"], 12).unwrap(),
        )
    })
    .unwrap();
    let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_128_GCM, &cek).unwrap());
    let opened = key
        .open_in_place(
            aead::Nonce::try_assume_unique_for_key(&nonce).unwrap(),
            aead::Aad::empty(),
            &mut record,
        )
        .unwrap();
    assert_eq!(opened.last(), Some(&0x02));
    assert_eq!(&opened[..opened.len() - 1], plaintext);
}

#[tokio::test]
async fn vapid_authorization_is_a_verifiable_es256_token() -> Result<(), String> {
    let Some((pool, schema)) = test_pool().await else {
        return Ok(());
    };
    let result = async {
        let key = load_or_create_vapid_key(&pool).await?;
        let again = load_or_create_vapid_key(&pool).await?;
        assert_eq!(key.public_key, again.public_key, "the key is created once");

        let now = Utc::now();
        let header = key.authorization("https://web.push.apple.com/QGuQyavXutnMR/abc", now)?;
        let (token, public) = header
            .strip_prefix("vapid t=")
            .and_then(|rest| rest.split_once(", k="))
            .expect("vapid t=…, k=… header");
        assert_eq!(public, key.public_key);
        let (signing_input, signature) = token.rsplit_once('.').unwrap();
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, b64(public))
            .verify(signing_input.as_bytes(), &b64(signature))
            .expect("signature verifies with the advertised key");
        let claims: Value =
            serde_json::from_slice(&b64(signing_input.split_once('.').unwrap().1)).unwrap();
        assert_eq!(claims["aud"], "https://web.push.apple.com");
        assert_eq!(claims["exp"], now.timestamp() + 12 * 60 * 60);
        assert!(claims["sub"].as_str().unwrap().starts_with("https://"));
        Ok::<_, String>(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result
}

#[test]
fn only_browser_push_services_are_accepted() {
    for endpoint in [
        "https://fcm.googleapis.com/fcm/send/abc:def",
        "https://web.push.apple.com/QGuQyavXutnMR",
        "https://updates.push.services.mozilla.com/wpush/v2/gAAA",
        "https://wns2-par02p.notify.windows.com/w/?token=abc",
    ] {
        assert!(validate_endpoint(endpoint).is_ok(), "{endpoint}");
    }
    for endpoint in [
        "http://fcm.googleapis.com/fcm/send/abc",
        "https://evil.example/fcm.googleapis.com",
        "https://fcm.googleapis.com.evil.example/x",
        "https://user@fcm.googleapis.com/x",
        "https://127.0.0.1:9/x",
        "file:///etc/passwd",
        "https://fcm.googleapis.com/a b",
    ] {
        assert!(validate_endpoint(endpoint).is_err(), "{endpoint}");
    }
}

#[tokio::test]
async fn subscriptions_require_well_formed_keys() -> Result<(), String> {
    let Some((pool, schema)) = test_pool().await else {
        return Ok(());
    };
    let result = async {
        let rng = SystemRandom::new();
        let ua = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng).unwrap();
        let p256dh = URL_SAFE_NO_PAD.encode(ua.compute_public_key().unwrap().as_ref());
        let request = |endpoint: &str, p256dh: &str, auth: &str| PushSubscribeRequest {
            endpoint: endpoint.to_owned(),
            keys: PushSubscriptionKeys {
                p256dh: p256dh.to_owned(),
                auth: auth.to_owned(),
            },
            user_agent: Some("iPhone".to_owned()),
        };
        let endpoint = "https://web.push.apple.com/device-1";
        let auth = URL_SAFE_NO_PAD.encode([1u8; 16]);

        assert!(
            save_push_subscription(&pool, request(endpoint, "AAAA", &auth))
                .await
                .is_err()
        );
        assert!(
            save_push_subscription(&pool, request(endpoint, &p256dh, "AAAA"))
                .await
                .is_err()
        );
        assert!(
            save_push_subscription(&pool, request("https://example.com/x", &p256dh, &auth))
                .await
                .is_err()
        );

        // Padded input is normalized, and re-subscribing the same endpoint upserts.
        save_push_subscription(&pool, request(endpoint, &p256dh, &format!("{auth}=="))).await?;
        save_push_subscription(&pool, request(endpoint, &p256dh, &auth)).await?;
        let rows: Vec<(String, String)> =
            sqlx::query_as("select endpoint, auth from push_subscriptions")
                .fetch_all(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(rows, vec![(endpoint.to_owned(), auth)]);
        Ok::<_, String>(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result
}

async fn insert_task(
    pool: &SqlitePool,
    channel_id: Uuid,
    agent_id: Uuid,
    title: &str,
    status: &str,
    updated_at: &str,
) -> Result<Uuid, String> {
    let message_id: Uuid = sqlx::query_scalar(
        "insert into messages (channel_id, sender_name, sender_role, body) values ($1, 'owner', 'owner', $2) returning id",
    )
    .bind(channel_id)
    .bind(title)
    .fetch_one(pool)
    .await
    .map_err(|err| err.to_string())?;
    sqlx::query_scalar(
        r#"
        insert into tasks (message_id, channel_id, title, status, assignee_agent_id, updated_at)
        values ($1, $2, $3, $4, $5, $6)
        returning id
        "#,
    )
    .bind(message_id)
    .bind(channel_id)
    .bind(title)
    .bind(status)
    .bind(agent_id)
    .bind(updated_at)
    .fetch_one(pool)
    .await
    .map_err(|err| err.to_string())
}

async fn insert_decision(
    pool: &SqlitePool,
    channel_id: Uuid,
    agent_id: Uuid,
    title: &str,
) -> Result<Uuid, String> {
    let message_id: Uuid = sqlx::query_scalar(
        "insert into messages (channel_id, sender_name, sender_role, body) values ($1, 'agent', 'agent', $2) returning id",
    )
    .bind(channel_id)
    .bind(title)
    .fetch_one(pool)
    .await
    .map_err(|err| err.to_string())?;
    sqlx::query_scalar(
        "insert into decisions (message_id, channel_id, requester_agent_id, title) values ($1, $2, $3, $4) returning id",
    )
    .bind(message_id)
    .bind(channel_id)
    .bind(agent_id)
    .bind(title)
    .fetch_one(pool)
    .await
    .map_err(|err| err.to_string())
}

#[tokio::test]
async fn new_needs_you_items_are_announced_once() -> Result<(), String> {
    let Some((pool, schema)) = test_pool().await else {
        return Ok(());
    };
    let result = async {
        let channel_id = insert_test_channel(&pool, "lantor-dev").await?;
        let agent_id = insert_test_agent(&pool, "Vegapunk").await?;
        let now = Utc::now();
        let stale = (now - Duration::days(2)).to_rfc3339();
        let fresh = (now - Duration::seconds(5)).to_rfc3339();

        let old_task = insert_task(
            &pool,
            channel_id,
            agent_id,
            "Old review",
            "in_review",
            &stale,
        )
        .await?;
        let new_task = insert_task(
            &pool,
            channel_id,
            agent_id,
            "Fresh review",
            "in_review",
            &fresh,
        )
        .await?;
        insert_task(
            &pool,
            channel_id,
            agent_id,
            "Still working",
            "in_progress",
            &fresh,
        )
        .await?;
        let decision = insert_decision(&pool, channel_id, agent_id, "Merge #174?").await?;

        // A backlog from before the owner subscribed stays quiet.
        let first = announce_new_items(&pool, now).await?;
        let titles = first
            .iter()
            .map(|item| item.payload.title.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            titles,
            ["@Vegapunk needs your call", "Task #2 is ready for review"]
        );
        assert_eq!(first[0].payload.body, "Merge #174?\n#lantor-dev");
        assert_eq!(first[0].payload.badge, 3);
        assert_eq!(
            first[1].payload.body,
            "Fresh review\n@Vegapunk · #lantor-dev"
        );
        let target = first[1].payload.target.as_ref().unwrap();
        assert_eq!(target.thread_root_id, Some(target.message_id));
        assert_eq!(first[0].key, format!("decision:{}", decision.simple()));
        assert_eq!(first[1].key, format!("task_review:{}", new_task.simple()));

        assert!(
            announce_new_items(&pool, now).await?.is_empty(),
            "announced once"
        );

        // Leaving review and coming back announces again; the stale task stays quiet.
        sqlx::query("update tasks set status = 'in_progress' where id in ($1, $2)")
            .bind(new_task)
            .bind(old_task)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        sqlx::query("update decisions set status = 'answered' where id = $1")
            .bind(decision)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert!(announce_new_items(&pool, now).await?.is_empty());
        sqlx::query("update tasks set status = 'in_review', updated_at = $2 where id = $1")
            .bind(new_task)
            .bind(Utc::now().to_rfc3339())
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        let again = announce_new_items(&pool, Utc::now()).await?;
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].payload.badge, 1);
        Ok::<_, String>(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result
}
