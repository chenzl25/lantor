use std::collections::HashMap;

use sqlx::{sqlite::SqliteRow, Row, SqlitePool};
use uuid::Uuid;

use crate::agent_profile::DEFAULT_OWNER_DISPLAY_NAME;
use crate::agent_routing::{
    queue_agent_message_mentions, queue_mentions_as_work_items, upsert_agent_thread_subscription,
    MentionDispatchOrigin,
};
use crate::agent_work_dispatch::dispatch_unassigned_task_availability;
use crate::attachments::{write_attachment_file, ATTACHMENT_SIZE_LIMIT};
use crate::ui_notifications::{notify_ui_message_upsert, notify_ui_refresh};
use crate::{
    app::{to_string, CommandResult},
    models::{Artifact, AttachmentUpload, Message, MessageAttachment, SavedMessage},
};

pub(crate) async fn load_messages(pool: &SqlitePool) -> CommandResult<Vec<Message>> {
    let rows = sqlx::query(
        r#"
        select
            m.id,
            m.channel_id,
            m.thread_root_id,
            m.sender_agent_id,
            m.sender_name,
            m.sender_role,
            m.body,
            m.is_task,
            m.thread_followed,
            m.delivery_state,
            m.stream_key,
            t.number as task_number,
            t.status as task_status,
            m.created_at,
            m.updated_at
        from messages m
        left join tasks t on t.message_id = m.id
        order by julianday(m.created_at) asc, m.created_at asc
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    let mut messages: Vec<Message> = rows
        .into_iter()
        .map(|row| Message {
            id: row.get("id"),
            channel_id: row.get("channel_id"),
            thread_root_id: row.get("thread_root_id"),
            sender_agent_id: row.get("sender_agent_id"),
            sender_name: row.get("sender_name"),
            sender_role: row.get("sender_role"),
            body: row.get("body"),
            is_task: row.get("is_task"),
            thread_followed: row.get("thread_followed"),
            delivery_state: row.get("delivery_state"),
            stream_key: row.get("stream_key"),
            task_number: row.get("task_number"),
            task_status: row.get("task_status"),
            attachments: Vec::new(),
            artifacts: Vec::new(),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
        .collect();
    attach_message_attachments(pool, &mut messages).await?;
    attach_message_artifacts(pool, &mut messages).await?;
    Ok(messages)
}

pub(crate) async fn load_saved_messages(pool: &SqlitePool) -> CommandResult<Vec<SavedMessage>> {
    let rows = sqlx::query(
        r#"
        select
            sm.id,
            sm.message_id,
            m.channel_id,
            c.name as channel_name,
            m.thread_root_id,
            m.sender_name,
            m.sender_role,
            m.body,
            m.created_at as message_created_at,
            sm.created_at
        from saved_messages sm
        join messages m on m.id = sm.message_id
        join channels c on c.id = m.channel_id
        order by sm.created_at desc
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    Ok(rows
        .into_iter()
        .map(|row| SavedMessage {
            id: row.get("id"),
            message_id: row.get("message_id"),
            channel_id: row.get("channel_id"),
            channel_name: row.get("channel_name"),
            thread_root_id: row.get("thread_root_id"),
            sender_name: row.get("sender_name"),
            sender_role: row.get("sender_role"),
            body: row.get("body"),
            message_created_at: row.get("message_created_at"),
            created_at: row.get("created_at"),
        })
        .collect())
}

pub(crate) async fn load_message(pool: &SqlitePool, message_id: Uuid) -> CommandResult<Message> {
    let row = sqlx::query(
        r#"
        select
            m.id,
            m.channel_id,
            m.thread_root_id,
            m.sender_agent_id,
            m.sender_name,
            m.sender_role,
            m.body,
            m.is_task,
            m.thread_followed,
            m.delivery_state,
            m.stream_key,
            t.number as task_number,
            t.status as task_status,
            m.created_at,
            m.updated_at
        from messages m
        left join tasks t on t.message_id = m.id
        where m.id = $1
        "#,
    )
    .bind(message_id)
    .fetch_one(pool)
    .await
    .map_err(to_string)?;

    let mut message = Message {
        id: row.get("id"),
        channel_id: row.get("channel_id"),
        thread_root_id: row.get("thread_root_id"),
        sender_agent_id: row.get("sender_agent_id"),
        sender_name: row.get("sender_name"),
        sender_role: row.get("sender_role"),
        body: row.get("body"),
        is_task: row.get("is_task"),
        thread_followed: row.get("thread_followed"),
        delivery_state: row.get("delivery_state"),
        stream_key: row.get("stream_key"),
        task_number: row.get("task_number"),
        task_status: row.get("task_status"),
        attachments: Vec::new(),
        artifacts: Vec::new(),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    };
    attach_message_attachments(pool, std::slice::from_mut(&mut message)).await?;
    attach_message_artifacts(pool, std::slice::from_mut(&mut message)).await?;
    Ok(message)
}

pub(crate) async fn insert_agent_message(
    pool: &SqlitePool,
    agent_id: Uuid,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    body: &str,
    as_task: bool,
) -> CommandResult<Uuid> {
    insert_agent_message_with_options(
        pool,
        agent_id,
        channel_id,
        thread_root_id,
        body,
        as_task,
        true,
    )
    .await
}

pub(crate) async fn insert_agent_handoff_message(
    pool: &SqlitePool,
    agent_id: Uuid,
    channel_id: Uuid,
    thread_root_id: Uuid,
    body: &str,
) -> CommandResult<Uuid> {
    insert_agent_message_with_options(
        pool,
        agent_id,
        channel_id,
        Some(thread_root_id),
        body,
        false,
        false,
    )
    .await
}

pub(crate) async fn insert_agent_message_with_options(
    pool: &SqlitePool,
    agent_id: Uuid,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    body: &str,
    as_task: bool,
    dispatch_mentions: bool,
) -> CommandResult<Uuid> {
    if body.is_empty() {
        return Err("message event body is empty".to_owned());
    }
    if as_task && thread_root_id.is_some() {
        return Err("task message events must be root messages".to_owned());
    }
    if as_task {
        let channel_kind: Option<String> =
            sqlx::query_scalar("select kind from channels where id = $1")
                .bind(channel_id)
                .fetch_optional(pool)
                .await
                .map_err(to_string)?;
        if channel_kind.as_deref() == Some("dm") {
            return Err("direct messages do not support tasks".to_owned());
        }
    }
    if let Some(thread_root_id) = thread_root_id {
        let root_channel: Option<Uuid> = sqlx::query_scalar(
            "select channel_id from messages where id = $1 and thread_root_id is null",
        )
        .bind(thread_root_id)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?;
        if root_channel != Some(channel_id) {
            return Err("thread_root_id does not belong to target channel".to_owned());
        }
    }

    let sender = sqlx::query("select display_name, role from agents where id = $1")
        .bind(agent_id)
        .fetch_one(pool)
        .await
        .map_err(to_string)?;
    let sender_name: String = sender.get("display_name");
    let sender_role: String = sender.get("role");

    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(to_string)?;
    let msg_id: Uuid = sqlx::query_scalar(
        r#"
        insert into messages (
            channel_id, thread_root_id, sender_agent_id, sender_name, sender_role, body, is_task
        )
        values ($1, $2, $3, $4, $5, $6, $7)
        returning id
        "#,
    )
    .bind(channel_id)
    .bind(thread_root_id)
    .bind(agent_id)
    .bind(sender_name)
    .bind(sender_role)
    .bind(body)
    .bind(as_task)
    .fetch_one(&mut *tx)
    .await
    .map_err(to_string)?;

    if as_task {
        sqlx::query(
            r#"
            insert into tasks (message_id, channel_id, title, status, assignee_agent_id)
            values ($1, $2, $3, 'todo', $4)
            "#,
        )
        .bind(msg_id)
        .bind(channel_id)
        .bind(body.lines().next().unwrap_or("Untitled task"))
        .bind(agent_id)
        .execute(&mut *tx)
        .await
        .map_err(to_string)?;
    }

    tx.commit().await.map_err(to_string)?;
    let conversation_thread_root_id = thread_root_id.unwrap_or(msg_id);
    upsert_agent_thread_subscription(
        pool,
        agent_id,
        channel_id,
        conversation_thread_root_id,
        if as_task {
            "task_message"
        } else {
            "agent_message"
        },
        Some(msg_id),
    )
    .await?;
    if !as_task && dispatch_mentions {
        queue_agent_message_mentions(pool, msg_id).await?;
    }
    let _ = notify_ui_refresh(pool, "message").await;
    Ok(msg_id)
}

pub(crate) async fn send_owner_message_in_pool(
    pool: &SqlitePool,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    body: &str,
    as_task: bool,
    attachments: Vec<AttachmentUpload>,
) -> CommandResult<()> {
    if body.trim().is_empty() && attachments.is_empty() {
        return Err("message body or attachment is required".to_owned());
    }
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(to_string)?;
    let channel_kind: Option<String> =
        sqlx::query_scalar("select kind from channels where id = $1")
            .bind(channel_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(to_string)?;
    let Some(channel_kind) = channel_kind else {
        return Err("channel does not exist".to_owned());
    };
    if as_task && channel_kind == "dm" {
        return Err("direct messages do not support tasks".to_owned());
    }

    let owner_display_name =
        sqlx::query_scalar::<_, String>("select display_name from owner_profile where id = 1")
            .fetch_optional(&mut *tx)
            .await
            .map_err(to_string)?
            .unwrap_or_else(|| DEFAULT_OWNER_DISPLAY_NAME.to_owned());

    let msg_id: Uuid = sqlx::query_scalar(
        r#"
        insert into messages (channel_id, thread_root_id, sender_name, sender_role, body, is_task)
        values ($1, $2, $3, 'owner', $4, $5)
        returning id
        "#,
    )
    .bind(channel_id)
    .bind(thread_root_id)
    .bind(owner_display_name)
    .bind(body.trim())
    .bind(as_task)
    .fetch_one(&mut *tx)
    .await
    .map_err(to_string)?;

    insert_message_attachments_tx(&mut tx, msg_id, attachments).await?;

    let mut task_id = None;
    if as_task {
        task_id = Some(
            sqlx::query_scalar(
                r#"
            insert into tasks (message_id, channel_id, title, status)
            values ($1, $2, $3, 'todo')
            returning id
            "#,
            )
            .bind(msg_id)
            .bind(channel_id)
            .bind(body.lines().next().unwrap_or("Untitled task"))
            .fetch_one(&mut *tx)
            .await
            .map_err(to_string)?,
        );
    }

    tx.commit().await.map_err(to_string)?;
    queue_mentions_as_work_items(
        pool,
        channel_id,
        thread_root_id,
        msg_id,
        task_id,
        body.trim(),
        MentionDispatchOrigin::Owner,
    )
    .await?;
    if let Some(task_id) = task_id {
        dispatch_unassigned_task_availability(pool, task_id).await?;
    }
    let _ = notify_ui_refresh(pool, "message").await;
    Ok(())
}

pub(crate) async fn update_message_in_pool(
    pool: &SqlitePool,
    message_id: Uuid,
    body: &str,
) -> CommandResult<()> {
    let body = body.trim();
    if body.is_empty() {
        return Err("message body is empty".to_owned());
    }

    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(to_string)?;
    let result = sqlx::query("update messages set body = $2, updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') where id = $1")
        .bind(message_id)
        .bind(body)
        .execute(&mut *tx)
        .await
        .map_err(to_string)?;
    if result.rows_affected() == 0 {
        return Err("message does not exist".to_owned());
    }

    sqlx::query(
        r#"
        update tasks
        set title = $2, updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where message_id = $1
        "#,
    )
    .bind(message_id)
    .bind(body.lines().next().unwrap_or("Untitled task"))
    .execute(&mut *tx)
    .await
    .map_err(to_string)?;

    tx.commit().await.map_err(to_string)?;
    Ok(())
}

pub(crate) async fn delete_message_in_pool(
    pool: &SqlitePool,
    message_id: Uuid,
) -> CommandResult<()> {
    let result = sqlx::query("delete from messages where id = $1")
        .bind(message_id)
        .execute(pool)
        .await
        .map_err(to_string)?;
    if result.rows_affected() == 0 {
        return Err("message does not exist".to_owned());
    }
    Ok(())
}

pub(crate) async fn set_message_saved_in_pool(
    pool: &SqlitePool,
    message_id: Uuid,
    saved: bool,
) -> CommandResult<()> {
    let exists: bool = sqlx::query_scalar("select exists(select 1 from messages where id = $1)")
        .bind(message_id)
        .fetch_one(pool)
        .await
        .map_err(to_string)?;
    if !exists {
        return Err("message does not exist".to_owned());
    }

    if saved {
        sqlx::query(
            r#"
            insert into saved_messages (message_id)
            values ($1)
            on conflict (message_id) do nothing
            "#,
        )
        .bind(message_id)
        .execute(pool)
        .await
        .map_err(to_string)?;
    } else {
        sqlx::query("delete from saved_messages where message_id = $1")
            .bind(message_id)
            .execute(pool)
            .await
            .map_err(to_string)?;
    }
    let _ = notify_ui_refresh(pool, "saved_message_updated").await;
    Ok(())
}

pub(crate) async fn insert_message_attachments_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    message_id: Uuid,
    attachments: Vec<AttachmentUpload>,
) -> CommandResult<usize> {
    let mut inserted = 0;
    for attachment in attachments {
        if attachment.bytes.is_empty() {
            continue;
        }
        if attachment.bytes.len() > ATTACHMENT_SIZE_LIMIT {
            return Err(format!(
                "attachment {} is larger than 25MB",
                attachment.original_name
            ));
        }
        let attachment_id = Uuid::new_v4();
        let original_name = attachment.original_name.trim();
        let original_name = if original_name.is_empty() {
            "attachment"
        } else {
            original_name
        };
        let mime_type = attachment.mime_type.trim();
        let mime_type = if mime_type.is_empty() {
            "application/octet-stream"
        } else {
            mime_type
        };
        let storage_path =
            write_attachment_file(message_id, attachment_id, original_name, &attachment.bytes)?;
        sqlx::query(
            r#"
            insert into message_attachments (
                id,
                message_id,
                original_name,
                mime_type,
                size_bytes,
                storage_path
            )
            values ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(attachment_id)
        .bind(message_id)
        .bind(original_name)
        .bind(mime_type)
        .bind(attachment.bytes.len() as i64)
        .bind(storage_path)
        .execute(&mut **tx)
        .await
        .map_err(to_string)?;
        inserted += 1;
    }
    Ok(inserted)
}

pub(crate) async fn insert_agent_attachment_message(
    pool: &SqlitePool,
    agent_id: Uuid,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    body: &str,
    attachments: Vec<AttachmentUpload>,
) -> CommandResult<Uuid> {
    if attachments.is_empty() {
        return Err("attachment_create requires at least one file".to_owned());
    }
    let body = body.trim();
    if body.is_empty() {
        return Err("attachment_create body is empty".to_owned());
    }
    if let Some(thread_root_id) = thread_root_id {
        let root_channel: Option<Uuid> = sqlx::query_scalar(
            "select channel_id from messages where id = $1 and thread_root_id is null",
        )
        .bind(thread_root_id)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?;
        if root_channel != Some(channel_id) {
            return Err("thread_root_id does not belong to target channel".to_owned());
        }
    }

    let sender = sqlx::query("select display_name, role from agents where id = $1")
        .bind(agent_id)
        .fetch_one(pool)
        .await
        .map_err(to_string)?;
    let sender_name: String = sender.get("display_name");
    let sender_role: String = sender.get("role");

    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(to_string)?;
    let msg_id: Uuid = sqlx::query_scalar(
        r#"
        insert into messages (
            channel_id, thread_root_id, sender_agent_id, sender_name, sender_role, body, is_task
        )
        values ($1, $2, $3, $4, $5, $6, false)
        returning id
        "#,
    )
    .bind(channel_id)
    .bind(thread_root_id)
    .bind(agent_id)
    .bind(sender_name)
    .bind(sender_role)
    .bind(body)
    .fetch_one(&mut *tx)
    .await
    .map_err(to_string)?;

    let inserted = insert_message_attachments_tx(&mut tx, msg_id, attachments).await?;
    if inserted == 0 {
        return Err("attachment_create produced no attachments".to_owned());
    }
    tx.commit().await.map_err(to_string)?;

    let conversation_thread_root_id = thread_root_id.unwrap_or(msg_id);
    upsert_agent_thread_subscription(
        pool,
        agent_id,
        channel_id,
        conversation_thread_root_id,
        "agent_attachment_message",
        Some(msg_id),
    )
    .await?;
    queue_agent_message_mentions(pool, msg_id).await?;
    if let Ok(message) = load_message(pool, msg_id).await {
        let _ = notify_ui_message_upsert(pool, &message, "attachment_created").await;
    } else {
        let _ = notify_ui_refresh(pool, "attachment_created").await;
    }
    Ok(msg_id)
}

async fn attach_message_attachments(
    pool: &SqlitePool,
    messages: &mut [Message],
) -> CommandResult<()> {
    if messages.is_empty() {
        return Ok(());
    }
    let ids: Vec<Uuid> = messages.iter().map(|message| message.id).collect();
    let placeholders = (0..ids.len())
        .map(|index| format!("${}", index + 1))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        r#"
        select id, message_id, original_name, mime_type, size_bytes, storage_path, created_at
        from message_attachments
        where message_id in ({placeholders})
        order by created_at asc
        "#,
    );
    let mut query = sqlx::query(&sql);
    for id in &ids {
        query = query.bind(*id);
    }
    let rows = query.fetch_all(pool).await.map_err(to_string)?;
    let mut attachments_by_message: HashMap<Uuid, Vec<MessageAttachment>> = HashMap::new();
    for row in rows {
        let attachment = MessageAttachment {
            id: row.get("id"),
            message_id: row.get("message_id"),
            original_name: row.get("original_name"),
            mime_type: row.get("mime_type"),
            size_bytes: row.get("size_bytes"),
            storage_path: row.get("storage_path"),
            created_at: row.get("created_at"),
        };
        attachments_by_message
            .entry(attachment.message_id)
            .or_default()
            .push(attachment);
    }
    for message in messages {
        message.attachments = attachments_by_message
            .remove(&message.id)
            .unwrap_or_default();
    }
    Ok(())
}

fn artifact_from_row(row: &SqliteRow) -> Artifact {
    Artifact {
        id: row.get("id"),
        message_id: row.get("message_id"),
        channel_id: row.get("channel_id"),
        thread_root_id: row.get("thread_root_id"),
        creator_agent_id: row.get("creator_agent_id"),
        creator_agent_handle: row.get("creator_agent_handle"),
        kind: row.get("kind"),
        title: row.get("title"),
        summary: row.get("summary"),
        content: row.get("content"),
        metadata: row.get("metadata"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

pub(crate) async fn load_artifacts(pool: &SqlitePool) -> CommandResult<Vec<Artifact>> {
    let rows = sqlx::query(
        r#"
        select
            ar.id,
            ar.message_id,
            ar.channel_id,
            ar.thread_root_id,
            ar.creator_agent_id,
            a.handle as creator_agent_handle,
            ar.kind,
            ar.title,
            ar.summary,
            ar.content,
            ar.metadata,
            ar.created_at,
            ar.updated_at
        from artifacts ar
        left join agents a on a.id = ar.creator_agent_id
        order by ar.created_at asc
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(to_string)?;
    Ok(rows.iter().map(artifact_from_row).collect())
}

pub(crate) async fn load_artifact(pool: &SqlitePool, artifact_id: Uuid) -> CommandResult<Artifact> {
    let row = sqlx::query(
        r#"
        select
            ar.id,
            ar.message_id,
            ar.channel_id,
            ar.thread_root_id,
            ar.creator_agent_id,
            a.handle as creator_agent_handle,
            ar.kind,
            ar.title,
            ar.summary,
            ar.content,
            ar.metadata,
            ar.created_at,
            ar.updated_at
        from artifacts ar
        left join agents a on a.id = ar.creator_agent_id
        where ar.id = $1
        "#,
    )
    .bind(artifact_id)
    .fetch_one(pool)
    .await
    .map_err(to_string)?;
    Ok(artifact_from_row(&row))
}

async fn attach_message_artifacts(
    pool: &SqlitePool,
    messages: &mut [Message],
) -> CommandResult<()> {
    if messages.is_empty() {
        return Ok(());
    }
    let ids: Vec<Uuid> = messages.iter().map(|message| message.id).collect();
    let placeholders = (0..ids.len())
        .map(|index| format!("${}", index + 1))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        r#"
        select
            ar.id,
            ar.message_id,
            ar.channel_id,
            ar.thread_root_id,
            ar.creator_agent_id,
            a.handle as creator_agent_handle,
            ar.kind,
            ar.title,
            ar.summary,
            ar.content,
            ar.metadata,
            ar.created_at,
            ar.updated_at
        from artifacts ar
        left join agents a on a.id = ar.creator_agent_id
        where ar.message_id in ({placeholders})
        order by ar.created_at asc
        "#,
    );
    let mut query = sqlx::query(&sql);
    for id in &ids {
        query = query.bind(*id);
    }
    let rows = query.fetch_all(pool).await.map_err(to_string)?;
    let mut artifacts_by_message: HashMap<Uuid, Vec<Artifact>> = HashMap::new();
    for row in rows {
        let artifact = artifact_from_row(&row);
        artifacts_by_message
            .entry(artifact.message_id)
            .or_default()
            .push(artifact);
    }
    for message in messages {
        message.artifacts = artifacts_by_message.remove(&message.id).unwrap_or_default();
    }
    Ok(())
}
