use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{sqlite::SqliteRow, Row, Sqlite, SqlitePool, Transaction};
use uuid::Uuid;

use crate::ui_notifications::{
    enqueue_ui_event_in_tx, enqueue_ui_work_item_changed_in_tx, notify_supervisor_wake,
    reconcile_work_item_change, UiEvent,
};
use crate::{
    app::{to_string, CommandResult},
    attachments::attachment_summary_sql,
    channel_wiki::channel_wiki_wake_block,
    context_tool::short_id,
    events::activity::record_agent_activity,
    text::compact_chars_middle,
};

const DISPATCH_MESSAGE_BODY_LIMIT: usize = 4 * 1024;
const INBOX_WAKE_BATCH_LIMIT: i64 = 8;
const INBOX_WAKE_THREAD_CONTEXT_LIMIT: i64 = 10;
const INBOX_WAKE_THREAD_CONTEXT_BODY_LIMIT: usize = 1200;

pub(crate) struct AgentInboxItemInput<'a> {
    pub(crate) agent_id: Uuid,
    pub(crate) channel_id: Option<Uuid>,
    pub(crate) thread_root_id: Option<Uuid>,
    pub(crate) source_message_id: Option<Uuid>,
    pub(crate) task_id: Option<Uuid>,
    pub(crate) kind: &'a str,
    pub(crate) priority: i32,
    pub(crate) title: &'a str,
    pub(crate) body_preview: &'a str,
    pub(crate) payload: Value,
}

#[derive(Clone)]
pub(crate) struct InboxWakeItem {
    pub(crate) id: Uuid,
    pub(crate) channel_id: Option<Uuid>,
    pub(crate) channel_name: Option<String>,
    pub(crate) channel_kind: Option<String>,
    pub(crate) thread_root_id: Option<Uuid>,
    pub(crate) source_message_id: Option<Uuid>,
    pub(crate) task_id: Option<Uuid>,
    pub(crate) kind: String,
    pub(crate) priority: i32,
    pub(crate) title: String,
    pub(crate) body_preview: String,
    pub(crate) attachment_summary: String,
    pub(crate) message_seq: Option<i64>,
    pub(crate) message_created_at: Option<DateTime<Utc>>,
    pub(crate) sender_name: Option<String>,
    pub(crate) sender_role: Option<String>,
}

impl InboxWakeItem {
    fn target(&self) -> String {
        format_inbox_target(
            self.channel_kind.as_deref(),
            self.channel_name.as_deref(),
            self.thread_root_id,
        )
    }

    fn message_header(&self) -> String {
        let msg = self
            .source_message_id
            .map(short_id)
            .unwrap_or_else(|| short_id(self.id));
        let time = self
            .message_created_at
            .map(|time| time.to_rfc3339())
            .unwrap_or_else(|| "-".to_owned());
        let sender_role = self.sender_role.as_deref().unwrap_or("unknown");
        let sender = self
            .sender_name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("unknown");
        let preview = compact_chars_middle(&self.body_preview, DISPATCH_MESSAGE_BODY_LIMIT)
            .replace('\n', " ");
        let mut output = format!(
            "[target={} msg={} time={} type={}] {}: {}",
            self.target(),
            msg,
            time,
            sender_role,
            sender,
            preview
        );
        append_attachment_summary(&mut output, &self.attachment_summary);
        output
    }
}

struct RecentSameThreadContext {
    body: String,
    max_seq: i64,
}

pub(crate) async fn create_agent_inbox_item(
    pool: &SqlitePool,
    input: AgentInboxItemInput<'_>,
) -> CommandResult<Uuid> {
    if let Some(source_message_id) = input.source_message_id {
        let existing_id: Option<Uuid> = sqlx::query_scalar(
            r#"
            select id
            from agent_inbox_items
            where agent_id = $1 and source_message_id = $2 and kind = $3
            limit 1
            "#,
        )
        .bind(input.agent_id)
        .bind(source_message_id)
        .bind(input.kind)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?;
        if let Some(existing_id) = existing_id {
            sqlx::query(
                r#"
                update agent_inbox_items
                set channel_id = $2,
                    thread_root_id = $3,
                    task_id = $4,
                    priority = max(priority, $5),
                    state = case when state = 'archived' then 'unread' else state end,
                    title = $6,
                    body_preview = $7,
                    payload = $8,
                    updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
                    archived_at = case when state = 'archived' then null else archived_at end
                where id = $1
                "#,
            )
            .bind(existing_id)
            .bind(input.channel_id)
            .bind(input.thread_root_id)
            .bind(input.task_id)
            .bind(input.priority)
            .bind(input.title)
            .bind(compact_chars_middle(
                input.body_preview,
                DISPATCH_MESSAGE_BODY_LIMIT,
            ))
            .bind(input.payload)
            .execute(pool)
            .await
            .map_err(to_string)?;
            return Ok(existing_id);
        }
    }

    let inbox_item_id: Uuid = sqlx::query_scalar(
        r#"
        insert into agent_inbox_items (
            agent_id, channel_id, thread_root_id, source_message_id, task_id,
            kind, priority, state, title, body_preview, payload
        )
        values ($1, $2, $3, $4, $5, $6, $7, 'unread', $8, $9, $10)
        returning id
        "#,
    )
    .bind(input.agent_id)
    .bind(input.channel_id)
    .bind(input.thread_root_id)
    .bind(input.source_message_id)
    .bind(input.task_id)
    .bind(input.kind)
    .bind(input.priority)
    .bind(input.title)
    .bind(compact_chars_middle(
        input.body_preview,
        DISPATCH_MESSAGE_BODY_LIMIT,
    ))
    .bind(input.payload)
    .fetch_one(pool)
    .await
    .map_err(to_string)?;
    Ok(inbox_item_id)
}

pub(crate) async fn attach_work_item_to_inbox_in_tx(
    transaction: &mut Transaction<'_, Sqlite>,
    inbox_item_id: Uuid,
    work_item_id: Uuid,
) -> CommandResult<()> {
    attach_work_item_to_inboxes_in_tx(transaction, &[inbox_item_id], work_item_id).await
}

async fn attach_work_item_to_inboxes(
    pool: &SqlitePool,
    inbox_item_ids: &[Uuid],
    work_item_id: Uuid,
) -> CommandResult<()> {
    if inbox_item_ids.is_empty() {
        return Ok(());
    }
    for inbox_item_id in inbox_item_ids {
        sqlx::query(
            r#"
            update agent_inbox_items
            set work_item_id = $2,
                state = 'processing',
                updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
                archived_at = null
            where id = $1
            "#,
        )
        .bind(*inbox_item_id)
        .bind(work_item_id)
        .execute(pool)
        .await
        .map_err(to_string)?;
    }
    Ok(())
}

async fn attach_work_item_to_inboxes_in_tx(
    transaction: &mut Transaction<'_, Sqlite>,
    inbox_item_ids: &[Uuid],
    work_item_id: Uuid,
) -> CommandResult<()> {
    for inbox_item_id in inbox_item_ids {
        sqlx::query(
            r#"
            update agent_inbox_items
            set work_item_id = $2,
                state = 'processing',
                updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
                archived_at = null
            where id = $1
            "#,
        )
        .bind(*inbox_item_id)
        .bind(work_item_id)
        .execute(&mut **transaction)
        .await
        .map_err(to_string)?;
    }
    Ok(())
}

fn format_inbox_target(
    channel_kind: Option<&str>,
    channel_name: Option<&str>,
    thread_root_id: Option<Uuid>,
) -> String {
    match (channel_kind, channel_name, thread_root_id) {
        (Some("dm"), Some(name), Some(thread_root_id)) => {
            format!("dm:{name}:{}", short_id(thread_root_id))
        }
        (Some("dm"), Some(name), None) => format!("dm:{name}"),
        (_, Some(name), Some(thread_root_id)) => format!("#{name}:{}", short_id(thread_root_id)),
        (_, Some(name), None) => format!("#{name}"),
        _ => "unknown target".to_owned(),
    }
}

fn inbox_wake_item_from_row(row: &SqliteRow) -> InboxWakeItem {
    InboxWakeItem {
        id: row.get("id"),
        channel_id: row.get("channel_id"),
        channel_name: row.get("channel_name"),
        channel_kind: row.get("channel_kind"),
        thread_root_id: row.get("thread_root_id"),
        source_message_id: row.get("source_message_id"),
        task_id: row.get("task_id"),
        kind: row.get("kind"),
        priority: row.get("priority"),
        title: row.get("title"),
        body_preview: row.get("body_preview"),
        attachment_summary: row.get("attachment_summary"),
        message_seq: row.get("message_seq"),
        message_created_at: row.get("message_created_at"),
        sender_name: row.get("sender_name"),
        sender_role: row.get("sender_role"),
    }
}

fn append_attachment_summary(output: &mut String, attachment_summary: &str) {
    if attachment_summary.trim().is_empty() {
        return;
    }
    output.push_str("\n  attachments:");
    for line in attachment_summary.lines() {
        output.push_str("\n  - ");
        output.push_str(line);
    }
    output.push_str("\n  To inspect an attachment, run attachment-info with its attachment_id.");
}

async fn next_unread_inbox_wake_item(
    pool: &SqlitePool,
    agent_id: Uuid,
) -> CommandResult<Option<InboxWakeItem>> {
    let row = sqlx::query(&format!(
        r#"
        select
            i.id,
            i.channel_id,
            c.name as channel_name,
            c.kind as channel_kind,
            i.thread_root_id,
            i.source_message_id,
            i.task_id,
            i.kind,
            i.priority,
            i.title,
            i.body_preview,
            m.seq as message_seq,
            m.created_at as message_created_at,
            m.sender_name,
            m.sender_role,
            {}
        from agent_inbox_items i
        left join channels c on c.id = i.channel_id
        left join messages m on m.id = i.source_message_id
        where i.agent_id = $1
          and i.state = 'unread'
        order by i.priority desc, i.created_at asc
        limit 1
        "#,
        attachment_summary_sql()
    ))
    .bind(agent_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;

    Ok(row.as_ref().map(inbox_wake_item_from_row))
}

async fn load_unread_inbox_wake_batch(
    pool: &SqlitePool,
    agent_id: Uuid,
    channel_id: Option<Uuid>,
    thread_root_id: Option<Uuid>,
) -> CommandResult<Vec<InboxWakeItem>> {
    let rows = sqlx::query(&format!(
        r#"
        select
            i.id,
            i.channel_id,
            c.name as channel_name,
            c.kind as channel_kind,
            i.thread_root_id,
            i.source_message_id,
            i.task_id,
            i.kind,
            i.priority,
            i.title,
            i.body_preview,
            m.seq as message_seq,
            m.created_at as message_created_at,
            m.sender_name,
            m.sender_role,
            {}
        from agent_inbox_items i
        left join channels c on c.id = i.channel_id
        left join messages m on m.id = i.source_message_id
        where i.agent_id = $1
          and i.state = 'unread'
          and i.channel_id is not distinct from $2
          and i.thread_root_id is not distinct from $3
        order by i.priority desc, i.created_at asc
        limit $4
        "#,
        attachment_summary_sql()
    ))
    .bind(agent_id)
    .bind(channel_id)
    .bind(thread_root_id)
    .bind(INBOX_WAKE_BATCH_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    Ok(rows.iter().map(inbox_wake_item_from_row).collect())
}

pub(crate) async fn load_inbox_wake_items_for_work_item(
    pool: &SqlitePool,
    work_item_id: Uuid,
) -> CommandResult<Vec<InboxWakeItem>> {
    let rows = sqlx::query(&format!(
        r#"
        select
            i.id,
            i.channel_id,
            c.name as channel_name,
            c.kind as channel_kind,
            i.thread_root_id,
            i.source_message_id,
            i.task_id,
            i.kind,
            i.priority,
            i.title,
            i.body_preview,
            m.seq as message_seq,
            m.created_at as message_created_at,
            m.sender_name,
            m.sender_role,
            {}
        from agent_inbox_items i
        left join channels c on c.id = i.channel_id
        left join messages m on m.id = i.source_message_id
        where i.work_item_id = $1
        order by i.priority desc, i.created_at asc
        "#,
        attachment_summary_sql()
    ))
    .bind(work_item_id)
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    Ok(rows.iter().map(inbox_wake_item_from_row).collect())
}

async fn find_queued_inbox_wake_work_item_for_surface(
    pool: &SqlitePool,
    agent_id: Uuid,
    channel_id: Option<Uuid>,
    thread_root_id: Option<Uuid>,
) -> CommandResult<Option<Uuid>> {
    sqlx::query_scalar(
        r#"
        select id
        from agent_work_items
        where agent_id = $1
          and source_kind = 'inbox_wake'
          and status = 'queued'
          and channel_id is not distinct from $2
          and thread_root_id is not distinct from $3
        order by created_at asc
        limit 1
        "#,
    )
    .bind(agent_id)
    .bind(channel_id)
    .bind(thread_root_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)
}

fn inbox_wake_work_item_title(items: &[InboxWakeItem]) -> String {
    let Some(primary) = items.first() else {
        return "Process inbox".to_owned();
    };
    if items.len() == 1 {
        format!("Process inbox: {}", primary.title)
    } else {
        format!(
            "Process inbox: {} (+{} more)",
            primary.title,
            items.len() - 1
        )
    }
}

#[cfg(test)]
pub(crate) fn inbox_wake_context(items: &[InboxWakeItem]) -> String {
    inbox_wake_context_with_thread_context(items, None, None)
}

/// Load the channel-wiki announcement block for the batch's primary channel,
/// so every inbox wake tells the agent which wiki revision is current.
async fn load_channel_wiki_context(
    pool: &SqlitePool,
    items: &[InboxWakeItem],
) -> CommandResult<Option<String>> {
    let Some(primary) = items.first() else {
        return Ok(None);
    };
    let Some(channel_id) = primary.channel_id else {
        return Ok(None);
    };
    let label = match (
        primary.channel_name.as_deref(),
        primary.channel_kind.as_deref(),
    ) {
        (Some(name), Some("dm")) => format!("dm:{name}"),
        (Some(name), _) => format!("#{name}"),
        _ => "this channel".to_owned(),
    };
    channel_wiki_wake_block(pool, channel_id, &label).await
}

pub(crate) fn inbox_wake_context_with_thread_context(
    items: &[InboxWakeItem],
    same_thread_context: Option<&str>,
    channel_wiki_block: Option<&str>,
) -> String {
    let Some(primary) = items.first() else {
        return "Lantor agent inbox wake.".to_owned();
    };
    let target = primary.target();
    let has_task_available = items.iter().any(|item| item.kind == "task_available");
    let mut lines = vec![
        "Lantor agent inbox wake.".to_owned(),
        if items.len() == 1 {
            "The default inbox item below is already selected for this turn. Handle it directly from this context when enough detail is present.".to_owned()
        } else {
            format!(
                "This turn batches {} inbox items from the same channel/thread target. Handle them together when possible.",
                items.len()
            )
        },
        "Use the selected message header directly; call inbox-read only when its preview or attachment metadata is insufficient.".to_owned(),
        String::new(),
        format!("Default reply target for normal assistant text: {target}"),
        String::new(),
    ];
    if let Some(channel_wiki_block) = channel_wiki_block.filter(|block| !block.trim().is_empty()) {
        lines.push(channel_wiki_block.trim().to_owned());
        lines.push(String::new());
    }
    if let Some(same_thread_context) =
        same_thread_context.filter(|context| !context.trim().is_empty())
    {
        lines.push("Recent same-thread context (oldest to newest):".to_owned());
        lines.push(same_thread_context.trim().to_owned());
        lines.push(String::new());
    }
    if has_task_available {
        lines.extend([
            "Task claim opportunity mode:".to_owned(),
            "- This is a competitive, unassigned task opportunity sent to multiple agents.".to_owned(),
            "- If you can start now, emit only the standalone `LANTOR_EVENT {\"type\":\"task_claim\",\"task_number\":...}` control line, then finish with `LANTOR_SILENT_REPLY: claim attempted`.".to_owned(),
            "- Do not post a visible reply, do not narrate that you are queued/starting, and do not emit activity events for the claim attempt.".to_owned(),
            "- Lantor atomically accepts one claimant, ignores stale claims, and will send a separate task_assigned inbox turn to the winning agent to do the work visibly.".to_owned(),
            String::new(),
        ]);
    }

    if items.len() == 1 {
        lines.push("Default inbox item:".to_owned());
    } else {
        lines.push("Batched inbox items:".to_owned());
    }
    for (index, item) in items.iter().enumerate() {
        if items.len() > 1 {
            lines.push(format!("{}. {}", index + 1, item.message_header()));
        } else {
            lines.push(item.message_header());
        }
        lines.push(format!("   inbox_id: {}", item.id));
        lines.push(format!(
            "   kind: {}, priority: {}, title: {}",
            item.kind, item.priority, item.title
        ));
        if index + 1 < items.len() {
            lines.push(String::new());
        }
    }

    lines.join("\n")
}

fn inbox_item_is_existing_thread_message(item: &InboxWakeItem) -> bool {
    matches!(
        (item.thread_root_id, item.source_message_id),
        (Some(thread_root_id), Some(source_message_id)) if thread_root_id != source_message_id
    )
}

fn format_recent_thread_context_row(row: &SqliteRow, target: &str) -> String {
    let message_id: Uuid = row.get("id");
    let created_at: DateTime<Utc> = row.get("created_at");
    let sender_role: String = row.get("sender_role");
    let sender_name: String = row.get("sender_name");
    let delivery_state: String = row.get("delivery_state");
    let state_suffix = if delivery_state == "complete" {
        String::new()
    } else {
        format!(" delivery={delivery_state}")
    };
    let body: String = row.get("body");
    let body = compact_chars_middle(&body, INBOX_WAKE_THREAD_CONTEXT_BODY_LIMIT).replace('\n', " ");
    let mut output = format!(
        "[target={target} msg={} time={} type={}{}] {}: {}",
        short_id(message_id),
        created_at.to_rfc3339(),
        sender_role,
        state_suffix,
        sender_name,
        body
    );
    let attachment_summary: String = row.get("attachment_summary");
    append_attachment_summary(&mut output, &attachment_summary);
    output
}

async fn load_recent_same_thread_context(
    pool: &SqlitePool,
    items: &[InboxWakeItem],
) -> CommandResult<Option<RecentSameThreadContext>> {
    let Some(context_item) = items
        .iter()
        .find(|item| inbox_item_is_existing_thread_message(item))
    else {
        return Ok(None);
    };
    let (Some(channel_id), Some(thread_root_id)) =
        (context_item.channel_id, context_item.thread_root_id)
    else {
        return Ok(None);
    };
    let rows = sqlx::query(&format!(
        r#"
        select
            m.id,
            m.sender_name,
            m.sender_role,
            m.body,
            m.delivery_state,
            m.seq,
            m.created_at,
            {}
        from messages m
        where m.channel_id = $1
          and (m.id = $2 or m.thread_root_id = $2)
        order by m.seq desc
        limit $3
        "#,
        attachment_summary_sql()
    ))
    .bind(channel_id)
    .bind(thread_root_id)
    .bind(INBOX_WAKE_THREAD_CONTEXT_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    if rows.is_empty() {
        return Ok(None);
    }

    let target = context_item.target();
    let max_seq = rows
        .iter()
        .map(|row| row.get::<i64, _>("seq"))
        .max()
        .unwrap_or(0);
    let lines = rows
        .into_iter()
        .rev()
        .map(|row| format_recent_thread_context_row(&row, &target))
        .collect::<Vec<_>>();
    Ok(Some(RecentSameThreadContext {
        body: lines.join("\n"),
        max_seq,
    }))
}

fn inbox_wake_context_max_seq(
    items: &[InboxWakeItem],
    same_thread_context: Option<&RecentSameThreadContext>,
) -> i64 {
    let item_max = items
        .iter()
        .filter_map(|item| item.message_seq)
        .max()
        .unwrap_or(0);
    item_max.max(
        same_thread_context
            .map(|context| context.max_seq)
            .unwrap_or(0),
    )
}

pub(crate) fn build_steer_followup_prompt(items: &[InboxWakeItem]) -> String {
    let Some(primary) = items.first() else {
        return "Same-channel/thread live inbox follow-up.".to_owned();
    };
    let target = primary.target();
    let mut lines = vec![
        "Same-channel/thread live inbox follow-up.".to_owned(),
        "Treat the message header(s) below as the newest input for the active turn.".to_owned(),
        format!("Default reply target for normal assistant text: {target}"),
        String::new(),
    ];

    if items.len() == 1 {
        lines.push("New inbox message:".to_owned());
    } else {
        lines.push("New inbox messages:".to_owned());
    }
    for (index, item) in items.iter().enumerate() {
        if items.len() > 1 {
            lines.push(format!("{}. {}", index + 1, item.message_header()));
        } else {
            lines.push(item.message_header());
        }
        lines.push(format!("   inbox_id: {}", item.id));
        if index + 1 < items.len() {
            lines.push(String::new());
        }
    }

    lines.join("\n")
}

async fn refresh_inbox_wake_work_item(
    pool: &SqlitePool,
    work_item_id: Uuid,
    items: &[InboxWakeItem],
) -> CommandResult<()> {
    let Some(primary) = items.first() else {
        return Ok(());
    };
    let same_thread_context = load_recent_same_thread_context(pool, items).await?;
    let channel_wiki_block = load_channel_wiki_context(pool, items).await?;
    let context_max_seq = inbox_wake_context_max_seq(items, same_thread_context.as_ref());
    let mut transaction = pool.begin().await.map_err(to_string)?;
    sqlx::query(
        r#"
        update agent_work_items
        set channel_id = $2,
            thread_root_id = $3,
            source_message_id = $4,
            inbox_item_id = $5,
            task_id = $6,
            title = $7,
            context = $8,
            context_max_seq = $9,
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1
        "#,
    )
    .bind(work_item_id)
    .bind(primary.channel_id)
    .bind(primary.thread_root_id)
    .bind(primary.source_message_id)
    .bind(primary.id)
    .bind(primary.task_id)
    .bind(inbox_wake_work_item_title(items))
    .bind(inbox_wake_context_with_thread_context(
        items,
        same_thread_context
            .as_ref()
            .map(|context| context.body.as_str()),
        channel_wiki_block.as_deref(),
    ))
    .bind(context_max_seq)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;
    enqueue_ui_work_item_changed_in_tx(&mut transaction, work_item_id, "work_item_merged").await?;
    transaction.commit().await.map_err(to_string)?;
    reconcile_work_item_change(pool, work_item_id, "work_item_merged").await?;
    Ok(())
}

pub(crate) fn prepend_inbox_context(inbox_item_id: Uuid, kind: &str, context: &str) -> String {
    let mut lines = vec![
        "Agent inbox item:".to_owned(),
        format!("id: {inbox_item_id}"),
        format!("kind: {kind}"),
        "decision: decide whether this inbox item needs a visible reply, a task claim/create, a reminder, a handoff, or a silent_reply.".to_owned(),
    ];
    if !context.trim().is_empty() {
        lines.push(String::new());
        lines.push(context.trim().to_owned());
    }
    lines.join("\n")
}

pub(crate) async fn sync_inbox_for_work_item(
    pool: &SqlitePool,
    work_item_id: Uuid,
) -> CommandResult<()> {
    let row = sqlx::query(
        r#"
        select agent_id, source_kind, status
        from agent_work_items
        where id = $1
        "#,
    )
    .bind(work_item_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;
    let Some(row) = row else {
        return Ok(());
    };
    let agent_id: Uuid = row.get("agent_id");
    let source_kind: String = row.get("source_kind");
    let status: String = row.get("status");
    let inbox_state = match status.as_str() {
        "queued" | "running" | "cancelling" => "processing",
        "done" | "failed" | "cancelled" | "silent" | "held" => "archived",
        _ => "processing",
    };
    sqlx::query(
        r#"
        update agent_inbox_items
        set state = $2,
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
            archived_at = case when $2 = 'archived' then strftime('%Y-%m-%dT%H:%M:%f+00:00','now') else null end
        where work_item_id = $1
        "#,
    )
    .bind(work_item_id)
    .bind(inbox_state)
    .execute(pool)
    .await
    .map_err(to_string)?;
    if source_kind == "inbox_wake"
        && matches!(status.as_str(), "done" | "failed" | "cancelled" | "held")
    {
        let _ = Box::pin(ensure_agent_inbox_wake_work_item(pool, agent_id)).await?;
    }
    Ok(())
}

pub(crate) async fn ensure_agent_inbox_wake_work_item(
    pool: &SqlitePool,
    agent_id: Uuid,
) -> CommandResult<Option<(Uuid, bool)>> {
    let Some(primary) = next_unread_inbox_wake_item(pool, agent_id).await? else {
        return Ok(None);
    };
    let mut batch =
        load_unread_inbox_wake_batch(pool, agent_id, primary.channel_id, primary.thread_root_id)
            .await?;
    if batch.is_empty() {
        batch.push(primary);
    }
    let inbox_item_ids: Vec<Uuid> = batch.iter().map(|item| item.id).collect();

    if let Some(existing_work_item_id) = find_queued_inbox_wake_work_item_for_surface(
        pool,
        agent_id,
        batch[0].channel_id,
        batch[0].thread_root_id,
    )
    .await?
    {
        attach_work_item_to_inboxes(pool, &inbox_item_ids, existing_work_item_id).await?;
        let items = load_inbox_wake_items_for_work_item(pool, existing_work_item_id).await?;
        refresh_inbox_wake_work_item(pool, existing_work_item_id, &items).await?;
        let scheduled =
            enqueue_agent_work_if_available(pool, agent_id, existing_work_item_id).await?;
        let detail = format!(
            "{}: {}",
            items
                .first()
                .map(InboxWakeItem::target)
                .unwrap_or_else(|| "unknown target".to_owned()),
            inbox_wake_work_item_title(&items)
        );
        record_agent_activity(
            pool,
            Some(agent_id),
            None,
            "inbox",
            if scheduled {
                "Inbox wake merged and dispatched"
            } else {
                "Inbox wake merged"
            },
            detail,
        )
        .await?;
        return Ok(Some((existing_work_item_id, scheduled)));
    }

    let same_thread_context = load_recent_same_thread_context(pool, &batch).await?;
    let channel_wiki_block = load_channel_wiki_context(pool, &batch).await?;
    let context_max_seq = inbox_wake_context_max_seq(&batch, same_thread_context.as_ref());
    let mut transaction = pool.begin().await.map_err(to_string)?;
    let work_item_id: Uuid = sqlx::query_scalar(
        r#"
        insert into agent_work_items (
            agent_id, channel_id, thread_root_id, source_message_id, inbox_item_id, task_id,
            source_kind, title, context, context_max_seq, status
        )
        values ($1, $2, $3, $4, $5, $6, 'inbox_wake', $7, $8, $9, 'queued')
        returning id
        "#,
    )
    .bind(agent_id)
    .bind(batch[0].channel_id)
    .bind(batch[0].thread_root_id)
    .bind(batch[0].source_message_id)
    .bind(batch[0].id)
    .bind(batch[0].task_id)
    .bind(inbox_wake_work_item_title(&batch))
    .bind(inbox_wake_context_with_thread_context(
        &batch,
        same_thread_context
            .as_ref()
            .map(|context| context.body.as_str()),
        channel_wiki_block.as_deref(),
    ))
    .bind(context_max_seq)
    .fetch_one(&mut *transaction)
    .await
    .map_err(to_string)?;
    attach_work_item_to_inboxes_in_tx(&mut transaction, &inbox_item_ids, work_item_id).await?;
    enqueue_ui_work_item_changed_in_tx(&mut transaction, work_item_id, "work_item_created").await?;
    transaction.commit().await.map_err(to_string)?;
    reconcile_work_item_change(pool, work_item_id, "work_item_created").await?;
    let scheduled = enqueue_agent_work_if_available(pool, agent_id, work_item_id).await?;
    let target = batch
        .first()
        .map(InboxWakeItem::target)
        .unwrap_or_else(|| "unknown target".to_owned());
    record_agent_activity(
        pool,
        Some(agent_id),
        None,
        "inbox",
        if scheduled {
            "Inbox wake dispatched"
        } else {
            "Inbox wake queued"
        },
        format!("{target}: {}", inbox_wake_work_item_title(&batch)),
    )
    .await?;
    Ok(Some((work_item_id, scheduled)))
}

pub(crate) async fn agent_runtime(
    pool: &SqlitePool,
    agent_id: Uuid,
) -> CommandResult<Option<String>> {
    sqlx::query_scalar("select runtime from agents where id = $1")
        .bind(agent_id)
        .fetch_optional(pool)
        .await
        .map_err(to_string)
}

/// Seconds an agent whose last launch failed waits before Lantor launches it
/// again on its own. New work is still accepted and queued during the cooldown;
/// only the launch is deferred (the scheduler sweep retries it), so a broken
/// runtime is not relaunched by every incoming message while a transient
/// failure still recovers on the next pass. A manual start bypasses it.
const FAILED_START_COOLDOWN_SECS: i64 = 60;

/// True while an agent flagged `error` had a launch failure within the cooldown.
pub(crate) async fn agent_start_cooldown_active(
    pool: &SqlitePool,
    agent_id: Uuid,
) -> CommandResult<bool> {
    let Some(row) = sqlx::query("select handle, status from agents where id = $1")
        .bind(agent_id)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?
    else {
        return Ok(false);
    };
    let status: String = row.get("status");
    if !status.eq_ignore_ascii_case("error") {
        return Ok(false);
    }
    let handle: String = row.get("handle");
    let recent_failure: Option<i64> = sqlx::query_scalar(
        r#"
        select 1
        from agent_activities
        where agent_id = $1
          and agent_handle = $2
          and created_at > strftime('%Y-%m-%dT%H:%M:%f+00:00','now', $3)
          and kind = 'run_error'
        limit 1
        "#,
    )
    .bind(agent_id)
    .bind(handle)
    .bind(format!("-{FAILED_START_COOLDOWN_SECS} seconds"))
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;
    Ok(recent_failure.is_some())
}

async fn agent_has_active_run(pool: &SqlitePool, agent_id: Uuid) -> CommandResult<bool> {
    let active_run: Option<Uuid> = sqlx::query_scalar(
        r#"
        select id
        from agent_runs
        where agent_id = $1
          and stopped_at is null
          and status in ('starting', 'running', 'stopping')
        limit 1
        "#,
    )
    .bind(agent_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;

    Ok(active_run.is_some())
}

pub(crate) async fn agent_has_active_or_pending_start(
    pool: &SqlitePool,
    agent_id: Uuid,
) -> CommandResult<bool> {
    if agent_has_active_run(pool, agent_id).await? {
        return Ok(true);
    }

    let pending_start: Option<Uuid> = sqlx::query_scalar(
        r#"
        select id
        from supervisor_commands
        where command_type = 'start_agent'
          and agent_id = $1
          and status in ('pending', 'running')
        limit 1
        "#,
    )
    .bind(agent_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;

    Ok(pending_start.is_some())
}

pub(crate) async fn enqueue_agent_work_if_available(
    pool: &SqlitePool,
    agent_id: Uuid,
    work_item_id: Uuid,
) -> CommandResult<bool> {
    let status: Option<String> =
        sqlx::query_scalar("select status from agent_work_items where id = $1 and agent_id = $2")
            .bind(work_item_id)
            .bind(agent_id)
            .fetch_optional(pool)
            .await
            .map_err(to_string)?;
    if status.as_deref() != Some("queued") {
        return Ok(false);
    }
    if agent_start_cooldown_active(pool, agent_id).await? {
        return Ok(false);
    }
    let runtime = agent_runtime(pool, agent_id).await?;
    let is_codex = runtime
        .as_deref()
        .is_some_and(|runtime| runtime.eq_ignore_ascii_case("codex"));
    let has_active_run = agent_has_active_run(pool, agent_id).await?;
    if !is_codex && agent_has_active_or_pending_start(pool, agent_id).await? {
        return Ok(false);
    }

    let pending_for_work: Option<Uuid> = sqlx::query_scalar(
        r#"
        select id
        from supervisor_commands
        where command_type = 'start_agent'
          and work_item_id = $1
          and status in ('pending', 'running')
        limit 1
        "#,
    )
    .bind(work_item_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;
    if pending_for_work.is_some() {
        return Ok(true);
    }

    let mut transaction = pool.begin().await.map_err(to_string)?;
    sqlx::query(
        r#"
        insert into supervisor_commands (command_type, agent_id, work_item_id)
        values ('start_agent', $1, $2)
        "#,
    )
    .bind(agent_id)
    .bind(work_item_id)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;
    sqlx::query("update agents set status = 'queued' where id = $1")
        .bind(agent_id)
        .execute(&mut *transaction)
        .await
        .map_err(to_string)?;
    if is_codex && has_active_run {
        sqlx::query("update agents set status = 'running' where id = $1")
            .bind(agent_id)
            .execute(&mut *transaction)
            .await
            .map_err(to_string)?;
    }
    enqueue_ui_event_in_tx(
        &mut transaction,
        &UiEvent::Refresh {
            reason: "supervisor_command",
        },
    )
    .await?;
    transaction.commit().await.map_err(to_string)?;
    let _ = notify_supervisor_wake(pool).await;

    Ok(true)
}

#[cfg(test)]
#[path = "tests/agent_inbox_wake.rs"]
mod relocated_tests;
