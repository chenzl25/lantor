use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::agent_inbox_wake::{
    agent_accepts_new_work, create_agent_inbox_item, ensure_agent_inbox_wake_work_item,
    AgentInboxItemInput,
};
use crate::events::activity::record_agent_activity;
use crate::message_store::insert_agent_handoff_message;
use crate::ui_notifications::insert_system_message;
use crate::{to_string, CommandResult};

pub(crate) fn extract_agent_mentions(body: &str) -> Vec<String> {
    let mut handles = Vec::new();
    let mut chars = body.char_indices().peekable();
    while let Some((idx, ch)) = chars.next() {
        if ch != '@' {
            continue;
        }
        if body[..idx]
            .chars()
            .next_back()
            .map(|prev| prev.is_ascii_alphanumeric() || prev == '_' || prev == '-')
            .unwrap_or(false)
        {
            continue;
        }
        let mut handle = String::new();
        while let Some((_, next)) = chars.peek().copied() {
            if next.is_ascii_alphanumeric() || next == '_' || next == '-' {
                handle.push(next);
                chars.next();
            } else {
                break;
            }
        }
        if !handle.is_empty() && !handles.contains(&handle) {
            handles.push(handle);
        }
    }
    handles
}

#[derive(Clone, Copy)]
pub(crate) enum MentionDispatchOrigin {
    Owner,
    Agent {
        sender_agent_id: Uuid,
        allow_channel_member_invite: bool,
    },
}

impl MentionDispatchOrigin {
    fn sender_agent_id(self) -> Option<Uuid> {
        match self {
            MentionDispatchOrigin::Owner => None,
            MentionDispatchOrigin::Agent {
                sender_agent_id, ..
            } => Some(sender_agent_id),
        }
    }

    fn allows_dm_auto_dispatch(self) -> bool {
        matches!(self, MentionDispatchOrigin::Owner)
    }

    fn is_agent(self) -> bool {
        matches!(self, MentionDispatchOrigin::Agent { .. })
    }

    fn allows_channel_member_invite(self) -> bool {
        match self {
            MentionDispatchOrigin::Owner => true,
            MentionDispatchOrigin::Agent {
                allow_channel_member_invite,
                ..
            } => allow_channel_member_invite,
        }
    }
}

const INTER_AGENT_THREAD_MESSAGE_LIMIT: i64 = 10;

#[derive(Clone, Copy, PartialEq, Eq)]
enum DispatchKind {
    ChannelMessage,
    Mention,
    Dm,
    ThreadFollowUp,
}

pub(crate) async fn upsert_agent_thread_subscription(
    pool: &SqlitePool,
    agent_id: Uuid,
    channel_id: Uuid,
    thread_root_id: Uuid,
    source_kind: &str,
    source_message_id: Option<Uuid>,
) -> CommandResult<()> {
    sqlx::query(
        r#"
        insert into agent_thread_subscriptions (
            agent_id, channel_id, thread_root_id, source_kind, last_source_message_id
        )
        values ($1, $2, $3, $4, $5)
        on conflict (agent_id, thread_root_id) do update
        set channel_id = excluded.channel_id,
            source_kind = excluded.source_kind,
            last_source_message_id = coalesce(excluded.last_source_message_id, agent_thread_subscriptions.last_source_message_id),
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        "#,
    )
    .bind(agent_id)
    .bind(channel_id)
    .bind(thread_root_id)
    .bind(source_kind)
    .bind(source_message_id)
    .execute(pool)
    .await
    .map_err(to_string)?;
    Ok(())
}

pub(crate) async fn queue_mentions_as_work_items(
    pool: &SqlitePool,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    message_id: Uuid,
    task_id: Option<Uuid>,
    body: &str,
    origin: MentionDispatchOrigin,
) -> CommandResult<()> {
    let mentions = extract_agent_mentions(body);
    let channel_row = sqlx::query("select name, kind, dm_agent_id from channels where id = $1")
        .bind(channel_id)
        .fetch_one(pool)
        .await
        .map_err(to_string)?;
    let channel_name: String = channel_row.get("name");
    let channel_kind: String = channel_row.get("kind");
    let dm_agent_id: Option<Uuid> = channel_row.get("dm_agent_id");

    let mut targets = Vec::new();
    let mut dispatch_kind = DispatchKind::Mention;
    if channel_kind == "dm" {
        dispatch_kind = DispatchKind::Dm;
        if !origin.allows_dm_auto_dispatch() {
            return Ok(());
        }
        if let Some(agent_id) = dm_agent_id {
            let agent_handle: Option<String> =
                sqlx::query_scalar("select handle from agents where id = $1 and status <> 'error'")
                    .bind(agent_id)
                    .fetch_optional(pool)
                    .await
                    .map_err(to_string)?;
            if let Some(agent_handle) = agent_handle {
                targets.push((agent_id, agent_handle));
            }
        }
    } else {
        for handle in &mentions {
            let agent_id: Option<Uuid> =
                sqlx::query_scalar("select id from agents where handle = $1 and status <> 'error'")
                    .bind(handle)
                    .fetch_optional(pool)
                    .await
                    .map_err(to_string)?;
            let Some(agent_id) = agent_id else {
                continue;
            };
            if Some(agent_id) == origin.sender_agent_id() {
                continue;
            }
            if origin.is_agent() && !origin.allows_channel_member_invite() {
                let is_channel_member: bool = sqlx::query_scalar(
                    r#"
                    select exists (
                        select 1
                        from channel_members
                        where channel_id = $1 and agent_id = $2
                    )
                    "#,
                )
                .bind(channel_id)
                .bind(agent_id)
                .fetch_one(pool)
                .await
                .map_err(to_string)?;
                if !is_channel_member {
                    continue;
                }
            }
            targets.push((agent_id, handle.clone()));
        }
        if mentions.is_empty()
            && targets.is_empty()
            && matches!(origin, MentionDispatchOrigin::Owner)
        {
            if let Some(thread_root_id) = thread_root_id {
                targets = load_thread_followup_targets(pool, channel_id, thread_root_id).await?;
                if !targets.is_empty() {
                    dispatch_kind = DispatchKind::ThreadFollowUp;
                }
            } else if task_id.is_none() {
                targets = load_channel_root_delivery_targets(pool, channel_id).await?;
                if !targets.is_empty() {
                    dispatch_kind = DispatchKind::ChannelMessage;
                }
            } else {
                let channel_targets = load_channel_root_delivery_targets(pool, channel_id).await?;
                if channel_targets.len() == 1 {
                    targets = channel_targets;
                    dispatch_kind = DispatchKind::ChannelMessage;
                }
            }
        }
    }

    if targets.is_empty() {
        return Ok(());
    }
    if task_id.is_some() {
        targets.truncate(1);
    }
    let reply_thread_root_id = thread_root_id.unwrap_or(message_id);
    if origin.is_agent()
        && inter_agent_thread_message_count_since_last_owner(pool, channel_id, reply_thread_root_id)
            .await?
            >= INTER_AGENT_THREAD_MESSAGE_LIMIT
    {
        insert_system_message(
            pool,
            channel_id,
            Some(reply_thread_root_id),
            format!(
                "Inter-agent collaboration paused: this thread reached {INTER_AGENT_THREAD_MESSAGE_LIMIT} agent messages. Add a human reply to continue."
            ),
        )
        .await?;
        return Ok(());
    }

    let title = body
        .lines()
        .next()
        .map(|line| line.chars().take(120).collect::<String>())
        .filter(|line| !line.trim().is_empty())
        .unwrap_or_else(|| match dispatch_kind {
            DispatchKind::ChannelMessage => format!("Channel message in #{channel_name}"),
            DispatchKind::Dm => format!("DM in #{channel_name}"),
            DispatchKind::Mention => format!("Mention in #{channel_name}"),
            DispatchKind::ThreadFollowUp => format!("Thread follow-up in #{channel_name}"),
        });

    if let (Some(task_id), Some((agent_id, _))) = (task_id, targets.first()) {
        sqlx::query(
            r#"
            update tasks
            set assignee_agent_id = $2,
                status = 'in_progress',
                version = version + 1,
                updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
            where id = $1 and assignee_agent_id is null
            "#,
        )
        .bind(task_id)
        .bind(*agent_id)
        .execute(pool)
        .await
        .map_err(to_string)?;
    }

    for (agent_id, agent_handle) in targets {
        let already_queued: bool = sqlx::query_scalar(
            r#"
            select exists (
                select 1
                from agent_work_items
                where source_message_id = $1 and agent_id = $2
            )
            "#,
        )
        .bind(message_id)
        .bind(agent_id)
        .fetch_one(pool)
        .await
        .map_err(to_string)?;
        if already_queued {
            continue;
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

        let source_kind = if task_id.is_some() {
            "task"
        } else {
            match (dispatch_kind, origin.is_agent()) {
                (DispatchKind::ChannelMessage, _) => "channel_message",
                (DispatchKind::Dm, _) => "dm",
                (DispatchKind::ThreadFollowUp, _) => "thread_followup",
                (DispatchKind::Mention, true) => "collaboration",
                (DispatchKind::Mention, false) => "mention",
            }
        };
        let inbox_kind = if task_id.is_some() {
            "task_assigned"
        } else {
            source_kind
        };
        let priority = match (dispatch_kind, task_id.is_some(), origin.is_agent()) {
            (_, true, _) => 95,
            (DispatchKind::Dm, _, _) => 85,
            (DispatchKind::Mention, _, false) => 80,
            (DispatchKind::Mention, _, true) => 70,
            (DispatchKind::ThreadFollowUp, _, _) => 60,
            (DispatchKind::ChannelMessage, _, _) => 35,
        };
        let inbox_item_id = create_agent_inbox_item(
            pool,
            AgentInboxItemInput {
                agent_id,
                channel_id: Some(channel_id),
                thread_root_id: Some(reply_thread_root_id),
                source_message_id: Some(message_id),
                task_id,
                kind: inbox_kind,
                priority,
                title: &title,
                body_preview: body,
                payload: json!({
                    "channel_name": &channel_name,
                    "source_kind": source_kind,
                    "origin": if origin.is_agent() { "agent" } else { "owner" },
                }),
            },
        )
        .await?;
        upsert_agent_thread_subscription(
            pool,
            agent_id,
            channel_id,
            reply_thread_root_id,
            source_kind,
            Some(message_id),
        )
        .await?;
        let scheduled = ensure_agent_inbox_wake_work_item(pool, agent_id)
            .await?
            .is_some_and(|(_, scheduled)| scheduled);
        record_agent_activity(
            pool,
            Some(agent_id),
            None,
            match dispatch_kind {
                DispatchKind::ChannelMessage => "channel",
                DispatchKind::Dm => "dm",
                DispatchKind::Mention => "mention",
                DispatchKind::ThreadFollowUp => "thread",
            },
            match (dispatch_kind, scheduled, origin.is_agent()) {
                (DispatchKind::ChannelMessage, true, _) => "Channel message delivered to inbox",
                (DispatchKind::ChannelMessage, false, _) => "Channel message queued in inbox",
                (DispatchKind::Dm, true, _) => "DM delivered to inbox",
                (DispatchKind::Dm, false, _) => "DM queued in inbox",
                (DispatchKind::ThreadFollowUp, true, _) => "Thread follow-up delivered to inbox",
                (DispatchKind::ThreadFollowUp, false, _) => "Thread follow-up queued in inbox",
                (DispatchKind::Mention, true, true) => "Collaboration delivered to inbox",
                (DispatchKind::Mention, false, true) => "Collaboration queued in inbox",
                (DispatchKind::Mention, true, false) => "Mention delivered to inbox",
                (DispatchKind::Mention, false, false) => "Mention queued in inbox",
            },
            json!({
                "channel": format!("#{channel_name}"),
                "target_agent": format!("@{agent_handle}"),
                "inbox_item_id": inbox_item_id,
                "title": title,
            })
            .to_string(),
        )
        .await?;
    }

    Ok(())
}

async fn load_thread_followup_targets(
    pool: &SqlitePool,
    channel_id: Uuid,
    thread_root_id: Uuid,
) -> CommandResult<Vec<(Uuid, String)>> {
    let rows = sqlx::query(
        r#"
        select a.id, a.handle
        from (
            select agent_id, max(last_at) as last_at
            from (
                select sender_agent_id as agent_id, max(created_at) as last_at
                from messages
                where channel_id = $1
                  and (id = $2 or thread_root_id = $2)
                  and sender_agent_id is not null
                group by sender_agent_id
                union all
                select agent_id, max(created_at) as last_at
                from agent_work_items
                where channel_id = $1
                  and thread_root_id = $2
                group by agent_id
                union all
                select agent_id, max(updated_at) as last_at
                from agent_thread_subscriptions
                where channel_id = $1
                  and thread_root_id = $2
                group by agent_id
            ) candidates
            where agent_id is not null
            group by agent_id
        ) candidates
        join agents a on a.id = candidates.agent_id
        join channel_members cm on cm.channel_id = $1 and cm.agent_id = a.id
        where a.status <> 'error'
        order by candidates.last_at desc, lower(a.handle)
        limit 8
        "#,
    )
    .bind(channel_id)
    .bind(thread_root_id)
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    Ok(rows
        .into_iter()
        .map(|row| (row.get("id"), row.get("handle")))
        .collect())
}

async fn load_channel_root_delivery_targets(
    pool: &SqlitePool,
    channel_id: Uuid,
) -> CommandResult<Vec<(Uuid, String)>> {
    let rows = sqlx::query(
        r#"
        select a.id, a.handle
        from channel_members cm
        join agents a on a.id = cm.agent_id
        where cm.channel_id = $1
          and a.status <> 'error'
        order by lower(a.handle)
        "#,
    )
    .bind(channel_id)
    .fetch_all(pool)
    .await
    .map_err(to_string)?;
    Ok(rows
        .into_iter()
        .map(|row| (row.get("id"), row.get("handle")))
        .collect())
}

async fn inter_agent_thread_message_count_since_last_owner(
    pool: &SqlitePool,
    channel_id: Uuid,
    thread_root_id: Uuid,
) -> CommandResult<i64> {
    let last_owner_created_at: Option<DateTime<Utc>> = sqlx::query_scalar(
        r#"
        select max(created_at)
        from messages
        where channel_id = $1
          and (id = $2 or thread_root_id = $2)
          and sender_role = 'owner'
        "#,
    )
    .bind(channel_id)
    .bind(thread_root_id)
    .fetch_one(pool)
    .await
    .map_err(to_string)?;

    let count = if let Some(last_owner_created_at) = last_owner_created_at {
        sqlx::query_scalar(
            r#"
            select count(*)
            from messages
            where channel_id = $1
              and (id = $2 or thread_root_id = $2)
              and sender_agent_id is not null
              and julianday(created_at) > julianday($3)
            "#,
        )
        .bind(channel_id)
        .bind(thread_root_id)
        .bind(last_owner_created_at)
        .fetch_one(pool)
        .await
        .map_err(to_string)?
    } else {
        sqlx::query_scalar(
            r#"
            select count(*)
            from messages
            where channel_id = $1
              and (id = $2 or thread_root_id = $2)
              and sender_agent_id is not null
            "#,
        )
        .bind(channel_id)
        .bind(thread_root_id)
        .fetch_one(pool)
        .await
        .map_err(to_string)?
    };
    Ok(count)
}

pub(crate) async fn queue_agent_message_mentions(
    pool: &SqlitePool,
    message_id: Uuid,
) -> CommandResult<()> {
    let Some(row) = sqlx::query(
        r#"
        select channel_id, thread_root_id, sender_agent_id, body, is_task
        from messages
        where id = $1
        "#,
    )
    .bind(message_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?
    else {
        return Ok(());
    };

    let Some(sender_agent_id) = row.get::<Option<Uuid>, _>("sender_agent_id") else {
        return Ok(());
    };
    let is_task: bool = row.get("is_task");
    if is_task {
        return Ok(());
    }

    let channel_id: Uuid = row.get("channel_id");
    let thread_root_id: Option<Uuid> = row.get("thread_root_id");
    let body: String = row.get("body");
    queue_mentions_as_work_items(
        pool,
        channel_id,
        thread_root_id,
        message_id,
        None,
        body.trim(),
        MentionDispatchOrigin::Agent {
            sender_agent_id,
            allow_channel_member_invite: false,
        },
    )
    .await
}

pub(crate) async fn resolve_task_for_handoff(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
    task_number: Option<i64>,
) -> CommandResult<(Uuid, i64, String, Uuid, Uuid)> {
    let row = if let Some(task_number) = task_number {
        sqlx::query(
            r#"
            select id, number, title, channel_id, message_id, assignee_agent_id, status
            from tasks
            where number = $1
            "#,
        )
        .bind(task_number)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?
    } else {
        sqlx::query(
            r#"
            select t.id, t.number, t.title, t.channel_id, t.message_id, t.assignee_agent_id, t.status
            from agent_work_items w
            join tasks t on t.id = w.task_id
            where w.run_id = $1 and w.agent_id = $2
            order by w.updated_at desc
            limit 1
            "#,
        )
        .bind(run_id)
        .bind(agent_id)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?
    };
    let row = row.ok_or_else(|| {
        task_number
            .map(|task_number| format!("task #{task_number} does not exist"))
            .unwrap_or_else(|| {
                "task_handoff needs task_number when the current run is not tied to a task"
                    .to_owned()
            })
    })?;
    let resolved_task_number: i64 = row.get("number");
    let status: String = row.get("status");
    if status == "done" {
        return Err(format!("task #{resolved_task_number} is already done"));
    }
    let assignee_agent_id: Option<Uuid> = row.get("assignee_agent_id");
    if assignee_agent_id != Some(agent_id) {
        return Err(format!(
            "task #{resolved_task_number} can only be handed off by its current assignee"
        ));
    }
    Ok((
        row.get("id"),
        resolved_task_number,
        row.get("title"),
        row.get("channel_id"),
        row.get("message_id"),
    ))
}

pub(crate) async fn resolve_agent_by_handle(
    pool: &SqlitePool,
    handle: &str,
) -> CommandResult<Uuid> {
    let normalized = handle.trim().trim_start_matches('@');
    if normalized.is_empty() {
        return Err("assignee handle is empty".to_owned());
    }
    sqlx::query_scalar("select id from agents where lower(handle) = lower($1)")
        .bind(normalized)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?
        .ok_or_else(|| format!("agent @{normalized} does not exist"))
}

pub(crate) async fn resolve_agent_handle(
    pool: &SqlitePool,
    agent_id: Uuid,
) -> CommandResult<String> {
    sqlx::query_scalar("select handle from agents where id = $1")
        .bind(agent_id)
        .fetch_one(pool)
        .await
        .map_err(to_string)
}

pub(crate) async fn ensure_agent_channel_member(
    pool: &SqlitePool,
    agent_id: Uuid,
    channel_id: Uuid,
    event_name: &str,
) -> CommandResult<()> {
    let is_member: bool = sqlx::query_scalar(
        r#"
        select exists (
            select 1 from channel_members
            where channel_id = $1 and agent_id = $2
        )
        "#,
    )
    .bind(channel_id)
    .bind(agent_id)
    .fetch_one(pool)
    .await
    .map_err(to_string)?;
    if is_member {
        Ok(())
    } else {
        Err(format!(
            "{event_name} requires source agent channel membership"
        ))
    }
}

pub(crate) async fn create_agent_handoff(
    pool: &SqlitePool,
    source_agent_id: Uuid,
    channel_id: Uuid,
    thread_root_id: Uuid,
    target_agent: &str,
    reason: Option<&str>,
    body: &str,
) -> CommandResult<(Uuid, String, Uuid, Uuid)> {
    let target_agent_id = resolve_agent_by_handle(pool, target_agent).await?;
    if target_agent_id == source_agent_id {
        return Err("handoff_create target_agent must be a different agent".to_owned());
    }
    if !agent_accepts_new_work(pool, target_agent_id).await? {
        return Err(format!(
            "handoff_create target_agent @{target_agent} is in error state"
        ));
    }
    let target_handle = resolve_agent_handle(pool, target_agent_id).await?;
    let source_handle = resolve_agent_handle(pool, source_agent_id).await?;
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
    ensure_agent_channel_member(pool, source_agent_id, channel_id, "handoff_create").await?;

    let request_body = body.trim();
    if request_body.is_empty() {
        return Err("handoff_create body is required".to_owned());
    }
    let reason = reason
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Agent handoff requested");
    let handoff_message_id = insert_agent_handoff_message(
        pool,
        source_agent_id,
        channel_id,
        thread_root_id,
        request_body,
    )
    .await?;

    sqlx::query(
        r#"
        insert into channel_members (channel_id, agent_id)
        values ($1, $2)
        on conflict (channel_id, agent_id) do nothing
        "#,
    )
    .bind(channel_id)
    .bind(target_agent_id)
    .execute(pool)
    .await
    .map_err(to_string)?;
    upsert_agent_thread_subscription(
        pool,
        target_agent_id,
        channel_id,
        thread_root_id,
        "handoff",
        Some(handoff_message_id),
    )
    .await?;
    let title = request_body
        .lines()
        .next()
        .map(|line| line.chars().take(120).collect::<String>())
        .filter(|line| !line.trim().is_empty())
        .unwrap_or_else(|| format!("Handoff from @{source_handle}"));
    let inbox_item_id = create_agent_inbox_item(
        pool,
        AgentInboxItemInput {
            agent_id: target_agent_id,
            channel_id: Some(channel_id),
            thread_root_id: Some(thread_root_id),
            source_message_id: Some(handoff_message_id),
            task_id: None,
            kind: "handoff",
            priority: 80,
            title: &title,
            body_preview: request_body,
            payload: json!({"source_agent_id": source_agent_id, "reason": reason}),
        },
    )
    .await?;
    let wake = ensure_agent_inbox_wake_work_item(pool, target_agent_id).await?;
    let Some((work_item_id, scheduled)) = wake else {
        return Err("handoff inbox item was not wakeable".to_owned());
    };
    record_agent_activity(
        pool,
        Some(target_agent_id),
        None,
        "handoff",
        if scheduled {
            "Handoff dispatched"
        } else {
            "Handoff queued"
        },
        json!({
            "from": source_handle,
            "reason": reason,
            "inbox_item_id": inbox_item_id,
            "work_item_id": work_item_id,
            "message_id": handoff_message_id,
            "thread_root_id": thread_root_id
        })
        .to_string(),
    )
    .await?;
    Ok((
        target_agent_id,
        target_handle,
        work_item_id,
        handoff_message_id,
    ))
}
