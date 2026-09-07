use std::fs;

use sqlx::SqlitePool;
use uuid::Uuid;

use crate::db::{db_connect_with_url, migrate};

pub(crate) async fn test_pool() -> Option<(SqlitePool, String)> {
    test_pool_with_connections(1).await
}

pub(crate) async fn test_pool_with_connections(
    max_connections: u32,
) -> Option<(SqlitePool, String)> {
    let database_path =
        std::env::temp_dir().join(format!("lantor-test-{}.sqlite", Uuid::new_v4().simple()));
    let database_path = database_path.to_string_lossy().into_owned();
    let database_url = format!("sqlite://{database_path}");
    let pool = match db_connect_with_url(&database_url, max_connections).await {
        Ok(pool) => pool,
        Err(err) => {
            eprintln!("skipping SQLite-backed Lantor test: {err}");
            return None;
        }
    };
    if let Err(err) = migrate(&pool).await {
        eprintln!("skipping SQLite-backed Lantor test: {err}");
        pool.close().await;
        drop_sqlite_test_files(&database_path);
        return None;
    }
    Some((pool, database_path))
}

fn drop_sqlite_test_files(database_path: &str) {
    let _ = fs::remove_file(database_path);
    let _ = fs::remove_file(format!("{database_path}-wal"));
    let _ = fs::remove_file(format!("{database_path}-shm"));
    let _ = fs::remove_dir_all(std::path::Path::new(database_path).with_extension("run-logs"));
}

pub(crate) async fn drop_test_schema(pool: SqlitePool, database_path: String) {
    pool.close().await;
    drop_sqlite_test_files(&database_path);
}

pub(crate) async fn insert_test_agent(pool: &SqlitePool, handle: &str) -> Result<Uuid, String> {
    sqlx::query_scalar(
        r#"
        insert into agents (handle, display_name, role, status, runtime, model, avatar, description)
        values ($1, $2, 'agent', 'idle', 'codex', 'gpt-5.5', 'D', 'test agent')
        returning id
        "#,
    )
    .bind(handle)
    .bind(handle)
    .fetch_one(pool)
    .await
    .map_err(|err| err.to_string())
}

pub(crate) async fn insert_test_channel(pool: &SqlitePool, name: &str) -> Result<Uuid, String> {
    sqlx::query_scalar(
        r#"
        insert into channels (name, description, kind)
        values ($1, 'test channel', 'channel')
        returning id
        "#,
    )
    .bind(name)
    .fetch_one(pool)
    .await
    .map_err(|err| err.to_string())
}
