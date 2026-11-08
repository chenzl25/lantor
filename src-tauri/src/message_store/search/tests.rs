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

#[tokio::test]
async fn thread_find_counts_folded_matches_in_unloaded_replies() {
    let (pool, schema) = test_pool().await.expect("isolated database");
    let channel = insert_test_channel(&pool, "thread-find").await.unwrap();
    let insert = |thread_root_id: Option<Uuid>, body: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, Uuid>("insert into messages(channel_id,thread_root_id,sender_name,sender_role,body) values($1,$2,'owner','owner',$3) returning id")
                .bind(channel).bind(thread_root_id).bind(body).fetch_one(&pool).await.unwrap()
        }
    };
    let root = insert(None, "Root mentions RetryBudget").await;
    let other = insert(None, "other root").await;
    let code = insert(
        Some(root),
        "```rust\nlet retry_budget = RetryBudget::new();\n```\nretrybudget again",
    )
    .await;
    insert(Some(root), "nothing to see").await;
    let cjk = insert(Some(root), "重试预算 RETRY\nBUDGET 重试预算").await;
    let overlap = insert(Some(root), "aaaa").await;
    insert(Some(other), "RetryBudget in another thread").await;
    let newest = insert(Some(root), "RetryBudget in the loaded page").await;
    let seq = |id: Uuid| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>("select seq from messages where id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    let hits = |query: &'static str, before: Option<i64>| {
        let pool = pool.clone();
        async move {
            super::find_in_thread(&pool, root, query, before)
                .await
                .unwrap()
                .into_iter()
                .map(|hit| (hit.message_id, hit.seq, hit.matches))
                .collect::<Vec<_>>()
        }
    };

    // Case folds and the root, other threads and loaded replies stay out.
    let newest_seq = seq(newest).await;
    assert_eq!(
        hits("retrybudget", Some(newest_seq)).await,
        vec![(code, seq(code).await, 2)]
    );
    assert_eq!(hits("retrybudget", None).await.len(), 2);
    // A line break matches a space, and non-ASCII text matches as typed.
    assert_eq!(
        hits("retry budget", Some(newest_seq)).await,
        vec![(cjk, seq(cjk).await, 1)]
    );
    assert_eq!(hits("重试预算", None).await, vec![(cjk, seq(cjk).await, 2)]);
    // Matches do not overlap, as in the client.
    assert_eq!(
        hits("aa", None).await,
        vec![(overlap, seq(overlap).await, 2)]
    );
    assert!(hits("   ", None).await.is_empty());
    assert!(hits("", None).await.is_empty());
    assert!(hits("absent", None).await.is_empty());
    drop_test_schema(pool, schema).await;
}

#[test]
fn find_folding_keeps_offsets() {
    for text in [
        "İstanbul",
        "ẞ straße",
        "Σίσυφος",
        "tab\there\u{3000}wide",
        "😀 Emoji",
    ] {
        let folded = super::fold_for_find(text);
        assert_eq!(
            folded.encode_utf16().count(),
            text.encode_utf16().count(),
            "{text:?}"
        );
    }
    assert_eq!(super::fold_for_find("İ ẞ Σ\tA"), "İ ß σ a");
}
