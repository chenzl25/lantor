use std::collections::HashSet;

use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::app::{to_string, CommandResult};
use crate::events::activity::record_agent_activity;
use crate::models::{Channel, ChannelMember, ThreadActivity};
use crate::ui_notifications::notify_ui_refresh;

pub(crate) async fn load_channels(pool: &SqlitePool) -> CommandResult<Vec<Channel>> {
    let rows = sqlx::query(
        r#"
        select
            c.id,
            c.name,
            c.description,
            c.kind,
            c.dm_agent_id,
            cast(count(m.id) filter (
                where julianday(m.created_at) > julianday(
                    coalesce(r.last_read_at, '0001-01-01T00:00:00+00:00')
                )
                  and m.sender_role <> 'owner'
                  and m.delivery_state <> 'streaming'
                  and not (
                    m.sender_role <> 'system'
                    and m.delivery_state = 'complete'
                    and trim(m.body) = ''
                    and m.stream_key glob '????????-????-????-????-????????????:*'
                    and not exists (
                      select 1 from message_attachments ma where ma.message_id = m.id
                    )
                    and not exists (
                      select 1 from artifacts ar where ar.message_id = m.id
                    )
                  )
            ) as integer) as unread_count
        from channels c
        left join channel_read_state r on r.channel_id = c.id
        left join messages m on m.channel_id = c.id
        group by c.id, c.name, c.description, c.kind, c.dm_agent_id
        order by
          case
            when c.kind = 'channel' and c.name = 'lantor' then 0
            when c.kind = 'channel' then 1
            else 2
          end,
          c.name
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    Ok(rows
        .into_iter()
        .map(|row| Channel {
            id: row.get("id"),
            name: row.get("name"),
            description: row.get("description"),
            kind: row.get("kind"),
            dm_agent_id: row.get("dm_agent_id"),
            unread_count: row.get("unread_count"),
        })
        .collect())
}

pub(crate) async fn load_thread_activities(
    pool: &SqlitePool,
) -> CommandResult<Vec<ThreadActivity>> {
    let rows = sqlx::query(
        r#"
        with visible_thread_replies as (
            select
                m.id,
                m.channel_id,
                m.thread_root_id,
                m.sender_role,
                m.created_at
            from messages m
            where m.thread_root_id is not null
              and m.delivery_state <> 'streaming'
              and not (
                m.sender_role <> 'system'
                and m.sender_role <> 'owner'
                and m.stream_key glob '????????-????-????-????-????????????:*'
                and m.delivery_state = 'complete'
                and trim(m.body) = ''
                and not exists (
                  select 1 from message_attachments ma where ma.message_id = m.id
                )
                and not exists (
                  select 1 from artifacts ar where ar.message_id = m.id
                )
              )
        ),
        thread_read_markers as (
            select
                root.id as thread_root_id,
                case
                    when thread_read_state.read_until is null then read_state.last_read_at
                    when read_state.last_read_at is null then thread_read_state.read_until
                    when julianday(thread_read_state.read_until) >= julianday(read_state.last_read_at)
                        then thread_read_state.read_until
                    else read_state.last_read_at
                end as read_until
            from messages root
            left join channel_read_state read_state on read_state.channel_id = root.channel_id
            left join owner_inbox_read_state thread_read_state
              on thread_read_state.item_id = 'thread:' || lower(
                substr(hex(root.id), 1, 8) || '-' ||
                substr(hex(root.id), 9, 4) || '-' ||
                substr(hex(root.id), 13, 4) || '-' ||
                substr(hex(root.id), 17, 4) || '-' ||
                substr(hex(root.id), 21, 12)
              )
            where root.thread_root_id is null
        ),
        latest_visible_thread_replies as (
            select
                ranked.thread_root_id,
                ranked.id as latest_message_id,
                ranked.created_at as latest_activity_at
            from (
                select
                    vr.*,
                    row_number() over (
                        partition by vr.thread_root_id
                        order by julianday(vr.created_at) desc, vr.created_at desc, vr.id desc
                    ) as row_num
                from visible_thread_replies vr
            ) ranked
            where ranked.row_num = 1
        )
        select
            root.id as thread_root_id,
            root.channel_id,
            cast(sum(case
                when reply.sender_role <> 'owner'
                  and julianday(reply.created_at) > julianday(
                    coalesce(read_marker.read_until, '0001-01-01T00:00:00+00:00')
                  ) then 1
                else 0
            end) as integer) as unread_count,
            latest.latest_message_id,
            latest.latest_activity_at
        from messages root
        join visible_thread_replies reply on reply.thread_root_id = root.id
        join latest_visible_thread_replies latest on latest.thread_root_id = root.id
        left join thread_read_markers read_marker on read_marker.thread_root_id = root.id
        where root.thread_root_id is null
        group by
            root.id,
            root.channel_id,
            latest.latest_message_id,
            latest.latest_activity_at
        order by julianday(latest.latest_activity_at) desc, latest.latest_activity_at desc, root.id desc
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    Ok(rows
        .into_iter()
        .map(|row| ThreadActivity {
            thread_root_id: row.get("thread_root_id"),
            channel_id: row.get("channel_id"),
            unread_count: row.get("unread_count"),
            latest_message_id: row.get("latest_message_id"),
            latest_activity_at: row.get("latest_activity_at"),
        })
        .collect())
}

pub(crate) async fn load_channel_members(pool: &SqlitePool) -> CommandResult<Vec<ChannelMember>> {
    let rows = sqlx::query(
        r#"
        select
            m.channel_id,
            m.agent_id,
            a.handle as agent_handle,
            a.display_name as agent_display_name,
            m.created_at
        from channel_members m
        join agents a on a.id = m.agent_id
        join channels c on c.id = m.channel_id
        order by c.name, a.handle
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    Ok(rows
        .into_iter()
        .map(|row| ChannelMember {
            channel_id: row.get("channel_id"),
            agent_id: row.get("agent_id"),
            agent_handle: row.get("agent_handle"),
            agent_display_name: row.get("agent_display_name"),
            created_at: row.get("created_at"),
        })
        .collect())
}

pub(crate) async fn create_channel_in_pool(
    pool: &SqlitePool,
    name: &str,
    description: &str,
) -> CommandResult<Uuid> {
    let normalized = normalize_channel_name(name);
    if normalized.is_empty() {
        return Err("channel name is empty".to_owned());
    }
    ensure_channel_name_available(pool, &normalized, None).await?;
    let channel_id = sqlx::query_scalar(
        r#"
        insert into channels (name, description, kind)
        values ($1, $2, 'channel')
        returning id
        "#,
    )
    .bind(normalized)
    .bind(description.trim())
    .fetch_one(pool)
    .await
    .map_err(to_string)?;

    let _ = notify_ui_refresh(pool, "channel_created").await;
    Ok(channel_id)
}

pub(crate) async fn create_channel_with_members(
    pool: &SqlitePool,
    name: &str,
    description: &str,
    agent_ids: Option<Vec<Uuid>>,
) -> CommandResult<Uuid> {
    let channel_id = create_channel_in_pool(pool, name, description).await?;
    if let Some(ids) = agent_ids {
        let mut seen = HashSet::new();
        for agent_id in ids {
            if !seen.insert(agent_id) {
                continue;
            }
            add_agent_to_channel(pool, channel_id, agent_id).await?;
        }
    }
    Ok(channel_id)
}

pub(crate) async fn update_channel_in_pool(
    pool: &SqlitePool,
    channel_id: Uuid,
    name: String,
    description: String,
) -> CommandResult<()> {
    let normalized = normalize_channel_name(&name);
    if normalized.is_empty() {
        return Err("channel name is empty".to_owned());
    }

    let kind: Option<String> = sqlx::query_scalar("select kind from channels where id = $1")
        .bind(channel_id)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?;
    match kind.as_deref() {
        Some("dm") => return Err("direct messages cannot be renamed".to_owned()),
        Some(_) => {}
        None => return Err("channel does not exist".to_owned()),
    }
    ensure_channel_name_available(pool, &normalized, Some(channel_id)).await?;

    sqlx::query(
        r#"
        update channels
        set name = $2, description = $3
        where id = $1
        "#,
    )
    .bind(channel_id)
    .bind(normalized)
    .bind(description.trim())
    .execute(pool)
    .await
    .map_err(to_string)?;

    let _ = notify_ui_refresh(pool, "channel_updated").await;
    Ok(())
}

pub(crate) async fn set_channel_agent_membership_in_pool(
    pool: &SqlitePool,
    channel_id: Uuid,
    agent_id: Uuid,
    member: bool,
) -> CommandResult<()> {
    let channel_row = sqlx::query("select name, kind from channels where id = $1")
        .bind(channel_id)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?;
    let Some(channel_row) = channel_row else {
        return Err("channel does not exist".to_owned());
    };
    let channel_name: String = channel_row.get("name");
    let channel_kind: String = channel_row.get("kind");
    if channel_kind == "dm" {
        return Err("direct message membership is fixed".to_owned());
    }

    let agent_handle: Option<String> =
        sqlx::query_scalar("select handle from agents where id = $1")
            .bind(agent_id)
            .fetch_optional(pool)
            .await
            .map_err(to_string)?;
    if agent_handle.is_none() {
        return Err("agent does not exist".to_owned());
    }

    if member {
        sqlx::query(
            r#"
            insert into channel_members (channel_id, agent_id)
            values ($1, $2)
            on conflict (channel_id, agent_id) do nothing
            "#,
        )
        .bind(channel_id)
        .bind(agent_id)
        .execute(pool)
        .await
        .map_err(to_string)?;
    } else {
        sqlx::query("delete from channel_members where channel_id = $1 and agent_id = $2")
            .bind(channel_id)
            .bind(agent_id)
            .execute(pool)
            .await
            .map_err(to_string)?;
    }

    record_agent_activity(
        pool,
        Some(agent_id),
        None,
        "membership",
        if member {
            "Agent joined channel"
        } else {
            "Agent left channel"
        },
        format!("#{channel_name}"),
    )
    .await?;

    let _ = notify_ui_refresh(pool, "channel_membership_updated").await;
    Ok(())
}

pub(crate) async fn delete_channel_in_pool(
    pool: &SqlitePool,
    channel_id: Uuid,
) -> CommandResult<()> {
    let result = sqlx::query("delete from channels where id = $1")
        .bind(channel_id)
        .execute(pool)
        .await
        .map_err(to_string)?;
    if result.rows_affected() == 0 {
        return Err("channel does not exist".to_owned());
    }

    let _ = notify_ui_refresh(pool, "channel_deleted").await;

    Ok(())
}

pub(crate) async fn open_dm_with_agent_in_pool(
    pool: &SqlitePool,
    agent_id: Uuid,
) -> CommandResult<String> {
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(to_string)?;
    let agent_row = sqlx::query("select handle from agents where id = $1")
        .bind(agent_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(to_string)?;
    let Some(agent_row) = agent_row else {
        return Err("agent does not exist".to_owned());
    };
    let agent_handle: String = agent_row.get("handle");

    let existing: Option<Uuid> = sqlx::query_scalar(
        "select id from channels where kind = 'dm' and dm_agent_id = $1 limit 1",
    )
    .bind(agent_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(to_string)?;
    if let Some(channel_id) = existing {
        tx.commit().await.map_err(to_string)?;
        return Ok(channel_id.to_string());
    }

    let channel_id: Uuid = sqlx::query_scalar(
        r#"
        insert into channels (name, description, kind, dm_agent_id)
        values ($1, $2, 'dm', $3)
        returning id
        "#,
    )
    .bind(format!("dm:{agent_id}"))
    .bind(format!("Direct message with @{agent_handle}"))
    .bind(agent_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(to_string)?;

    sqlx::query(
        r#"
        insert into channel_members (channel_id, agent_id)
        values ($1, $2)
        on conflict (channel_id, agent_id) do nothing
        "#,
    )
    .bind(channel_id)
    .bind(agent_id)
    .execute(&mut *tx)
    .await
    .map_err(to_string)?;

    tx.commit().await.map_err(to_string)?;
    record_agent_activity(
        pool,
        Some(agent_id),
        None,
        "dm",
        "Direct message opened",
        format!("@{agent_handle}"),
    )
    .await?;

    let _ = notify_ui_refresh(pool, "dm_opened").await;
    Ok(channel_id.to_string())
}

async fn ensure_channel_name_available(
    pool: &SqlitePool,
    normalized: &str,
    excluding_channel_id: Option<Uuid>,
) -> CommandResult<()> {
    let existing_id: Option<Uuid> = match excluding_channel_id {
        Some(channel_id) => sqlx::query_scalar(
            r#"
            select id
            from channels
            where name = $1 and id <> $2
            limit 1
            "#,
        )
        .bind(normalized)
        .bind(channel_id)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?,
        None => sqlx::query_scalar(
            r#"
            select id
            from channels
            where name = $1
            limit 1
            "#,
        )
        .bind(normalized)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?,
    };
    if existing_id.is_some() {
        return Err(format!("channel #{normalized} already exists"));
    }
    Ok(())
}

pub(crate) async fn add_agent_to_channel(
    pool: &SqlitePool,
    channel_id: Uuid,
    agent_id: Uuid,
) -> CommandResult<()> {
    let kind: Option<String> = sqlx::query_scalar("select kind from channels where id = $1")
        .bind(channel_id)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?;
    if kind.as_deref() == Some("dm") {
        return Err("direct message membership is fixed".to_owned());
    }
    sqlx::query(
        r#"
        insert into channel_members (channel_id, agent_id)
        values ($1, $2)
        on conflict (channel_id, agent_id) do nothing
        "#,
    )
    .bind(channel_id)
    .bind(agent_id)
    .execute(pool)
    .await
    .map_err(to_string)?;
    let _ = notify_ui_refresh(pool, "channel_membership_updated").await;
    Ok(())
}

pub(crate) fn normalize_channel_name(name: &str) -> String {
    name.trim()
        .trim_start_matches('#')
        .to_lowercase()
        .replace(' ', "-")
}

#[cfg(test)]
mod tests {
    use std::fs as std_fs;

    use sqlx::{Row, SqlitePool};
    use uuid::Uuid;

    use super::{
        create_channel_in_pool, delete_channel_in_pool, load_channels, load_thread_activities,
        open_dm_with_agent_in_pool, set_channel_agent_membership_in_pool, update_channel_in_pool,
    };
    use crate::db::{db_connect_with_url, migrate};

    #[tokio::test]
    async fn channel_unread_count_compares_mixed_timezone_timestamps_by_instant() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let channel_id = insert_test_channel(&pool, "mixed-timezone-unread").await?;
            sqlx::query(
                r#"
                insert into channel_read_state (channel_id, last_read_at)
                values ($1, '2026-05-19T08:42:52.432+00:00')
                "#,
            )
            .bind(channel_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
            sqlx::query(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, created_at)
                values ($1, 'Dylan', 'owner', 'own message after read marker', '2026-05-19T08:42:52.433+00:00')
                "#,
            )
            .bind(channel_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
            sqlx::query(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, created_at)
                values ($1, 'Agent', 'agent', 'older in absolute time', '2026-05-19T13:20:09.372481+08:00')
                "#,
            )
            .bind(channel_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let channels = load_channels(&pool).await?;
            let channel = channels
                .iter()
                .find(|channel| channel.id == channel_id)
                .ok_or_else(|| "missing test channel".to_owned())?;
            assert_eq!(channel.unread_count, 0);

            sqlx::query(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, created_at)
                values ($1, 'Agent', 'agent', 'newer in absolute time', '2026-05-19T17:00:00+08:00')
                "#,
            )
            .bind(channel_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let channels = load_channels(&pool).await?;
            let channel = channels
                .iter()
                .find(|channel| channel.id == channel_id)
                .ok_or_else(|| "missing test channel".to_owned())?;
            assert_eq!(channel.unread_count, 1);

            let bodies: Vec<String> = sqlx::query_scalar(
                r#"
                select body
                from messages
                where channel_id = $1
                order by julianday(created_at) asc, created_at asc
                "#,
            )
            .bind(channel_id)
            .fetch_all(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(
                bodies,
                vec![
                    "older in absolute time",
                    "own message after read marker",
                    "newer in absolute time",
                ]
            );
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn channel_unread_count_excludes_progress_only_complete_messages() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let channel_id = insert_test_channel(&pool, "progress-only-unread").await?;
            sqlx::query(
                r#"
                insert into channel_read_state (channel_id, last_read_at)
                values ($1, '2026-05-22T08:00:00+00:00')
                "#,
            )
            .bind(channel_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            sqlx::query(
                r#"
                insert into messages (
                    channel_id, sender_name, sender_role, body, delivery_state, stream_key, created_at
                )
                values (
                    $1,
                    'Dylan',
                    'agent',
                    '',
                    'complete',
                    '11111111-1111-1111-1111-111111111111:msg_progress',
                    '2026-05-22T08:01:00+00:00'
                )
                "#,
            )
            .bind(channel_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let channels = load_channels(&pool).await?;
            let channel = channels
                .iter()
                .find(|channel| channel.id == channel_id)
                .ok_or_else(|| "missing test channel".to_owned())?;
            assert_eq!(channel.unread_count, 0);

            let attachment_message_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (
                    channel_id, sender_name, sender_role, body, delivery_state, stream_key, created_at
                )
                values (
                    $1,
                    'Dylan',
                    'agent',
                    '',
                    'complete',
                    '22222222-2222-2222-2222-222222222222:msg_attachment',
                    '2026-05-22T08:02:00+00:00'
                )
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            sqlx::query(
                r#"
                insert into message_attachments (
                    message_id, original_name, mime_type, size_bytes, storage_path
                )
                values ($1, 'screenshot.png', 'image/png', 42, '/tmp/screenshot.png')
                "#,
            )
            .bind(attachment_message_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let artifact_message_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (
                    channel_id, sender_name, sender_role, body, delivery_state, stream_key, created_at
                )
                values (
                    $1,
                    'Dylan',
                    'agent',
                    '',
                    'complete',
                    '33333333-3333-3333-3333-333333333333:msg_artifact',
                    '2026-05-22T08:03:00+00:00'
                )
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            sqlx::query(
                r#"
                insert into artifacts (message_id, channel_id, kind, title, content)
                values ($1, $2, 'markdown', 'Report', 'Visible artifact')
                "#,
            )
            .bind(artifact_message_id)
            .bind(channel_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let channels = load_channels(&pool).await?;
            let channel = channels
                .iter()
                .find(|channel| channel.id == channel_id)
                .ok_or_else(|| "missing test channel".to_owned())?;
            assert_eq!(channel.unread_count, 2);

            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn thread_activities_report_latest_visible_reply_and_unread_counts() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let channel_id = insert_test_channel(&pool, "thread-unread-activity").await?;
            sqlx::query(
                r#"
                insert into channel_read_state (channel_id, last_read_at)
                values ($1, '2026-05-22T08:00:00+00:00')
                "#,
            )
            .bind(channel_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let thread_a_root: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, created_at)
                values ($1, 'Martin', 'owner', 'thread a root', '2026-05-22T07:00:00+00:00')
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            sqlx::query(
                r#"
                insert into messages (
                    channel_id, thread_root_id, sender_name, sender_role, body, created_at
                )
                values ($1, $2, 'Dylan', 'agent', 'already read reply', '2026-05-22T07:30:00+00:00')
                "#,
            )
            .bind(channel_id)
            .bind(thread_a_root)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            sqlx::query(
                r#"
                insert into messages (
                    channel_id, thread_root_id, sender_name, sender_role, body, delivery_state, stream_key, created_at
                )
                values (
                    $1,
                    $2,
                    'Dylan',
                    'agent',
                    '',
                    'complete',
                    'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa:msg_hidden',
                    '2026-05-22T08:10:00+00:00'
                )
                "#,
            )
            .bind(channel_id)
            .bind(thread_a_root)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            sqlx::query(
                r#"
                insert into messages (
                    channel_id, thread_root_id, sender_name, sender_role, body, created_at
                )
                values ($1, $2, 'Martin', 'owner', 'owner follow-up should not be unread', '2026-05-22T08:12:00+00:00')
                "#,
            )
            .bind(channel_id)
            .bind(thread_a_root)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let thread_a_latest_reply: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (
                    channel_id, thread_root_id, sender_name, sender_role, body, created_at
                )
                values ($1, $2, 'Dylan', 'agent', 'visible unread reply', '2026-05-22T08:15:00+00:00')
                returning id
                "#,
            )
            .bind(channel_id)
            .bind(thread_a_root)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let thread_b_root: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, created_at)
                values ($1, 'Martin', 'owner', 'thread b root', '2026-05-22T07:05:00+00:00')
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let thread_b_latest_reply: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (
                    channel_id, thread_root_id, sender_name, sender_role, body, created_at
                )
                values ($1, $2, 'OtherAgent', 'agent', 'second thread latest', '2026-05-22T08:05:00+00:00')
                returning id
                "#,
            )
            .bind(channel_id)
            .bind(thread_b_root)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let thread_c_root: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, created_at)
                values ($1, 'Martin', 'owner', 'thread c root', '2026-05-22T07:08:00+00:00')
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            sqlx::query(
                r#"
                insert into messages (
                    channel_id, thread_root_id, sender_name, sender_role, body, created_at
                )
                values ($1, $2, 'OtherAgent', 'agent', 'thread read before marker', '2026-05-22T08:05:00+00:00')
                "#,
            )
            .bind(channel_id)
            .bind(thread_c_root)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let thread_c_latest_reply: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (
                    channel_id, thread_root_id, sender_name, sender_role, body, created_at
                )
                values ($1, $2, 'OtherAgent', 'agent', 'thread unread after marker', '2026-05-22T08:12:30+00:00')
                returning id
                "#,
            )
            .bind(channel_id)
            .bind(thread_c_root)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            sqlx::query(
                r#"
                insert into owner_inbox_read_state (item_id, read_until)
                values ($1, '2026-05-22T08:10:00+00:00')
                "#,
            )
            .bind(format!("thread:{thread_c_root}"))
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let thread_hidden_only_root: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, created_at)
                values ($1, 'Martin', 'owner', 'hidden-only root', '2026-05-22T07:10:00+00:00')
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            sqlx::query(
                r#"
                insert into messages (
                    channel_id, thread_root_id, sender_name, sender_role, body, delivery_state, stream_key, created_at
                )
                values (
                    $1,
                    $2,
                    'Dylan',
                    'agent',
                    '',
                    'streaming',
                    'bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb:msg_streaming',
                    '2026-05-22T08:20:00+00:00'
                )
                "#,
            )
            .bind(channel_id)
            .bind(thread_hidden_only_root)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            sqlx::query(
                r#"
                insert into messages (
                    channel_id, thread_root_id, sender_name, sender_role, body, delivery_state, stream_key, created_at
                )
                values (
                    $1,
                    $2,
                    'Dylan',
                    'agent',
                    'non-uuid streaming should not count',
                    'streaming',
                    'manual-stream-key',
                    '2026-05-22T08:25:00+00:00'
                )
                "#,
            )
            .bind(channel_id)
            .bind(thread_hidden_only_root)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let activities = load_thread_activities(&pool).await?;
            assert_eq!(activities.len(), 3);

            let first = &activities[0];
            assert_eq!(first.thread_root_id, thread_a_root);
            assert_eq!(first.channel_id, channel_id);
            assert_eq!(first.unread_count, 1);
            assert_eq!(first.latest_message_id, thread_a_latest_reply);

            let second = &activities[1];
            assert_eq!(second.thread_root_id, thread_c_root);
            assert_eq!(second.channel_id, channel_id);
            assert_eq!(second.unread_count, 1);
            assert_eq!(second.latest_message_id, thread_c_latest_reply);

            let third = &activities[2];
            assert_eq!(third.thread_root_id, thread_b_root);
            assert_eq!(third.channel_id, channel_id);
            assert_eq!(third.unread_count, 1);
            assert_eq!(third.latest_message_id, thread_b_latest_reply);

            assert!(activities
                .iter()
                .all(|activity| activity.thread_root_id != thread_hidden_only_root));

            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn thread_activities_use_newer_channel_read_marker_over_older_thread_marker() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let channel_id = insert_test_channel(&pool, "thread-read-marker-max").await?;
            sqlx::query(
                r#"
                insert into channel_read_state (channel_id, last_read_at)
                values ($1, '2026-05-22T08:30:00+00:00')
                "#,
            )
            .bind(channel_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let thread_root: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, created_at)
                values ($1, 'Martin', 'owner', 'thread root', '2026-05-22T07:00:00+00:00')
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let latest_reply: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (
                    channel_id, thread_root_id, sender_name, sender_role, body, created_at
                )
                values ($1, $2, 'Dylan', 'agent', 'reply after old thread marker', '2026-05-22T08:20:00+00:00')
                returning id
                "#,
            )
            .bind(channel_id)
            .bind(thread_root)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            sqlx::query(
                r#"
                insert into owner_inbox_read_state (item_id, read_until)
                values ($1, '2026-05-22T08:10:00+00:00')
                "#,
            )
            .bind(format!("thread:{thread_root}"))
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let activities = load_thread_activities(&pool).await?;
            let activity = activities
                .iter()
                .find(|activity| activity.thread_root_id == thread_root)
                .ok_or_else(|| "missing thread activity".to_owned())?;
            assert_eq!(activity.latest_message_id, latest_reply);
            assert_eq!(activity.unread_count, 0);

            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn create_channel_rejects_duplicate_name() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let channel_id = create_channel_in_pool(&pool, "Project Room", "original").await?;
            let duplicate = create_channel_in_pool(&pool, "#project room", "updated").await;
            let err = match duplicate {
                Ok(_) => return Err("duplicate channel create succeeded".to_owned()),
                Err(err) => err,
            };
            assert_eq!(err, "channel #project-room already exists");

            let description: String =
                sqlx::query_scalar("select description from channels where id = $1")
                    .bind(channel_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(description, "original");
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn rename_channel_rejects_duplicate_name() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let alpha_id = create_channel_in_pool(&pool, "alpha", "").await?;
            let beta_id = create_channel_in_pool(&pool, "beta", "").await?;
            update_channel_in_pool(
                &pool,
                alpha_id,
                "#Alpha".to_owned(),
                "self rename".to_owned(),
            )
            .await?;

            let duplicate =
                update_channel_in_pool(&pool, beta_id, "alpha".to_owned(), "duplicate".to_owned())
                    .await;
            let err = match duplicate {
                Ok(_) => return Err("duplicate channel rename succeeded".to_owned()),
                Err(err) => err,
            };
            assert_eq!(err, "channel #alpha already exists");
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn set_channel_agent_membership_updates_regular_channels() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "membership-agent").await?;
            let channel_id = insert_test_channel(&pool, "membership-channel").await?;

            set_channel_agent_membership_in_pool(&pool, channel_id, agent_id, true).await?;
            let joined: i64 = sqlx::query_scalar(
                "select count(*) from channel_members where channel_id = $1 and agent_id = $2",
            )
            .bind(channel_id)
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(joined, 1);

            set_channel_agent_membership_in_pool(&pool, channel_id, agent_id, false).await?;
            let left: i64 = sqlx::query_scalar(
                "select count(*) from channel_members where channel_id = $1 and agent_id = $2",
            )
            .bind(channel_id)
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(left, 0);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn dm_open_is_idempotent_and_cascades_with_agent() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "dm-idempotent").await?;
            let dm1 = open_dm_with_agent_in_pool(&pool, agent_id).await?;
            let dm2 = open_dm_with_agent_in_pool(&pool, agent_id).await?;
            assert_eq!(dm1, dm2);

            let dm_channel_id = Uuid::parse_str(&dm1).map_err(|err| err.to_string())?;
            let row = sqlx::query(
                r#"
                select c.kind, c.dm_agent_id, count(m.agent_id) as members
                from channels c
                left join channel_members m on m.channel_id = c.id
                where c.id = $1
                group by c.kind, c.dm_agent_id
                "#,
            )
            .bind(dm_channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let kind: String = row.get("kind");
            let dm_agent_id: Uuid = row.get("dm_agent_id");
            let members: i64 = row.get("members");
            assert_eq!(kind, "dm");
            assert_eq!(dm_agent_id, agent_id);
            assert_eq!(members, 1);

            sqlx::query("delete from agents where id = $1")
                .bind(agent_id)
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            let remaining: i64 = sqlx::query_scalar("select count(*) from channels where id = $1")
                .bind(dm_channel_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
            assert_eq!(remaining, 0);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn delete_channel_removes_timeline_and_unlinks_requests() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "channel-delete-agent").await?;
            let channel_id = insert_test_channel(&pool, "channel-delete").await?;
            sqlx::query("insert into channel_members (channel_id, agent_id) values ($1, $2)")
                .bind(channel_id)
                .bind(agent_id)
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            let message_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'delete channel body', true)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            sqlx::query(
                r#"
                insert into tasks (message_id, channel_id, title, status, assignee_agent_id)
                values ($1, $2, 'delete channel task', 'in_progress', $3)
                "#,
            )
            .bind(message_id)
            .bind(channel_id)
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
            sqlx::query(
                r#"
                insert into reminders (channel_id, creator_agent_id, title, due_at)
                values ($1, $2, 'channel reminder', strftime('%Y-%m-%dT%H:%M:%f+00:00','now','+1 hour'))
                "#,
            )
            .bind(channel_id)
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let work_item_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_work_items (agent_id, channel_id, source_message_id, title, context, status)
                values ($1, $2, $3, 'request', 'context', 'queued')
                returning id
                "#,
            )
            .bind(agent_id)
            .bind(channel_id)
            .bind(message_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            delete_channel_in_pool(&pool, channel_id).await?;

            let channel_count: i64 =
                sqlx::query_scalar("select count(*) from channels where id = $1")
                    .bind(channel_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(channel_count, 0);
            let message_count: i64 =
                sqlx::query_scalar("select count(*) from messages where id = $1")
                    .bind(message_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(message_count, 0);
            let unlinked_work_item_channel: Option<Uuid> =
                sqlx::query_scalar("select channel_id from agent_work_items where id = $1")
                    .bind(work_item_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(unlinked_work_item_channel, None);
            let reminder_channel_count: i64 = sqlx::query_scalar(
                "select count(*) from reminders where title = 'channel reminder' and channel_id is null",
            )
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(reminder_channel_count, 1);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

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

    async fn insert_test_agent(pool: &SqlitePool, handle: &str) -> Result<Uuid, String> {
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
}
