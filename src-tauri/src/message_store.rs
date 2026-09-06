mod search;

pub(crate) use search::search_messages_without_artifact_content;

use std::collections::HashMap;

use sqlx::{sqlite::SqliteRow, Row, Sqlite, SqlitePool, Transaction};
use uuid::Uuid;

use crate::agent_profile::DEFAULT_OWNER_DISPLAY_NAME;
use crate::agent_routing::{
    queue_agent_message_mentions, queue_mentions_as_work_items, upsert_agent_thread_subscription,
    MentionDispatchOrigin,
};
use crate::agent_work_dispatch::dispatch_unassigned_task_availability;
use crate::attachments::{
    attachment_exceeds_size_limit, persist_attachment_upload, remove_attachment_files,
    PendingAttachmentWrites, ATTACHMENT_SIZE_LIMIT_MIB,
};
use crate::ui_notifications::{enqueue_ui_event_in_tx, UiEvent};
use crate::{
    app::{to_string, CommandResult},
    models::{
        Artifact, AttachmentUpload, ChannelMessageHistory, ChannelMessagePage, Message,
        MessageAttachment, SavedMessage,
    },
};

pub(crate) const WEB_BOOTSTRAP_ROOT_MESSAGES_PER_CHANNEL: i64 = 80;
pub(crate) const CHANNEL_PREVIEW_ROOT_MESSAGES_PER_CHANNEL: i64 = 3;
const CHANNEL_PREVIEW_REPLIES_PER_THREAD: i64 = 12;
const ACTIVITY_FEED_THREAD_LIMIT: i64 = 120;
const ACTIVITY_FEED_MENTION_LIMIT: i64 = 120;
const MAX_OLDER_CHANNEL_ROOT_MESSAGES_PER_PAGE: i64 = 100;

pub(crate) const COMPACT_BOOTSTRAP_ROOTS: i64 = 20;
const COMPACT_BOOTSTRAP_REPLIES: i64 = 2;

pub(crate) async fn load_bootstrap_messages(
    pool: &SqlitePool,
    channel_id: Option<Uuid>,
    roots: i64,
) -> CommandResult<Vec<Message>> {
    let rows = sqlx::query(
        r#"
        with roots as (
            select id, channel_id, row_number() over (partition by channel_id order by seq desc) as rank
            from messages where thread_root_id is null and ($1 is null or channel_id = $1)
        ), selected as (
            select id from roots where rank <= $2
            union all
            select reply.id from roots root join messages reply on reply.id in (
                select id from messages where thread_root_id = root.id order by seq desc limit $3
            ) where root.rank <= $2
        )
        select m.*, t.number as task_number, t.status as task_status
        from messages m left join tasks t on t.message_id = m.id
        where m.id in (select id from selected) order by m.seq
        "#,
    ).bind(channel_id).bind(roots).bind(COMPACT_BOOTSTRAP_REPLIES)
        .fetch_all(pool).await.map_err(to_string)?;
    messages_from_rows(pool, rows, false).await
}

pub(crate) async fn load_thread_messages_in_pool(
    pool: &SqlitePool,
    thread_root_id: Uuid,
) -> CommandResult<Vec<Message>> {
    let rows = sqlx::query(
        r#"select m.*, t.number as task_number, t.status as task_status
        from messages m left join tasks t on t.message_id = m.id
        where m.id = $1 or m.thread_root_id = $1 order by m.seq"#,
    )
    .bind(thread_root_id)
    .fetch_all(pool)
    .await
    .map_err(to_string)?;
    messages_from_rows(pool, rows, false).await
}

pub(crate) async fn load_messages(pool: &SqlitePool) -> CommandResult<Vec<Message>> {
    load_messages_with_scope(pool, MessageLoadScope::All, true).await
}

pub(crate) async fn load_recent_channel_messages_without_artifact_content(
    pool: &SqlitePool,
    channel_id: Uuid,
    limit: i64,
) -> CommandResult<Vec<Message>> {
    load_messages_with_scope(
        pool,
        MessageLoadScope::RecentChannel(channel_id, limit),
        false,
    )
    .await
}

pub(crate) async fn load_channel_preview_messages_without_artifact_content(
    pool: &SqlitePool,
    roots_per_channel: i64,
) -> CommandResult<Vec<Message>> {
    let roots_per_channel = roots_per_channel.clamp(1, 5);
    let rows = sqlx::query(
        r#"
        with root_message_activity as (
            select
                coalesce(thread_root_id, id) as id,
                channel_id,
                max(seq) as latest_message_seq
            from messages
            group by channel_id, coalesce(thread_root_id, id)
        ),
        ranked_root_messages as (
            select
                id,
                row_number() over (
                    partition by channel_id
                    order by latest_message_seq desc
                ) as root_rank
            from root_message_activity
        ),
        base_context_message_ids as (
            select id
            from ranked_root_messages
            where root_rank <= $1
            union
            select source_message_id
            from agent_work_items
            where status in ('queued', 'running', 'cancelling')
              and source_message_id is not null
            union
            select thread_root_id
            from agent_work_items
            where status in ('queued', 'running', 'cancelling')
              and thread_root_id is not null
            union
            select id
            from messages
            where delivery_state = 'streaming'
            union
            select thread_root_id
            from messages
            where delivery_state = 'streaming'
              and thread_root_id is not null
        ),
        selected_thread_root_ids as (
            select context_message.id
            from messages context_message
            join base_context_message_ids selected on selected.id = context_message.id
            where context_message.thread_root_id is null
            union
            select context_message.thread_root_id
            from messages context_message
            join base_context_message_ids selected on selected.id = context_message.id
            where context_message.thread_root_id is not null
        ),
        ranked_thread_replies as (
            select
                message.id,
                row_number() over (
                    partition by message.thread_root_id
                    order by message.seq desc
                ) as reply_rank
            from messages message
            join selected_thread_root_ids selected on selected.id = message.thread_root_id
        ),
        selected_message_ids as (
            select id
            from selected_thread_root_ids
            union
            select id
            from ranked_thread_replies
            where reply_rank <= $2
            union
            select id
            from base_context_message_ids
        )
        select
            m.id,
            m.seq,
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
        where m.id in (select id from selected_message_ids)
        order by m.seq asc
        "#,
    )
    .bind(roots_per_channel)
    .bind(CHANNEL_PREVIEW_REPLIES_PER_THREAD)
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    messages_from_rows(pool, rows, false).await
}

pub(crate) async fn load_activity_messages_without_artifact_content(
    pool: &SqlitePool,
    mention_handles: &[String],
) -> CommandResult<Vec<Message>> {
    let mention_handles = mention_handles
        .iter()
        .map(|handle| handle.trim())
        .filter(|handle| !handle.is_empty())
        .take(16)
        .collect::<Vec<_>>();
    let mention_predicate = if mention_handles.is_empty() {
        "0".to_owned()
    } else {
        std::iter::repeat_n("instr(lower(m.body), lower(?)) > 0", mention_handles.len())
            .collect::<Vec<_>>()
            .join(" or ")
    };
    let sql = format!(
        r#"
        with visible_messages as (
            select m.*
            from messages m
            where m.delivery_state <> 'streaming'
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
        latest_channel_messages as (
            select ranked.id, ranked.thread_root_id
            from (
                select
                    m.id,
                    m.thread_root_id,
                    row_number() over (
                        partition by m.channel_id
                        order by m.seq desc
                    ) as message_rank
                from visible_messages m
            ) ranked
            where ranked.message_rank = 1
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
            from visible_messages root
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
        latest_thread_replies as (
            select
                ranked.thread_root_id,
                ranked.id as latest_message_id,
                ranked.created_at as latest_activity_at
            from (
                select
                    reply.id,
                    reply.thread_root_id,
                    reply.created_at,
                    reply.seq,
                    row_number() over (
                        partition by reply.thread_root_id
                        order by reply.seq desc
                    ) as reply_rank
                from visible_messages reply
                where reply.thread_root_id is not null
            ) ranked
            where ranked.reply_rank = 1
        ),
        activity_thread_roots as (
            select
                root.id as thread_root_id,
                latest.latest_message_id,
                latest.latest_activity_at
            from visible_messages root
            join latest_thread_replies latest on latest.thread_root_id = root.id
            left join thread_read_markers read_marker on read_marker.thread_root_id = root.id
            where root.thread_root_id is null
              and (
                root.thread_followed
                or exists (
                    select 1
                    from visible_messages unread_reply
                    where unread_reply.thread_root_id = root.id
                      and unread_reply.sender_role <> 'owner'
                      and julianday(unread_reply.created_at) > julianday(
                        coalesce(read_marker.read_until, '0001-01-01T00:00:00+00:00')
                      )
                )
              )
            order by julianday(latest.latest_activity_at) desc, latest.latest_activity_at desc
            limit ?
        ),
        recent_mentions as (
            select m.id, m.thread_root_id
            from visible_messages m
            where m.sender_role <> 'owner'
              and ({mention_predicate})
            order by m.seq desc
            limit ?
        ),
        selected_message_ids as (
            select id from latest_channel_messages
            union
            select thread_root_id from latest_channel_messages where thread_root_id is not null
            union
            select thread_root_id from activity_thread_roots
            union
            select latest_message_id from activity_thread_roots
            union
            select id from recent_mentions
            union
            select thread_root_id from recent_mentions where thread_root_id is not null
        )
        select
            m.id,
            m.seq,
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
        where m.id in (select id from selected_message_ids)
        order by m.seq asc
        "#,
    );
    let mut query = sqlx::query(&sql).bind(ACTIVITY_FEED_THREAD_LIMIT);
    for handle in mention_handles {
        query = query.bind(handle);
    }
    let rows = query
        .bind(ACTIVITY_FEED_MENTION_LIMIT)
        .fetch_all(pool)
        .await
        .map_err(to_string)?;

    messages_from_rows(pool, rows, false).await
}

pub(crate) async fn load_recent_channel_message_page_without_artifact_content(
    pool: &SqlitePool,
    channel_id: Uuid,
    limit: i64,
) -> CommandResult<ChannelMessagePage> {
    let messages =
        load_recent_channel_messages_without_artifact_content(pool, channel_id, limit).await?;
    let history = channel_message_history_from_messages(&messages, limit)
        .into_iter()
        .find(|history| history.channel_id == channel_id);
    Ok(ChannelMessagePage {
        messages,
        next_before_seq: history.as_ref().and_then(|history| history.before_seq),
        has_more: history.is_some_and(|history| history.has_more),
    })
}

pub(crate) async fn load_older_channel_messages_without_artifact_content(
    pool: &SqlitePool,
    channel_id: Uuid,
    before_seq: i64,
    limit: i64,
) -> CommandResult<ChannelMessagePage> {
    load_older_channel_messages_with_options(pool, channel_id, before_seq, limit, false).await
}

async fn load_older_channel_messages_with_options(
    pool: &SqlitePool,
    channel_id: Uuid,
    before_seq: i64,
    limit: i64,
    include_artifact_content: bool,
) -> CommandResult<ChannelMessagePage> {
    let limit = limit.clamp(1, MAX_OLDER_CHANNEL_ROOT_MESSAGES_PER_PAGE);
    let root_rows = sqlx::query(
        r#"
        select id, seq
        from messages
        where channel_id = $1
          and thread_root_id is null
          and seq < $2
        order by seq desc
        limit $3
        "#,
    )
    .bind(channel_id)
    .bind(before_seq)
    .bind(limit + 1)
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    let has_more = root_rows.len() > limit as usize;
    let selected_roots = root_rows
        .into_iter()
        .take(limit as usize)
        .collect::<Vec<_>>();
    let next_before_seq = selected_roots.last().map(|row| row.get::<i64, _>("seq"));
    if selected_roots.is_empty() {
        return Ok(ChannelMessagePage {
            messages: Vec::new(),
            next_before_seq: None,
            has_more: false,
        });
    }

    let root_ids = selected_roots
        .iter()
        .map(|row| row.get::<Uuid, _>("id"))
        .collect::<Vec<_>>();
    let placeholders = (0..root_ids.len())
        .map(|index| format!("${}", index + 1))
        .collect::<Vec<_>>()
        .join(", ");
    let channel_placeholder = format!("${}", root_ids.len() + 1);
    let sql = format!(
        r#"
        select
            m.id,
            m.seq,
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
        where m.channel_id = {channel_placeholder}
          and (
              m.id in ({placeholders})
              or m.thread_root_id in ({placeholders})
          )
        order by m.seq asc
        "#,
    );
    let mut query = sqlx::query(&sql);
    for root_id in &root_ids {
        query = query.bind(*root_id);
    }
    query = query.bind(channel_id);
    let rows = query.fetch_all(pool).await.map_err(to_string)?;
    let messages = messages_from_rows(pool, rows, include_artifact_content).await?;

    Ok(ChannelMessagePage {
        messages,
        next_before_seq,
        has_more,
    })
}

pub(crate) fn channel_message_history_from_messages(
    messages: &[Message],
    per_channel_limit: i64,
) -> Vec<ChannelMessageHistory> {
    let limit = per_channel_limit.max(0) as usize;
    if limit == 0 {
        return Vec::new();
    }
    let mut roots_by_channel: HashMap<Uuid, Vec<i64>> = HashMap::new();
    for message in messages
        .iter()
        .filter(|message| message.thread_root_id.is_none())
    {
        roots_by_channel
            .entry(message.channel_id)
            .or_default()
            .push(message.seq);
    }
    let mut history = roots_by_channel
        .into_iter()
        .map(|(channel_id, mut root_seqs)| {
            root_seqs.sort_unstable_by(|left, right| right.cmp(left));
            let may_have_more = root_seqs.len() >= limit;
            ChannelMessageHistory {
                channel_id,
                before_seq: may_have_more.then(|| root_seqs[limit - 1]),
                has_more: may_have_more,
            }
        })
        .collect::<Vec<_>>();
    history.sort_unstable_by_key(|entry| entry.channel_id);
    history
}

enum MessageLoadScope {
    All,
    RecentChannel(Uuid, i64),
}

async fn load_messages_with_scope(
    pool: &SqlitePool,
    scope: MessageLoadScope,
    include_artifact_content: bool,
) -> CommandResult<Vec<Message>> {
    let (sql, channel_id, limit) = match scope {
        MessageLoadScope::All => (
            r#"
            select
                m.id,
                m.seq,
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
            order by m.seq asc
            "#,
            None,
            None,
        ),
        MessageLoadScope::RecentChannel(channel_id, limit) => (
            r#"
            with recent_messages as (
                select id
                from messages
                where channel_id = $1
                order by seq desc
                limit $2
            ),
            recent_root_messages as (
                select id
                from messages
                where channel_id = $1
                  and thread_root_id is null
                order by seq desc
                limit $2
            ),
            recent_work_items as (
                select source_message_id, thread_root_id
                from agent_work_items
                where channel_id = $1
                order by julianday(created_at) desc, created_at desc
                limit 80
            ),
            base_selected_message_ids as (
                select id from recent_messages
                union
                select id from recent_root_messages
                union
                select saved.message_id
                from saved_messages saved
                join messages saved_message on saved_message.id = saved.message_id
                where saved_message.channel_id = $1
                union
                select message_id from tasks where tasks.channel_id = $1
                union
                select source_message_id from recent_work_items where source_message_id is not null
                union
                select thread_root_id from recent_work_items where thread_root_id is not null
            ),
            selected_thread_root_ids as (
                select id
                from base_selected_message_ids
                where id is not null
                union
                select m.thread_root_id
                from messages m
                join base_selected_message_ids selected on selected.id = m.id
                where m.thread_root_id is not null
            ),
            selected_message_ids as (
                select id
                from selected_thread_root_ids
                union
                select m.id
                from messages m
                join selected_thread_root_ids selected on selected.id = m.thread_root_id
            )
            select
                m.id,
                m.seq,
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
            where m.channel_id = $1
              and m.id in (select id from selected_message_ids)
            order by m.seq asc
            "#,
            Some(channel_id),
            Some(limit),
        ),
    };
    let mut query = sqlx::query(sql);
    if let Some(channel_id) = channel_id {
        query = query.bind(channel_id);
    }
    if let Some(limit) = limit {
        query = query.bind(limit);
    }
    let rows = query.fetch_all(pool).await.map_err(to_string)?;

    messages_from_rows(pool, rows, include_artifact_content).await
}

async fn messages_from_rows(
    pool: &SqlitePool,
    rows: Vec<SqliteRow>,
    include_artifact_content: bool,
) -> CommandResult<Vec<Message>> {
    let mut messages: Vec<Message> = rows.iter().map(message_from_row).collect();
    attach_message_attachments(pool, &mut messages).await?;
    attach_message_artifacts(pool, &mut messages, include_artifact_content).await?;
    Ok(messages)
}

fn message_from_row(row: &SqliteRow) -> Message {
    Message {
        id: row.get("id"),
        seq: row.get("seq"),
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
    }
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

async fn load_message_with_artifact_content(
    pool: &SqlitePool,
    message_id: Uuid,
    include_artifact_content: bool,
) -> CommandResult<Message> {
    let row = sqlx::query(
        r#"
        select
            m.id,
            m.seq,
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

    let mut message = message_from_row(&row);
    attach_message_attachments(pool, std::slice::from_mut(&mut message)).await?;
    attach_message_artifacts(
        pool,
        std::slice::from_mut(&mut message),
        include_artifact_content,
    )
    .await?;
    Ok(message)
}

pub(crate) async fn load_message(pool: &SqlitePool, message_id: Uuid) -> CommandResult<Message> {
    load_message_with_artifact_content(pool, message_id, true).await
}

pub(crate) async fn load_message_without_artifact_content(
    pool: &SqlitePool,
    message_id: Uuid,
) -> CommandResult<Message> {
    load_message_with_artifact_content(pool, message_id, false).await
}

pub(crate) async fn load_message_patch_in_tx(
    transaction: &mut Transaction<'_, Sqlite>,
    message_id: Uuid,
) -> CommandResult<Message> {
    let row = sqlx::query(
        r#"
        select
            m.id,
            m.seq,
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
    .fetch_one(&mut **transaction)
    .await
    .map_err(to_string)?;
    let mut message = message_from_row(&row);
    let attachment_rows = sqlx::query(
        r#"
        select id, message_id, original_name, mime_type, size_bytes, storage_path, created_at
        from message_attachments
        where message_id = $1
        order by created_at asc
        "#,
    )
    .bind(message_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(to_string)?;
    message.attachments = attachment_rows
        .into_iter()
        .map(|row| MessageAttachment {
            id: row.get("id"),
            message_id: row.get("message_id"),
            original_name: row.get("original_name"),
            mime_type: row.get("mime_type"),
            size_bytes: row.get("size_bytes"),
            storage_path: row.get("storage_path"),
            created_at: row.get("created_at"),
        })
        .collect();
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

    let message = load_message_patch_in_tx(&mut tx, msg_id).await?;
    enqueue_ui_event_in_tx(
        &mut tx,
        &UiEvent::MessageUpsert {
            reason: "message",
            message: &message,
        },
    )
    .await?;
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
    Ok(msg_id)
}

pub(crate) async fn send_owner_message_in_pool(
    pool: &SqlitePool,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    body: &str,
    as_task: bool,
    attachments: Vec<AttachmentUpload>,
) -> CommandResult<Message> {
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

    let pending_attachment_writes =
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

    let message = load_message_patch_in_tx(&mut tx, msg_id).await?;
    enqueue_ui_event_in_tx(
        &mut tx,
        &UiEvent::MessageUpsert {
            reason: "message",
            message: &message,
        },
    )
    .await?;
    tx.commit().await.map_err(to_string)?;
    pending_attachment_writes.commit();
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
    let message = load_message(pool, msg_id).await?;
    Ok(message)
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

    let message = load_message_patch_in_tx(&mut tx, message_id).await?;
    enqueue_ui_event_in_tx(
        &mut tx,
        &UiEvent::MessageUpsert {
            reason: "message_updated",
            message: &message,
        },
    )
    .await?;
    tx.commit().await.map_err(to_string)?;
    Ok(())
}

pub(crate) async fn delete_message_in_pool(
    pool: &SqlitePool,
    message_id: Uuid,
) -> CommandResult<()> {
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(to_string)?;
    let attachment_paths = sqlx::query_scalar::<_, String>(
        r#"
        select ma.storage_path
        from message_attachments ma
        join messages m on m.id = ma.message_id
        where m.id = $1 or m.thread_root_id = $1
        "#,
    )
    .bind(message_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(to_string)?;
    let result = sqlx::query("delete from messages where id = $1")
        .bind(message_id)
        .execute(&mut *tx)
        .await
        .map_err(to_string)?;
    if result.rows_affected() == 0 {
        return Err("message does not exist".to_owned());
    }
    enqueue_ui_event_in_tx(
        &mut tx,
        &UiEvent::MessageDelete {
            reason: "message_deleted",
            message_id,
        },
    )
    .await?;
    tx.commit().await.map_err(to_string)?;
    remove_attachment_files(&attachment_paths);
    Ok(())
}

pub(crate) async fn set_message_saved_in_pool(
    pool: &SqlitePool,
    message_id: Uuid,
    saved: bool,
) -> CommandResult<()> {
    let mut transaction = pool.begin().await.map_err(to_string)?;
    let exists: bool = sqlx::query_scalar("select exists(select 1 from messages where id = $1)")
        .bind(message_id)
        .fetch_one(&mut *transaction)
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
        .execute(&mut *transaction)
        .await
        .map_err(to_string)?;
    } else {
        sqlx::query("delete from saved_messages where message_id = $1")
            .bind(message_id)
            .execute(&mut *transaction)
            .await
            .map_err(to_string)?;
    }
    enqueue_ui_event_in_tx(
        &mut transaction,
        &UiEvent::Refresh {
            reason: "saved_message_updated",
        },
    )
    .await?;
    transaction.commit().await.map_err(to_string)?;
    Ok(())
}

pub(crate) async fn insert_message_attachments_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    message_id: Uuid,
    attachments: Vec<AttachmentUpload>,
) -> CommandResult<PendingAttachmentWrites> {
    let mut pending_writes = PendingAttachmentWrites::default();
    for attachment in attachments {
        let size_bytes = attachment.size_bytes();
        if size_bytes == 0 {
            continue;
        }
        if attachment_exceeds_size_limit(size_bytes) {
            return Err(format!(
                "attachment {} is larger than {ATTACHMENT_SIZE_LIMIT_MIB}MB",
                attachment.original_name,
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
        let storage_path = persist_attachment_upload(
            message_id,
            attachment_id,
            original_name,
            &attachment,
            &mut pending_writes,
        )
        .await?;
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
        .bind(size_bytes as i64)
        .bind(storage_path)
        .execute(&mut **tx)
        .await
        .map_err(to_string)?;
    }
    Ok(pending_writes)
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

    let pending_attachment_writes =
        insert_message_attachments_tx(&mut tx, msg_id, attachments).await?;
    if pending_attachment_writes.is_empty() {
        return Err("attachment_create produced no attachments".to_owned());
    }
    let message = load_message_patch_in_tx(&mut tx, msg_id).await?;
    enqueue_ui_event_in_tx(
        &mut tx,
        &UiEvent::MessageUpsert {
            reason: "attachment_created",
            message: &message,
        },
    )
    .await?;
    tx.commit().await.map_err(to_string)?;
    pending_attachment_writes.commit();

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
    load_artifacts_with_content(pool, true).await
}

pub(crate) async fn load_artifact_summaries(pool: &SqlitePool) -> CommandResult<Vec<Artifact>> {
    load_artifacts_with_content(pool, false).await
}

async fn load_artifacts_with_content(
    pool: &SqlitePool,
    include_content: bool,
) -> CommandResult<Vec<Artifact>> {
    let content_select = if include_content {
        "ar.content"
    } else {
        "'' as content"
    };
    let rows = sqlx::query(&format!(
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
            {content_select},
            ar.metadata,
            ar.created_at,
            ar.updated_at
        from artifacts ar
        left join agents a on a.id = ar.creator_agent_id
        order by ar.created_at asc
        "#,
    ))
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

pub(crate) async fn load_artifact_in_tx(
    transaction: &mut Transaction<'_, Sqlite>,
    artifact_id: Uuid,
) -> CommandResult<Artifact> {
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
    .fetch_one(&mut **transaction)
    .await
    .map_err(to_string)?;
    Ok(artifact_from_row(&row))
}

async fn attach_message_artifacts(
    pool: &SqlitePool,
    messages: &mut [Message],
    include_content: bool,
) -> CommandResult<()> {
    if messages.is_empty() {
        return Ok(());
    }
    let ids: Vec<Uuid> = messages.iter().map(|message| message.id).collect();
    let placeholders = (0..ids.len())
        .map(|index| format!("${}", index + 1))
        .collect::<Vec<_>>()
        .join(", ");
    let content_select = if include_content {
        "ar.content"
    } else {
        "'' as content"
    };
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
            {content_select},
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

#[cfg(test)]
mod tests {
    use crate::test_support::{
        drop_test_schema, insert_test_agent, insert_test_channel, test_pool,
    };

    use super::{
        channel_message_history_from_messages, delete_message_in_pool,
        load_activity_messages_without_artifact_content,
        load_channel_preview_messages_without_artifact_content, load_messages,
        load_older_channel_messages_without_artifact_content,
        load_recent_channel_message_page_without_artifact_content,
        load_recent_channel_messages_without_artifact_content,
        search_messages_without_artifact_content,
    };
    use crate::attachments::write_attachment_file;

    #[tokio::test]
    async fn deleting_a_thread_removes_its_attachment_files() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let channel_id = insert_test_channel(&pool, "delete-message-attachments").await?;
            let root_id: uuid::Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'root', false)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let reply_id: uuid::Uuid = sqlx::query_scalar(
                r#"
                insert into messages (
                    channel_id, thread_root_id, sender_name, sender_role, body, is_task
                )
                values ($1, $2, 'agent', 'agent', 'reply', false)
                returning id
                "#,
            )
            .bind(channel_id)
            .bind(root_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let mut storage_paths = Vec::new();
            for (message_id, name) in [(root_id, "root.txt"), (reply_id, "reply.txt")] {
                let attachment_id = uuid::Uuid::new_v4();
                let storage_path =
                    write_attachment_file(message_id, attachment_id, name, name.as_bytes())?;
                sqlx::query(
                    r#"
                    insert into message_attachments (
                        id, message_id, original_name, mime_type, size_bytes, storage_path
                    )
                    values ($1, $2, $3, 'text/plain', $4, $5)
                    "#,
                )
                .bind(attachment_id)
                .bind(message_id)
                .bind(name)
                .bind(name.len() as i64)
                .bind(&storage_path)
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
                storage_paths.push(storage_path);
            }

            delete_message_in_pool(&pool, root_id).await?;

            assert!(storage_paths
                .iter()
                .all(|path| !std::path::Path::new(path).exists()));
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn recent_channel_message_page_only_loads_the_requested_channel() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let requested_channel_id = insert_test_channel(&pool, "requested-channel").await?;
            let other_channel_id = insert_test_channel(&pool, "other-channel").await?;
            let requested_root_id: uuid::Uuid = sqlx::query_scalar(
                r#"
                insert into messages (
                    channel_id, sender_name, sender_role, body, is_task, created_at
                )
                values ($1, 'Dylan', 'owner', 'requested root', false, '2026-01-01T00:00:00.000+00:00')
                returning id
                "#,
            )
            .bind(requested_channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            sqlx::query(
                r#"
                insert into messages (
                    channel_id, thread_root_id, sender_name, sender_role, body, is_task, created_at
                )
                values ($1, $2, 'agent', 'agent', 'requested reply', false, '2026-01-01T00:00:01.000+00:00')
                "#,
            )
            .bind(requested_channel_id)
            .bind(requested_root_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
            sqlx::query(
                r#"
                insert into messages (
                    channel_id, sender_name, sender_role, body, is_task, created_at
                )
                values ($1, 'Dylan', 'owner', 'other root', false, '2026-01-01T00:00:02.000+00:00')
                "#,
            )
            .bind(other_channel_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let page = load_recent_channel_message_page_without_artifact_content(
                &pool,
                requested_channel_id,
                1,
            )
            .await?;
            assert_eq!(page.messages.len(), 2);
            assert!(page
                .messages
                .iter()
                .all(|message| message.channel_id == requested_channel_id));
            assert!(page
                .messages
                .iter()
                .any(|message| message.body == "requested reply"));
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn channel_preview_keeps_active_and_streaming_threads_beyond_the_recent_limit() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let active_channel_id = insert_test_channel(&pool, "preview-active").await?;
            let streaming_channel_id = insert_test_channel(&pool, "preview-streaming").await?;
            let agent_id = insert_test_agent(&pool, "preview-agent").await?;

            let active_root_id: uuid::Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'active old root', false)
                returning id
                "#,
            )
            .bind(active_channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            sqlx::query(
                r#"
                insert into messages (
                    channel_id, thread_root_id, sender_agent_id, sender_name,
                    sender_role, body, is_task
                )
                values ($1, $2, $3, 'preview-agent', 'agent', 'active old reply', false)
                "#,
            )
            .bind(active_channel_id)
            .bind(active_root_id)
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
            sqlx::query(
                r#"
                insert into agent_work_items (
                    agent_id, channel_id, source_message_id, title, status
                )
                values ($1, $2, $3, 'active preview work', 'running')
                "#,
            )
            .bind(agent_id)
            .bind(active_channel_id)
            .bind(active_root_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let streaming_root_id: uuid::Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'streaming old root', false)
                returning id
                "#,
            )
            .bind(streaming_channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            sqlx::query(
                r#"
                insert into messages (
                    channel_id, thread_root_id, sender_agent_id, sender_name,
                    sender_role, body, is_task, delivery_state, stream_key
                )
                values (
                    $1, $2, $3, 'preview-agent', 'agent', 'streaming old reply',
                    false, 'streaming', 'preview-stream'
                )
                "#,
            )
            .bind(streaming_channel_id)
            .bind(streaming_root_id)
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            for (channel_id, prefix) in [
                (active_channel_id, "active recent"),
                (streaming_channel_id, "streaming recent"),
            ] {
                for index in 1..=2 {
                    sqlx::query(
                        r#"
                        insert into messages (
                            channel_id, sender_name, sender_role, body, is_task
                        )
                        values ($1, 'Dylan', 'owner', $2, false)
                        "#,
                    )
                    .bind(channel_id)
                    .bind(format!("{prefix} {index}"))
                    .execute(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
                }
            }

            let preview = load_channel_preview_messages_without_artifact_content(&pool, 1).await?;
            let bodies = preview
                .iter()
                .map(|message| message.body.as_str())
                .collect::<std::collections::HashSet<_>>();

            assert!(bodies.contains("active old root"));
            assert!(bodies.contains("active old reply"));
            assert!(bodies.contains("streaming old root"));
            assert!(bodies.contains("streaming old reply"));
            assert!(bodies.contains("active recent 2"));
            assert!(bodies.contains("streaming recent 2"));
            assert!(!bodies.contains("active recent 1"));
            assert!(!bodies.contains("streaming recent 1"));
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn channel_preview_ranks_threads_by_latest_reply() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let channel_id = insert_test_channel(&pool, "preview-latest-reply").await?;
            let old_root_id: uuid::Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'old active root', false)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            sqlx::query(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'newer inactive root', false)
                "#,
            )
            .bind(channel_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
            sqlx::query(
                r#"
                insert into messages (
                    channel_id, thread_root_id, sender_name, sender_role, body, is_task
                )
                values ($1, $2, 'preview-agent', 'agent', 'latest completed reply', false)
                "#,
            )
            .bind(channel_id)
            .bind(old_root_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let preview = load_channel_preview_messages_without_artifact_content(&pool, 1).await?;
            let bodies = preview
                .iter()
                .map(|message| message.body.as_str())
                .collect::<Vec<_>>();
            assert_eq!(bodies, vec!["old active root", "latest completed reply"]);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn channel_preview_bounds_recent_replies_per_thread() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let channel_id = insert_test_channel(&pool, "preview-bounded-replies").await?;
            let root_id: uuid::Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'bounded root', false)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            for index in 1..=15 {
                sqlx::query(
                    r#"
                    insert into messages (
                        channel_id, thread_root_id, sender_name, sender_role, body, is_task
                    )
                    values ($1, $2, 'preview-agent', 'agent', $3, false)
                    "#,
                )
                .bind(channel_id)
                .bind(root_id)
                .bind(format!("reply {index}"))
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            }

            let preview = load_channel_preview_messages_without_artifact_content(&pool, 1).await?;
            assert_eq!(preview.len(), 13);
            assert!(preview.iter().any(|message| message.body == "bounded root"));
            assert!(preview.iter().any(|message| message.body == "reply 4"));
            assert!(preview.iter().any(|message| message.body == "reply 15"));
            assert!(!preview.iter().any(|message| message.body == "reply 3"));
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn recent_channel_messages_keep_limited_replies_and_their_roots() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let channel_id = insert_test_channel(&pool, "recent-messages").await?;
            let root_id: uuid::Uuid = sqlx::query_scalar(
                r#"
                insert into messages (
                    channel_id, sender_name, sender_role, body, is_task, created_at
                )
                values ($1, 'Dylan', 'owner', 'old root', false, '2026-01-01T00:00:00.000+00:00')
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            for index in 1..=3 {
                sqlx::query(
                    r#"
                    insert into messages (
                        channel_id, thread_root_id, sender_name, sender_role, body, is_task, created_at
                    )
                    values (
                        $1, $2, 'agent', 'agent', $3, false,
                        printf('2026-01-01T00:00:0%d.000+00:00', $4)
                    )
                    "#,
                )
                .bind(channel_id)
                .bind(root_id)
                .bind(format!("reply {index}"))
                .bind(index)
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            }

            let full = load_messages(&pool).await?;
            assert_eq!(full.len(), 4);

            let recent =
                load_recent_channel_messages_without_artifact_content(&pool, channel_id, 2)
                    .await?;
            let bodies: Vec<&str> = recent.iter().map(|message| message.body.as_str()).collect();
            assert_eq!(bodies, vec!["old root", "reply 1", "reply 2", "reply 3"]);
            assert!(recent.iter().any(|message| message.id == root_id));
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn recent_channel_messages_keep_limited_root_timeline_messages() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let channel_id = insert_test_channel(&pool, "recent-root-messages").await?;
            let _old_root_id: uuid::Uuid = sqlx::query_scalar(
                r#"
                insert into messages (
                    channel_id, sender_name, sender_role, body, is_task, created_at
                )
                values ($1, 'Dylan', 'owner', 'old root', false, '2026-01-01T00:00:00.000+00:00')
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let middle_root_id: uuid::Uuid = sqlx::query_scalar(
                r#"
                insert into messages (
                    channel_id, sender_name, sender_role, body, is_task, created_at
                )
                values ($1, 'Dylan', 'owner', 'middle root', false, '2026-01-01T00:00:01.000+00:00')
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let newest_root_id: uuid::Uuid = sqlx::query_scalar(
                r#"
                insert into messages (
                    channel_id, sender_name, sender_role, body, is_task, created_at
                )
                values ($1, 'Dylan', 'owner', 'newest root', false, '2026-01-01T00:00:02.000+00:00')
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            for index in 1..=5 {
                sqlx::query(
                    r#"
                    insert into messages (
                        channel_id, thread_root_id, sender_name, sender_role, body, is_task, created_at
                    )
                    values (
                        $1, $2, 'agent', 'agent', $3, false,
                        printf('2026-01-01T00:00:0%d.000+00:00', $4)
                    )
                    "#,
                )
                .bind(channel_id)
                .bind(newest_root_id)
                .bind(format!("reply {index}"))
                .bind(index + 2)
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            }

            let recent =
                load_recent_channel_messages_without_artifact_content(&pool, channel_id, 2)
                    .await?;
            let bodies: Vec<&str> = recent.iter().map(|message| message.body.as_str()).collect();
            assert!(bodies.contains(&"middle root"));
            assert!(bodies.contains(&"newest root"));
            assert!(bodies.contains(&"reply 4"));
            assert!(bodies.contains(&"reply 5"));
            assert!(!bodies.contains(&"old root"));
            assert!(recent.iter().any(|message| message.id == middle_root_id));
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn older_channel_messages_uses_seq_cursor_and_omits_artifact_content() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let channel_id = insert_test_channel(&pool, "older-channel-messages").await?;
            let mut roots = Vec::new();
            for index in 1..=4 {
                let id: uuid::Uuid = sqlx::query_scalar(
                    r#"
                    insert into messages (
                        channel_id, sender_name, sender_role, body, is_task, created_at
                    )
                    values ($1, 'Dylan', 'owner', $2, false, '2026-01-01T00:00:00.000+00:00')
                    returning id
                    "#,
                )
                .bind(channel_id)
                .bind(format!("root {index}"))
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
                let seq: i64 = sqlx::query_scalar("select seq from messages where id = $1")
                    .bind(id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
                roots.push((id, seq));
            }
            for index in 1..=2 {
                sqlx::query(
                    r#"
                    insert into messages (
                        channel_id, thread_root_id, sender_name, sender_role, body, is_task,
                        created_at
                    )
                    values (
                        $1, $2, 'agent', 'agent', $3, false,
                        '2026-01-01T00:00:00.000+00:00'
                    )
                    "#,
                )
                .bind(channel_id)
                .bind(roots[1].0)
                .bind(format!("root 2 reply {index}"))
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            }
            sqlx::query(
                r#"
                insert into artifacts (
                    message_id, channel_id, kind, title, summary, content, metadata
                )
                values ($1, $2, 'markdown', 'history artifact', 'artifact summary',
                        'large historical content', '{}')
                "#,
            )
            .bind(roots[1].0)
            .bind(channel_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let first_page = load_older_channel_messages_without_artifact_content(
                &pool, channel_id, roots[3].1, 2,
            )
            .await?;
            let first_page_roots = first_page
                .messages
                .iter()
                .filter(|message| message.thread_root_id.is_none())
                .map(|message| message.body.as_str())
                .collect::<Vec<_>>();
            assert_eq!(first_page_roots, vec!["root 2", "root 3"]);
            assert_eq!(first_page.next_before_seq, Some(roots[1].1));
            assert!(first_page.has_more);
            assert!(first_page
                .messages
                .iter()
                .any(|message| message.body == "root 2 reply 1"));
            let artifact = &first_page
                .messages
                .iter()
                .find(|message| message.id == roots[1].0)
                .expect("root 2 should be present")
                .artifacts[0];
            assert_eq!(artifact.title, "history artifact");
            assert!(artifact.content.is_empty());

            let second_page = load_older_channel_messages_without_artifact_content(
                &pool,
                channel_id,
                first_page.next_before_seq.expect("first page cursor"),
                2,
            )
            .await?;
            let second_page_roots = second_page
                .messages
                .iter()
                .filter(|message| message.thread_root_id.is_none())
                .map(|message| message.body.as_str())
                .collect::<Vec<_>>();
            assert_eq!(second_page_roots, vec!["root 1"]);
            assert!(!second_page.has_more);

            let loaded_root_ids = first_page
                .messages
                .iter()
                .chain(second_page.messages.iter())
                .filter(|message| message.thread_root_id.is_none())
                .map(|message| message.id)
                .collect::<std::collections::HashSet<_>>();
            assert_eq!(loaded_root_ids.len(), 3);
            assert!(loaded_root_ids.contains(&roots[0].0));
            assert!(loaded_root_ids.contains(&roots[1].0));
            assert!(loaded_root_ids.contains(&roots[2].0));
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn channel_history_cursor_ignores_contextual_old_roots() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let channel_id = insert_test_channel(&pool, "history-cursor").await?;
            let mut roots = Vec::new();
            for index in 1..=100 {
                let id: uuid::Uuid = sqlx::query_scalar(
                    r#"
                    insert into messages (
                        channel_id, sender_name, sender_role, body, is_task, created_at
                    )
                    values ($1, 'Dylan', 'owner', $2, false, '2026-01-01T00:00:00.000+00:00')
                    returning id
                    "#,
                )
                .bind(channel_id)
                .bind(format!("root {index}"))
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
                let seq: i64 = sqlx::query_scalar("select seq from messages where id = $1")
                    .bind(id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
                roots.push((id, seq));
            }
            sqlx::query(
                r#"
                insert into messages (
                    channel_id, thread_root_id, sender_name, sender_role, body, is_task,
                    created_at
                )
                values (
                    $1, $2, 'agent', 'agent', 'recent reply on root 1', false,
                    '2026-01-01T00:00:00.000+00:00'
                )
                "#,
            )
            .bind(channel_id)
            .bind(roots[0].0)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let recent =
                load_recent_channel_messages_without_artifact_content(&pool, channel_id, 80)
                    .await?;
            assert!(recent.iter().any(|message| message.id == roots[0].0));
            let history = channel_message_history_from_messages(&recent, 80);
            let channel_history = history
                .iter()
                .find(|entry| entry.channel_id == channel_id)
                .expect("history cursor should be returned");
            assert_eq!(channel_history.before_seq, Some(roots[20].1));
            assert!(channel_history.has_more);
            assert_ne!(channel_history.before_seq, Some(roots[0].1));

            let page = load_older_channel_messages_without_artifact_content(
                &pool,
                channel_id,
                channel_history.before_seq.expect("history cursor"),
                40,
            )
            .await?;
            let page_root_ids = page
                .messages
                .iter()
                .filter(|message| message.thread_root_id.is_none())
                .map(|message| message.id)
                .collect::<std::collections::HashSet<_>>();
            assert!(page_root_ids.contains(&roots[19].0));
            assert!(page_root_ids.contains(&roots[1].0));
            assert!(page_root_ids.contains(&roots[0].0));
            assert_eq!(page_root_ids.len(), 20);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn activity_messages_include_global_channel_thread_and_mention_context() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let mention_channel_id = insert_test_channel(&pool, "activity-mention").await?;
            let other_channel_id = insert_test_channel(&pool, "activity-other").await?;

            let mention_root_id: uuid::Uuid = sqlx::query_scalar(
                r#"
                insert into messages (
                    channel_id, sender_name, sender_role, body, is_task, created_at
                )
                values ($1, 'Dylan', 'owner', 'old mention root', false, '2026-01-01T00:00:00.000+00:00')
                returning id
                "#,
            )
            .bind(mention_channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let mention_id: uuid::Uuid = sqlx::query_scalar(
                r#"
                insert into messages (
                    channel_id, thread_root_id, sender_name, sender_role, body, is_task,
                    created_at
                )
                values (
                    $1, $2, 'agent', 'agent', 'ping @Dylan from an old thread', false,
                    '2026-01-01T00:00:01.000+00:00'
                )
                returning id
                "#,
            )
            .bind(mention_channel_id)
            .bind(mention_root_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            for index in 0..5 {
                sqlx::query(
                    r#"
                    insert into messages (
                        channel_id, sender_name, sender_role, body, is_task, created_at
                    )
                    values ($1, 'Dylan', 'owner', $2, false, $3)
                    "#,
                )
                .bind(mention_channel_id)
                .bind(format!("newer root {index}"))
                .bind(format!("2026-01-01T00:01:0{index}.000+00:00"))
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            }

            let followed_root_id: uuid::Uuid = sqlx::query_scalar(
                r#"
                insert into messages (
                    channel_id, sender_name, sender_role, body, is_task, created_at
                )
                values ($1, 'Dylan', 'owner', 'followed root', false, '2026-01-01T00:02:00.000+00:00')
                returning id
                "#,
            )
            .bind(other_channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let followed_reply_id: uuid::Uuid = sqlx::query_scalar(
                r#"
                insert into messages (
                    channel_id, thread_root_id, sender_name, sender_role, body, is_task,
                    created_at
                )
                values (
                    $1, $2, 'agent', 'agent', 'latest followed reply', false,
                    '2026-01-01T00:02:01.000+00:00'
                )
                returning id
                "#,
            )
            .bind(other_channel_id)
            .bind(followed_root_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let messages = load_activity_messages_without_artifact_content(
                &pool,
                &["@Dylan".to_owned()],
            )
            .await?;
            let ids = messages
                .iter()
                .map(|message| message.id)
                .collect::<std::collections::HashSet<_>>();
            assert!(ids.contains(&mention_root_id));
            assert!(ids.contains(&mention_id));
            assert!(ids.contains(&followed_root_id));
            assert!(ids.contains(&followed_reply_id));
            assert!(messages
                .iter()
                .any(|message| message.body == "newer root 4"));
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn message_search_covers_all_channels_and_respects_literals_and_time() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let channel_id = insert_test_channel(&pool, "search-everywhere").await?;
            let target_id: uuid::Uuid = sqlx::query_scalar(
                r#"
                insert into messages (
                    channel_id, sender_name, sender_role, body, is_task, created_at
                )
                values (
                    $1, 'agent', 'agent', 'literal 100%_match across every channel', false,
                    '2026-01-02T00:00:00.000+00:00'
                )
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let matches =
                search_messages_without_artifact_content(&pool, "%_match", None, 40).await?;
            assert_eq!(matches.len(), 1);
            assert_eq!(matches[0].id, target_id);

            let too_new = search_messages_without_artifact_content(
                &pool,
                "every channel",
                Some("2026-01-03T00:00:00.000+00:00"),
                40,
            )
            .await?;
            assert!(too_new.is_empty());

            let channel_match =
                search_messages_without_artifact_content(&pool, "search-everywhere", None, 40)
                    .await?;
            assert_eq!(channel_match.len(), 1);
            assert_eq!(channel_match[0].id, target_id);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }
}
