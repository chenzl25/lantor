use std::collections::HashMap;

use chrono::{DateTime, Utc};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::ui_notifications::notify_ui_refresh;
use crate::{to_string, CommandResult};

pub(crate) async fn dismiss_inbox_items_in_pool<I>(pool: &SqlitePool, items: I) -> CommandResult<()>
where
    I: IntoIterator<Item = (String, DateTime<Utc>)>,
{
    let mut updated = false;
    for item in items {
        let item_id = item.0.trim();
        if item_id.is_empty() {
            continue;
        }
        sqlx::query(
            r#"
            insert into owner_inbox_hidden_items (item_id, hidden_until, hidden_at)
            values ($1, $2, strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
            on conflict (item_id) do update set
                hidden_until = max(
                    owner_inbox_hidden_items.hidden_until,
                    excluded.hidden_until
                ),
                hidden_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
            "#,
        )
        .bind(item_id)
        .bind(item.1)
        .execute(pool)
        .await
        .map_err(to_string)?;
        updated = true;
    }

    if updated {
        notify_ui_refresh(pool, "owner_inbox_dismissed").await?;
    }
    Ok(())
}

pub(crate) async fn mark_inbox_items_read_in_pool<I>(
    pool: &SqlitePool,
    items: I,
) -> CommandResult<()>
where
    I: IntoIterator<Item = (String, DateTime<Utc>)>,
{
    let mut updated = false;
    for item in items {
        let item_id = item.0.trim();
        if item_id.is_empty() {
            continue;
        }
        sqlx::query(
            r#"
            insert into owner_inbox_read_state (item_id, read_until, read_at)
            values ($1, $2, strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
            on conflict (item_id) do update set
                read_until = max(
                    owner_inbox_read_state.read_until,
                    excluded.read_until
                ),
                read_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
            "#,
        )
        .bind(item_id)
        .bind(item.1)
        .execute(pool)
        .await
        .map_err(to_string)?;
        updated = true;
    }

    if updated {
        notify_ui_refresh(pool, "owner_inbox_read").await?;
    }
    Ok(())
}

pub(crate) async fn mark_all_owner_inbox_read_in_pool(pool: &SqlitePool) -> CommandResult<()> {
    sqlx::query(
        r#"
        insert into owner_inbox_read_state (item_id, read_until, read_at)
        select 'task:' || lower(
            substr(hex(id), 1, 8) || '-' ||
            substr(hex(id), 9, 4) || '-' ||
            substr(hex(id), 13, 4) || '-' ||
            substr(hex(id), 17, 4) || '-' ||
            substr(hex(id), 21, 12)
        ), strftime('%Y-%m-%dT%H:%M:%f+00:00','now'), strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        from tasks
        where status = 'in_review'
        on conflict (item_id) do update set
            read_until = max(owner_inbox_read_state.read_until, excluded.read_until),
            read_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        "#,
    )
    .execute(pool)
    .await
    .map_err(to_string)?;

    sqlx::query(
        r#"
        insert into owner_inbox_read_state (item_id, read_until, read_at)
        select 'reminder:' || lower(
            substr(hex(id), 1, 8) || '-' ||
            substr(hex(id), 9, 4) || '-' ||
            substr(hex(id), 13, 4) || '-' ||
            substr(hex(id), 17, 4) || '-' ||
            substr(hex(id), 21, 12)
        ), strftime('%Y-%m-%dT%H:%M:%f+00:00','now'), strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        from reminders
        where status = 'fired'
        on conflict (item_id) do update set
            read_until = max(owner_inbox_read_state.read_until, excluded.read_until),
            read_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        "#,
    )
    .execute(pool)
    .await
    .map_err(to_string)?;

    sqlx::query(
        r#"
        insert into channel_read_state (channel_id, last_read_at)
        select id, strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        from channels
        where true
        on conflict (channel_id) do update set last_read_at = excluded.last_read_at
        "#,
    )
    .execute(pool)
    .await
    .map_err(to_string)?;

    notify_ui_refresh(pool, "owner_inbox_mark_all_read").await?;
    Ok(())
}

pub(crate) async fn mark_channel_read_in_pool(
    pool: &SqlitePool,
    channel_id: Uuid,
) -> CommandResult<()> {
    sqlx::query(
        r#"
        insert into channel_read_state (channel_id, last_read_at)
        values ($1, strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
        on conflict (channel_id) do update set last_read_at = excluded.last_read_at
        "#,
    )
    .bind(channel_id)
    .execute(pool)
    .await
    .map_err(to_string)?;

    let _ = notify_ui_refresh(pool, "channel_read").await;
    Ok(())
}

pub(crate) async fn update_thread_followed_in_pool(
    pool: &SqlitePool,
    thread_root_id: Uuid,
    followed: bool,
) -> CommandResult<()> {
    sqlx::query(
        r#"
        update messages
        set thread_followed = $2
        where id = $1 and thread_root_id is null
        "#,
    )
    .bind(thread_root_id)
    .bind(followed)
    .execute(pool)
    .await
    .map_err(to_string)?;

    Ok(())
}

pub(crate) async fn load_dismissed_inbox_items(
    pool: &SqlitePool,
) -> CommandResult<HashMap<String, DateTime<Utc>>> {
    let rows = sqlx::query(
        r#"
        select item_id, hidden_until
        from owner_inbox_hidden_items
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    Ok(rows
        .into_iter()
        .map(|row| (row.get("item_id"), row.get("hidden_until")))
        .collect())
}

pub(crate) async fn load_read_inbox_items(
    pool: &SqlitePool,
) -> CommandResult<HashMap<String, DateTime<Utc>>> {
    let rows = sqlx::query(
        r#"
        select item_id, max(read_until) as read_until
        from (
            select item_id, read_until
            from owner_inbox_read_state
            union all
            select item_id, dismissed_until as read_until
            from owner_inbox_dismissals
        ) reads
        group by item_id
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    Ok(rows
        .into_iter()
        .map(|row| (row.get("item_id"), row.get("read_until")))
        .collect())
}

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use sqlx::{Row, SqlitePool};
    use std::{fs as std_fs, time::Duration};
    use uuid::Uuid;

    use crate::{db_connect_with_url, migrate};

    use super::{
        dismiss_inbox_items_in_pool, mark_all_owner_inbox_read_in_pool,
        mark_inbox_items_read_in_pool, DateTime, Utc,
    };

    async fn test_pool() -> Option<(SqlitePool, String)> {
        let database_path =
            std::env::temp_dir().join(format!("lantor-test-{}.sqlite", Uuid::new_v4().simple()));
        let database_path = database_path.to_string_lossy().into_owned();
        let database_url = format!("sqlite://{database_path}");
        let pool = match db_connect_with_url(&database_url, 1).await {
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
        let _ = std_fs::remove_file(database_path);
        let _ = std_fs::remove_file(format!("{database_path}-wal"));
        let _ = std_fs::remove_file(format!("{database_path}-shm"));
    }

    async fn drop_test_schema(pool: SqlitePool, database_path: String) {
        pool.close().await;
        drop_sqlite_test_files(&database_path);
    }

    async fn latest_ui_event_id(pool: &SqlitePool) -> Result<i64, String> {
        sqlx::query_scalar("select coalesce(max(id), 0) from ui_events")
            .fetch_one(pool)
            .await
            .map_err(|err| err.to_string())
    }

    async fn wait_for_ui_refresh_reason(
        pool: &SqlitePool,
        last_event_id: &mut i64,
        reason: &str,
    ) -> Result<(), String> {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let rows = sqlx::query(
                    "select id, event_json from ui_events where id > $1 order by id asc",
                )
                .bind(*last_event_id)
                .fetch_all(pool)
                .await
                .map_err(|err| err.to_string())?;
                for row in rows {
                    *last_event_id = row.get::<i64, _>("id");
                    let payload: String = row.get("event_json");
                    let value: Value =
                        serde_json::from_str(&payload).map_err(|err| err.to_string())?;
                    if value.get("reason").and_then(Value::as_str) == Some(reason) {
                        return Ok(());
                    }
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .map_err(|_| format!("timed out waiting for {reason} notification"))?
    }

    async fn insert_test_channel(pool: &SqlitePool, name: &str) -> Result<Uuid, String> {
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

    #[tokio::test]
    async fn mark_all_inbox_read_uses_current_cutoff_without_dismissing_tasks() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let channel_id = insert_test_channel(&pool, "mark-all-read").await?;
            let message_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'Review this task', true)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let task_id: Uuid = sqlx::query_scalar(
                r#"
                insert into tasks (message_id, channel_id, title, status, updated_at)
                values ($1, $2, 'Review this task', 'in_review', strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-1 hour'))
                returning id
                "#,
            )
            .bind(message_id)
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let task_updated_at: DateTime<Utc> =
                sqlx::query_scalar("select updated_at from tasks where id = $1")
                    .bind(task_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            let before_mark_all: DateTime<Utc> = sqlx::query_scalar("select strftime('%Y-%m-%dT%H:%M:%f+00:00','now')")
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;

            mark_all_owner_inbox_read_in_pool(&pool).await?;

            let read_until: DateTime<Utc> = sqlx::query_scalar(
                "select read_until from owner_inbox_read_state where item_id = $1",
            )
            .bind(format!("task:{task_id}"))
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let hidden: bool = sqlx::query_scalar(
                "select exists(select 1 from owner_inbox_hidden_items where item_id = $1)",
            )
            .bind(format!("task:{task_id}"))
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            assert!(read_until > task_updated_at);
            assert!(read_until >= before_mark_all);
            assert!(!hidden);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn dismiss_inbox_item_hides_without_marking_read() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let item_id = "thread:root:latest".to_owned();
            let hidden_until: DateTime<Utc> =
                sqlx::query_scalar("select strftime('%Y-%m-%dT%H:%M:%f+00:00','now','+5 seconds')")
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;

            dismiss_inbox_items_in_pool(&pool, [(item_id.clone(), hidden_until)]).await?;

            let stored_hidden_until: DateTime<Utc> = sqlx::query_scalar(
                "select hidden_until from owner_inbox_hidden_items where item_id = $1",
            )
            .bind(&item_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let read_exists: bool = sqlx::query_scalar(
                "select exists(select 1 from owner_inbox_read_state where item_id = $1)",
            )
            .bind(&item_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            assert_eq!(stored_hidden_until, hidden_until);
            assert!(!read_exists);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn inbox_read_and_dismiss_emit_ui_refresh() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let mut last_event_id = latest_ui_event_id(&pool).await?;
            let cutoff: DateTime<Utc> =
                sqlx::query_scalar("select strftime('%Y-%m-%dT%H:%M:%f+00:00','now')")
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;

            dismiss_inbox_items_in_pool(&pool, [("thread:root".to_owned(), cutoff)]).await?;
            wait_for_ui_refresh_reason(&pool, &mut last_event_id, "owner_inbox_dismissed").await?;

            mark_inbox_items_read_in_pool(&pool, [("thread:root".to_owned(), cutoff)]).await?;
            wait_for_ui_refresh_reason(&pool, &mut last_event_id, "owner_inbox_read").await?;
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn mark_all_owner_inbox_read_writes_db_snapshot() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let channel_id = insert_test_channel(&pool, "mark-all-inbox").await?;
            let root_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'thread root', false)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let reply_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, thread_root_id, sender_name, sender_role, body, is_task)
                values ($1, $2, 'Agent', 'agent', '@Dylan latest reply', false)
                returning id
                "#,
            )
            .bind(channel_id)
            .bind(root_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let task_message_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'task root', true)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let task_id: Uuid = sqlx::query_scalar(
                r#"
                insert into tasks (message_id, channel_id, title, status)
                values ($1, $2, 'Review this', 'in_review')
                returning id
                "#,
            )
            .bind(task_message_id)
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let active_task_message_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'active task root', true)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let active_task_id: Uuid = sqlx::query_scalar(
                r#"
                insert into tasks (message_id, channel_id, title, status)
                values ($1, $2, 'In progress task', 'in_progress')
                returning id
                "#,
            )
            .bind(active_task_message_id)
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let reminder_id: Uuid = sqlx::query_scalar(
                r#"
                insert into reminders (channel_id, title, note, due_at, fired_at, recurrence, status)
                values ($1, 'Due reminder', '', strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-1 minute'), strftime('%Y-%m-%dT%H:%M:%f+00:00','now'), 'none', 'fired')
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let read_channel_id = insert_test_channel(&pool, "mark-all-read-thread").await?;
            let read_root_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'already read thread root', false)
                returning id
                "#,
            )
            .bind(read_channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let read_reply_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, thread_root_id, sender_name, sender_role, body, is_task)
                values ($1, $2, 'Agent', 'agent', '@Dylan already read reply', false)
                returning id
                "#,
            )
            .bind(read_channel_id)
            .bind(read_root_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            sqlx::query(
                "insert into channel_read_state (channel_id, last_read_at) values ($1, strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))",
            )
            .bind(read_channel_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            mark_all_owner_inbox_read_in_pool(&pool).await?;

            let read_state_count: i64 = sqlx::query_scalar(
                "select count(*) from channel_read_state where channel_id = $1",
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(read_state_count, 1);

            for item_id in [format!("task:{task_id}"), format!("reminder:{reminder_id}")] {
                let exists: bool = sqlx::query_scalar(
                    "select exists(select 1 from owner_inbox_read_state where item_id = $1)",
                )
                .bind(item_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
                assert!(exists);
            }
            for item_id in [
                format!("thread:{root_id}:{reply_id}"),
                format!("mention:{reply_id}"),
                format!("task:{active_task_id}"),
                format!("reminder:{reminder_id}"),
                format!("thread:{read_root_id}:{read_reply_id}"),
                format!("mention:{read_reply_id}"),
            ] {
                let exists: bool = sqlx::query_scalar(
                    "select exists(select 1 from owner_inbox_hidden_items where item_id = $1)",
                )
                .bind(item_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
                assert!(!exists);
            }

            let reminder_status: String =
                sqlx::query_scalar("select status from reminders where id = $1")
                    .bind(reminder_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(reminder_status, "fired");
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }
}
