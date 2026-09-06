use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use super::{fts_query, search_messages_without_artifact_content, search_sql};
use crate::test_support::{drop_test_schema, insert_test_channel, test_pool};

async fn assert_legacy_results(pool: &SqlitePool, query: &str, after: Option<&str>, limit: i64) {
    let actual = search_messages_without_artifact_content(pool, query, after, limit)
        .await
        .unwrap();
    let query = query.trim();
    let expected: Vec<Uuid> = if query.is_empty() {
        Vec::new()
    } else {
        let escaped = query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        sqlx::query(include_str!("legacy.sql"))
            .bind(format!("%{escaped}%"))
            .bind(after)
            .bind(limit.clamp(1, 100))
            .fetch_all(pool)
            .await
            .unwrap()
            .iter()
            .map(|row| row.get("id"))
            .collect()
    };
    assert_eq!(
        actual.iter().map(|message| message.id).collect::<Vec<_>>(),
        expected,
        "query {query:?}, after {after:?}, limit {limit} (including seq order)"
    );
}

#[tokio::test]
async fn indexed_search_preserves_substrings_fields_visibility_time_and_limits() {
    let (pool, path) = test_pool().await.expect("migrated FTS5 fixture");
    let channel_id = insert_test_channel(&pool, "Archive-频道-Matches_%")
        .await
        .unwrap();
    let other_id = insert_test_channel(&pool, "separate-space").await.unwrap();
    for (index, body) in [
        "English GitHub refresh abcdefgh",
        "中文消息搜索优化",
        "混合消息 FTS5 SQLite",
        "literal 100%_match and 100xxmatch",
        "quotes \"inside\" and a\"b",
        "foo OR bar AND baz NOT qux NEAR(one two)",
        "brackets [xyz] {body:foo} *star* -dash +plus ^caret",
        "backslash C:\\path\\file and a\\b",
        "line one\nline two\tthree",
        "emoji 😀🚀🤖 and café ÉCLAIR éclair Äpfel äpfel",
        "combining e\u{301}cole i\u{307}stanbul İSTANBUL",
        "abc\0trailing-text",
        "separated abc def; not abcdef",
        "plain content with sender-only match",
    ]
    .iter()
    .enumerate()
    {
        sqlx::query("insert into messages(channel_id, sender_name, sender_role, body, created_at) values ($1, $2, 'owner', $3, $4)")
            .bind(if index % 2 == 0 {channel_id} else {other_id})
            .bind(if index == 13 {"Unique-Sender发送者"} else {"Dylan"})
            .bind(body)
            .bind(if index % 2 == 0 {"2026-01-02T00:00:00+00:00"} else {"2026-01-01T00:00:00+00:00"})
            .execute(&pool).await.unwrap();
    }
    // More than the maximum page size, with non-monotonic timestamps.
    for index in 0..120 {
        sqlx::query("insert into messages(channel_id, sender_name, sender_role, body, created_at) values ($1, 'Dylan', 'owner', 'limitmarker', $2)")
            .bind(other_id)
            .bind(if index % 2 == 0 {"2026-01-02T00:00:00+00:00"} else {"2026-01-01T00:00:00+00:00"})
            .execute(&pool).await.unwrap();
    }
    let root_id: Uuid = sqlx::query_scalar("select id from messages order by seq limit 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    for (role, state, body, kind) in [
        ("agent", "streaming", "hidden streaming text", "none"),
        ("agent", "complete", "  ", "none"),
        ("agent", "complete", "", "attachment"),
        ("agent", "complete", "", "artifact"),
        ("agent", "interrupted", "visible interrupted output", "none"),
        ("system", "complete", "", "none"),
        ("owner", "complete", "", "none"),
    ] {
        let id: Uuid = sqlx::query_scalar("insert into messages(channel_id, thread_root_id, sender_name, sender_role, body, delivery_state, stream_key) values ($1, $2, 'Visibility-Sender', $3, $4, $5, $6) returning id")
            .bind(channel_id).bind(root_id).bind(role).bind(body).bind(state)
            .bind(format!("{}:final", Uuid::new_v4()))
            .fetch_one(&pool).await.unwrap();
        if kind == "attachment" {
            sqlx::query("insert into message_attachments(message_id, original_name, mime_type, size_bytes, storage_path) values ($1, 'fixture.txt', 'text/plain', 1, '/synthetic/fixture.txt')")
                .bind(id).execute(&pool).await.unwrap();
        }
        if kind == "artifact" {
            sqlx::query("insert into artifacts(message_id, channel_id, kind, title, content) values ($1, $2, 'markdown', 'Fixture artifact', 'artifact-content-only')")
                .bind(id).bind(channel_id).execute(&pool).await.unwrap();
        }
    }
    for query in [
        "",
        "   ",
        "a",
        "ab",
        "abc",
        "ABCDEFGH",
        "  github  ",
        "中文",
        "中文消息",
        "消息搜",
        "消息 FTS5",
        "FTS5 SQLite",
        "%_match",
        "100%",
        "_",
        "%",
        "\\",
        "a\\b",
        "\\path\\",
        "\"inside\"",
        "a\"b",
        "OR",
        "foo OR bar",
        "NEAR(one two)",
        "[xyz]",
        "{body:foo}",
        "*star*",
        "line one\nline two",
        "two\tthree",
        "😀",
        "😀🚀",
        "😀🚀🤖",
        "café",
        "éclair",
        "ÉCLAIR",
        "äpfel",
        "e\u{301}cole",
        "İSTANBUL",
        "i\u{307}stanbul",
        "abc\0ignored",
        "trailing-text",
        "abc def",
        "Unique-Sender",
        "发送者",
        "archive-频道",
        "Matches_%",
        "limitmarker",
        "Visibility-Sender",
        "hidden streaming",
        "interrupted",
        "artifact-content-only",
        "no-such-match",
    ] {
        for after in [
            None,
            Some("2026-01-02T00:00:00+00:00"),
            Some("2027-01-01T00:00:00Z"),
            Some("invalid date"),
        ] {
            for limit in [0, 7, 150] {
                assert_legacy_results(&pool, query, after, limit).await;
            }
        }
    }
    // A channel rename must be visible immediately without rewriting messages.
    sqlx::query("update channels set name = 'Renamed-频道' where id = $1")
        .bind(channel_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_legacy_results(&pool, "Renamed-频道", None, 100).await;
    assert_legacy_results(&pool, "archive-频道", None, 100).await;
    sqlx::query("delete from channels where id = $1")
        .bind(channel_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_legacy_results(&pool, "github", None, 100).await;
    assert_legacy_results(&pool, "Unique-Sender", None, 100).await;
    sqlx::query("insert into messages_fts(messages_fts, rank) values ('integrity-check', 1)")
        .execute(&pool)
        .await
        .unwrap();
    drop_test_schema(pool, path).await;
}

#[tokio::test]
async fn indexed_plan_uses_fts_rowids_and_channel_index() {
    let (pool, path) = test_pool().await.expect("migrated FTS5 fixture");
    let plan = sqlx::query(&format!("explain query plan {}", search_sql(true)))
        .bind("%needle%")
        .bind(None::<&str>)
        .bind(40)
        .bind(fts_query("needle"))
        .fetch_all(&pool)
        .await
        .unwrap()
        .iter()
        .map(|row| row.get::<String, _>("detail"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(plan.contains("messages_fts VIRTUAL TABLE INDEX"), "{plan}");
    assert!(
        plan.contains("SEARCH m USING INTEGER PRIMARY KEY"),
        "{plan}"
    );
    assert!(plan.contains("messages_channel_seq_idx"), "{plan}");
    assert!(!plan.contains("SCAN m "), "{plan}");
    drop_test_schema(pool, path).await;
}
