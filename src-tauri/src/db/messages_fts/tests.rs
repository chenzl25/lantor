use sqlx::{sqlite::SqlitePoolOptions, SqlitePool};

use super::migrate;

async fn fixture() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let fts5: i64 = sqlx::query_scalar("select sqlite_compileoption_used('ENABLE_FTS5')")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(fts5, 1, "the bundled SQLite must provide FTS5");
    sqlx::query(
        "create table messages (body text not null, sender_name text not null,
            delivery_state text not null default 'complete', metadata text default '')",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

async fn matches(pool: &SqlitePool, query: &str) -> Vec<i64> {
    sqlx::query_scalar("select rowid from messages_fts where messages_fts match $1 order by rowid")
        .bind(format!("\"{query}\""))
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn changes(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("select total_changes()")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn backfill_excludes_existing_streams_and_is_idempotent() {
    let pool = fixture().await;
    sqlx::query(
        "insert into messages(rowid, body, sender_name, delivery_state) values
            (1, '历史中文消息 GitHub', 'Dylan', 'complete'),
            (2, 'unfinished fragment', 'Agent', 'streaming')",
    )
    .execute(&pool)
    .await
    .unwrap();
    migrate(&pool).await.unwrap();
    assert_eq!(matches(&pool, "中文消").await, [1]);
    assert_eq!(matches(&pool, "github").await, [1]);
    assert!(matches(&pool, "unfinished").await.is_empty());

    let before = changes(&pool).await;
    migrate(&pool).await.unwrap();
    migrate(&pool).await.unwrap();
    assert_eq!(
        changes(&pool).await,
        before,
        "startup must not rebuild again"
    );

    sqlx::query("update messages set body = 'finished replacement', delivery_state = 'complete' where rowid = 2")
        .execute(&pool).await.unwrap();
    assert!(matches(&pool, "unfinished").await.is_empty());
    assert_eq!(matches(&pool, "finished replacement").await, [2]);
    // Every message is now finalized, so the external content must agree too.
    sqlx::query("insert into messages_fts(messages_fts, rank) values ('integrity-check', 1)")
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn streaming_deltas_and_metadata_updates_do_not_write_the_index() {
    let pool = fixture().await;
    migrate(&pool).await.unwrap();
    sqlx::query("insert into messages(rowid, body, sender_name, delivery_state) values (1, '', 'Agent', 'streaming')")
        .execute(&pool).await.unwrap();
    let before = changes(&pool).await;
    for delta in 0..64 {
        sqlx::query("update messages set body = body || $1, sender_name = $2 where rowid = 1")
            .bind(format!("中文 delta {delta} "))
            .bind(format!("Agent {delta}"))
            .execute(&pool)
            .await
            .unwrap();
    }
    assert_eq!(
        changes(&pool).await - before,
        64,
        "no FTS shadow-table writes per delta"
    );
    assert!(matches(&pool, "delta").await.is_empty());

    sqlx::query("update messages set delivery_state = 'complete' where rowid = 1")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(matches(&pool, "delta 63").await, [1]);
    let before = changes(&pool).await;
    sqlx::query("update messages set metadata = 'read', body = body, sender_name = sender_name, delivery_state = 'interrupted' where rowid = 1")
        .execute(&pool).await.unwrap();
    assert_eq!(
        changes(&pool).await - before,
        1,
        "unchanged indexed fields need no reindex"
    );
    sqlx::query("insert into messages_fts(messages_fts, rank) values ('integrity-check', 1)")
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn edits_restarted_streams_rollbacks_and_deletes_keep_the_index_consistent() {
    let pool = fixture().await;
    migrate(&pool).await.unwrap();
    sqlx::query("insert into messages(rowid, body, sender_name) values (1, 'original content', 'Original sender')")
        .execute(&pool).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("update messages set body = 'rolled back' where rowid = 1")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(matches(&pool, "original content").await, [1]);
    assert!(matches(&pool, "rolled back").await.is_empty());

    sqlx::query("update messages set body = 'edited content', sender_name = 'Renamed sender' where rowid = 1")
        .execute(&pool).await.unwrap();
    assert!(matches(&pool, "original").await.is_empty());
    assert_eq!(matches(&pool, "renamed").await, [1]);
    sqlx::query(
        "update messages set body = 'new stream', delivery_state = 'streaming' where rowid = 1",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(matches(&pool, "edited").await.is_empty());
    assert!(matches(&pool, "new stream").await.is_empty());
    sqlx::query("update messages set body = 'partial output', delivery_state = 'interrupted', rowid = 2 where rowid = 1")
        .execute(&pool).await.unwrap();
    assert_eq!(matches(&pool, "partial").await, [2]);
    sqlx::query("update messages set rowid = 3 where rowid = 2")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(matches(&pool, "partial").await, [3]);
    sqlx::query("insert into messages(rowid, body, sender_name, delivery_state) values (4, 'unindexed deletion', 'Agent', 'streaming')")
        .execute(&pool).await.unwrap();
    sqlx::query("delete from messages")
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches(&pool, "partial").await.is_empty());
    assert!(matches(&pool, "unindexed").await.is_empty());
    sqlx::query("insert into messages_fts(messages_fts, rank) values ('integrity-check', 1)")
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn failed_migration_rolls_back_table_and_backfill_for_retry() {
    let pool = fixture().await;
    sqlx::query(
        "create trigger messages_fts_after_insert after insert on messages begin select 1; end",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(migrate(&pool).await.is_err());
    let exists: bool = sqlx::query_scalar(
        "select exists(select 1 from sqlite_master where name = 'messages_fts')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!exists);
    sqlx::query("drop trigger messages_fts_after_insert")
        .execute(&pool)
        .await
        .unwrap();
    migrate(&pool).await.unwrap();
    sqlx::query("insert into messages(body, sender_name) values ('after retry', 'Dylan')")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(matches(&pool, "retry").await, [1]);
}
