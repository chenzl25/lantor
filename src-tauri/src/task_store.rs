use serde_json::json;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::events::activity::record_agent_activity;
use crate::models::Task;
use crate::ui_notifications::notify_ui_refresh;
use crate::{to_string, CommandResult};

pub(crate) async fn update_task_status_in_pool(
    pool: &SqlitePool,
    task_id: Uuid,
    status: String,
) -> CommandResult<()> {
    let status = status.trim();
    if !matches!(status, "todo" | "in_progress" | "in_review" | "done") {
        return Err(format!("unsupported task status: {status}"));
    }

    let affected = sqlx::query(
        r#"
        update tasks
        set status = $2, version = version + 1, updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1
        "#,
    )
    .bind(task_id)
    .bind(status)
    .execute(pool)
    .await
    .map_err(to_string)?
    .rows_affected();
    if affected == 0 {
        return Err("task does not exist".to_owned());
    }
    record_agent_activity(
        pool,
        None,
        None,
        "task",
        "Task status updated",
        json!({ "task_id": task_id, "status": status }).to_string(),
    )
    .await?;

    let _ = notify_ui_refresh(pool, "task_status_updated").await;
    Ok(())
}

pub(crate) async fn update_task_title_in_pool(
    pool: &SqlitePool,
    task_id: Uuid,
    title: String,
) -> CommandResult<()> {
    let title = title.trim();
    if title.is_empty() {
        return Err("task title is empty".to_owned());
    }

    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(to_string)?;
    let message_id: Uuid = sqlx::query_scalar(
        r#"
        update tasks
        set title = $2, version = version + 1, updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1
        returning message_id
        "#,
    )
    .bind(task_id)
    .bind(title)
    .fetch_one(&mut *tx)
    .await
    .map_err(to_string)?;

    sqlx::query("update messages set body = $2, updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') where id = $1")
        .bind(message_id)
        .bind(title)
        .execute(&mut *tx)
        .await
        .map_err(to_string)?;

    tx.commit().await.map_err(to_string)?;
    record_agent_activity(
        pool,
        None,
        None,
        "task",
        "Task title updated",
        json!({ "task_id": task_id, "title": title }).to_string(),
    )
    .await?;
    let _ = notify_ui_refresh(pool, "task_title_updated").await;
    Ok(())
}

pub(crate) async fn load_tasks(pool: &SqlitePool) -> CommandResult<Vec<Task>> {
    let rows = sqlx::query(
        r#"
        select
            t.id,
            t.number,
            t.message_id,
            t.channel_id,
            t.title,
            t.status,
            t.version,
            c.name as channel_name,
            t.assignee_agent_id as assignee_id,
            a.display_name as assignee_name,
            t.created_at,
            t.updated_at
        from tasks t
        join channels c on c.id = t.channel_id
        left join agents a on a.id = t.assignee_agent_id
        order by t.number desc
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    Ok(rows
        .into_iter()
        .map(|row| Task {
            id: row.get("id"),
            number: row.get("number"),
            message_id: row.get("message_id"),
            channel_id: row.get("channel_id"),
            title: row.get("title"),
            status: row.get("status"),
            version: row.get("version"),
            channel_name: row.get("channel_name"),
            assignee_id: row.get("assignee_id"),
            assignee_name: row.get("assignee_name"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
        .collect())
}
