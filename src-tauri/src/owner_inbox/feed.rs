use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::app::{to_string, CommandResult};

const CANDIDATES: &str = include_str!("feed.sql");
const PAGE_SIZE: usize = 30;

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FeedFilter {
    #[default]
    All,
    Unread,
    Thread,
    Mention,
    Dm,
    Channel,
    Task,
    Reminder,
}

impl FeedFilter {
    fn as_str(&self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Unread => "unread",
            Self::Thread => "thread",
            Self::Mention => "mention",
            Self::Dm => "dm",
            Self::Channel => "channel",
            Self::Task => "task",
            Self::Reminder => "reminder",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct FeedCursor {
    pub(crate) timestamp: String,
    pub(crate) id: String,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FeedRequest {
    #[serde(default)]
    pub(crate) filter: FeedFilter,
    #[serde(default)]
    pub(crate) mention_handles: Vec<String>,
    pub(crate) after: Option<FeedCursor>,
    pub(crate) before: Option<FeedCursor>,
}

#[derive(Deserialize)]
pub(crate) struct FeedPageRequest {
    pub(crate) request: FeedRequest,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FeedCountsRequest {
    #[serde(default)]
    pub(crate) mention_handles: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FeedItem {
    pub(crate) id: String,
    pub(crate) dismiss_id: String,
    pub(crate) kind: String,
    pub(crate) title: String,
    pub(crate) excerpt: String,
    pub(crate) surface: String,
    pub(crate) actor: String,
    pub(crate) timestamp: String,
    pub(crate) unread: bool,
    pub(crate) actor_agent_id: Option<Uuid>,
    pub(crate) actor_role: Option<String>,
    pub(crate) channel_id: Option<Uuid>,
    pub(crate) thread_id: Option<Uuid>,
    pub(crate) message_id: Option<Uuid>,
    pub(crate) task_id: Option<Uuid>,
    pub(crate) reminder_id: Option<Uuid>,
    pub(crate) reply_count: i64,
    pub(crate) new_count: i64,
}

impl FeedItem {
    fn cursor(&self) -> FeedCursor {
        FeedCursor {
            timestamp: self.timestamp.clone(),
            id: self.id.clone(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FeedPage {
    pub(crate) items: Vec<FeedItem>,
    pub(crate) next_cursor: Option<FeedCursor>,
    pub(crate) previous_cursor: Option<FeedCursor>,
}

#[derive(Debug, Serialize)]
pub(crate) struct FeedCounts {
    pub(crate) total: i64,
    pub(crate) unread: i64,
}

fn handles_json(handles: &[String]) -> CommandResult<String> {
    if handles.len() > 8 || handles.iter().any(|s| s.len() > 128 || s.trim().is_empty()) {
        return Err("Invalid Activity mention handles".into());
    }
    serde_json::to_string(handles).map_err(to_string)
}

pub(crate) async fn counts(pool: &SqlitePool, handles: &[String]) -> CommandResult<FeedCounts> {
    let sql = format!(
        "{CANDIDATES} select count(*) as total, coalesce(sum(unread),0) as unread from eligible"
    );
    let row = sqlx::query(&sql)
        .bind(handles_json(handles)?)
        .fetch_one(pool)
        .await
        .map_err(to_string)?;
    Ok(FeedCounts {
        total: row.get("total"),
        unread: row.get("unread"),
    })
}

pub(crate) async fn page(pool: &SqlitePool, request: FeedRequest) -> CommandResult<FeedPage> {
    if request.after.is_some() && request.before.is_some() {
        return Err("Use either an after or a before cursor".into());
    }
    let cursor = request.after.as_ref().or(request.before.as_ref());
    if let Some(cursor) = cursor {
        chrono::DateTime::parse_from_rfc3339(&cursor.timestamp).map_err(to_string)?;
        if cursor.id.len() > 128 {
            return Err("Invalid Activity cursor".into());
        }
    }
    let backwards = request.before.is_some();
    let (comparison, order) = if backwards {
        (">", "asc")
    } else {
        ("<", "desc")
    };
    // Page metadata first; only then read bounded text for at most 31 items.
    let sql = format!(
        r#"{CANDIDATES}, page as materialized (
        select * from eligible
        where (?2 = 'all' or (?2 = 'unread' and unread) or kind = ?2)
          and (?3 is null or (julianday(timestamp),id) {comparison} (julianday(?3),?4))
        order by julianday(timestamp) {order}, id {order} limit 31
    )
    select p.*, c.kind as channel_kind, substr(c.name,1,256) as channel_name,
        substr(dm.handle,1,128) as dm_handle,
        substr(m.body,1,2048) as body, substr(m.sender_name,1,256) as sender_name,
        m.sender_agent_id, m.sender_role,
        t.number as task_number, substr(t.title,1,512) as task_title, t.status as task_status,
        t.assignee_agent_id, substr(a.display_name,1,256) as assignee_name,
        substr(r.title,1,512) as reminder_title, substr(r.note,1,2048) as reminder_note
    from page p
    left join channels c on c.id = p.channel_id
    left join agents dm on dm.id = c.dm_agent_id
    left join messages m on m.id = p.source_id
    left join tasks t on t.id = p.task_id
    left join agents a on a.id = t.assignee_agent_id
    left join reminders r on r.id = p.reminder_id
    order by julianday(p.timestamp) {order}, p.id {order}"#
    );
    let rows = sqlx::query(&sql)
        .bind(handles_json(&request.mention_handles)?)
        .bind(request.filter.as_str())
        .bind(cursor.map(|c| &c.timestamp))
        .bind(cursor.map(|c| &c.id))
        .fetch_all(pool)
        .await
        .map_err(to_string)?;
    let more = rows.len() > PAGE_SIZE;
    let mut items = Vec::with_capacity(PAGE_SIZE);
    for row in rows.into_iter().take(PAGE_SIZE) {
        let id: String = row.get("id");
        let kind: String = row.get("kind");
        let body: String = row.get::<Option<String>, _>("body").unwrap_or_default();
        let channel_name: Option<String> = row.get("channel_name");
        let surface = if row.get::<Option<String>, _>("channel_kind").as_deref() == Some("dm") {
            format!(
                "@{}",
                row.get::<Option<String>, _>("dm_handle")
                    .unwrap_or_else(|| "agent".into())
            )
        } else {
            channel_name
                .map(|n| format!("#{n}"))
                .unwrap_or_else(|| "Reminder".into())
        };
        let (title, excerpt, actor, actor_agent_id, actor_role) = match kind.as_str() {
            "task" => (
                format!(
                    "Task #{}: {}",
                    row.get::<i64, _>("task_number"),
                    row.get::<String, _>("task_title")
                ),
                row.get::<Option<String>, _>("assignee_name")
                    .map(|n| format!("Assigned to {n}"))
                    .unwrap_or_else(|| "Unassigned".into()),
                row.get::<String, _>("task_status").replace('_', " "),
                row.get::<Option<Uuid>, _>("assignee_agent_id"),
                Some("agent".into()),
            ),
            "reminder" => (
                row.get("reminder_title"),
                row.get("reminder_note"),
                "Reminder due".into(),
                None,
                None,
            ),
            _ => (
                if kind == "channel" {
                    format!("New activity in {surface}")
                } else if kind == "dm" {
                    format!("DM with {surface}")
                } else {
                    body.lines()
                        .next()
                        .unwrap_or_default()
                        .chars()
                        .take(240)
                        .collect()
                },
                body,
                row.get::<Option<String>, _>("sender_name")
                    .unwrap_or_default(),
                row.get("sender_agent_id"),
                row.get("sender_role"),
            ),
        };
        items.push(FeedItem {
            dismiss_id: id.clone(),
            id,
            kind,
            title,
            excerpt,
            actor,
            actor_agent_id,
            actor_role,
            surface,
            timestamp: row.get("timestamp"),
            unread: row.get("unread"),
            channel_id: row.get("channel_id"),
            thread_id: row.get("thread_id"),
            message_id: row.get("message_id"),
            task_id: row.get("task_id"),
            reminder_id: row.get("reminder_id"),
            reply_count: row.get("reply_count"),
            new_count: row.get("new_count"),
        });
    }
    if backwards {
        items.reverse();
    }
    let next_cursor = if (backwards && cursor.is_some()) || (!backwards && more) {
        items.last().map(FeedItem::cursor)
    } else {
        None
    };
    let previous_cursor = if (backwards && more) || (!backwards && cursor.is_some()) {
        items.first().map(FeedItem::cursor)
    } else {
        None
    };
    Ok(FeedPage {
        items,
        next_cursor,
        previous_cursor,
    })
}
