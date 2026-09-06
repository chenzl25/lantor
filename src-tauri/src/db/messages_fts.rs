use sqlx::SqlitePool;

pub(super) async fn migrate(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let exists = "select exists(select 1 from sqlite_master where name = 'messages_fts')";
    if sqlx::query_scalar::<_, bool>(exists)
        .fetch_one(pool)
        .await?
    {
        return Ok(());
    }

    // The table, triggers and backfill become visible together. Recheck under
    // the write lock because another process may also be starting Lantor.
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    if !sqlx::query_scalar::<_, bool>(exists)
        .fetch_one(&mut *tx)
        .await?
    {
        sqlx::raw_sql(include_str!("messages_fts.sql"))
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

#[cfg(test)]
mod tests;
