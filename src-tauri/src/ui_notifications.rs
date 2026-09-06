use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::{
    sqlite::SqliteRow, Connection, Executor, Row, Sqlite, SqliteConnection, SqlitePool, Transaction,
};
use tauri::{Emitter, State};
use tokio::time::sleep;
use uuid::Uuid;

use crate::agent_inbox_wake::sync_inbox_for_work_item;
use crate::app::{to_string, AppState, CommandResult};
use crate::message_store::load_message_patch_in_tx;
use crate::models::{
    AgentActivity, AgentRunPatch, AgentSubscriptionStatus, AgentWorkItemPatch, Artifact,
    ChannelMember, Message,
};

const UI_REFRESH_EVENT: &str = "lantor://refresh";

// Separate scalar aggregates let SQLite seek both rowid endpoints. Combining
// min/max in one aggregate scans the retained event history on every drain.
pub(crate) const UI_EVENT_BOUNDS_SQL: &str = "select
    (select coalesce(min(id), 0) from ui_events) as min_id,
    (select coalesce(max(id), 0) from ui_events) as max_id";

#[derive(Clone, Debug, Serialize)]
pub(crate) struct UiEventDelivery {
    pub(crate) cursor: i64,
    pub(crate) event: String,
}

#[derive(Debug, Serialize)]
struct UiEventDeliveryBatch {
    #[serde(rename = "type")]
    event_type: &'static str,
    events: Vec<UiEventDelivery>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UiEventReplay {
    pub(crate) cursor: i64,
    pub(crate) replay_gap: bool,
    pub(crate) events: Vec<UiEventDelivery>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum UiEvent<'a> {
    Refresh {
        reason: &'a str,
    },
    MessageUpsert {
        reason: &'a str,
        message: &'a Message,
    },
    MessageDelta {
        reason: &'a str,
        message_id: Uuid,
        append: &'a str,
        body_length: usize,
        delivery_state: &'a str,
    },
    MessageDelete {
        reason: &'a str,
        message_id: Uuid,
    },
    ActivityUpsert {
        reason: &'a str,
        activity: &'a AgentActivity,
    },
    AgentRunUpsert {
        reason: &'a str,
        run: &'a AgentRunPatch,
    },
    AgentSubscriptionStatusUpsert {
        reason: &'a str,
        agent_id: Uuid,
        subscription_status: &'a AgentSubscriptionStatus,
    },
    WorkItemUpsert {
        reason: &'a str,
        work_item: &'a AgentWorkItemPatch,
    },
    ArtifactUpsert {
        reason: &'a str,
        artifact: UiArtifactPayload<'a>,
    },
    ChannelMemberUpsert {
        reason: &'a str,
        member: &'a ChannelMember,
    },
    ChannelMemberRemove {
        reason: &'a str,
        channel_id: Uuid,
        agent_id: Uuid,
    },
}

#[derive(Debug, Serialize)]
pub(crate) struct UiArtifactPayload<'a> {
    id: Uuid,
    message_id: Uuid,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    creator_agent_id: Option<Uuid>,
    creator_agent_handle: Option<&'a str>,
    kind: &'a str,
    title: &'a str,
    summary: &'a str,
    content: &'static str,
    metadata: &'a Value,
    created_at: &'a DateTime<Utc>,
    updated_at: &'a DateTime<Utc>,
}

impl<'a> From<&'a Artifact> for UiArtifactPayload<'a> {
    fn from(artifact: &'a Artifact) -> Self {
        Self {
            id: artifact.id,
            message_id: artifact.message_id,
            channel_id: artifact.channel_id,
            thread_root_id: artifact.thread_root_id,
            creator_agent_id: artifact.creator_agent_id,
            creator_agent_handle: artifact.creator_agent_handle.as_deref(),
            kind: &artifact.kind,
            title: &artifact.title,
            summary: &artifact.summary,
            content: "",
            metadata: &artifact.metadata,
            created_at: &artifact.created_at,
            updated_at: &artifact.updated_at,
        }
    }
}

async fn insert_ui_event<'e, E>(executor: E, event: &UiEvent<'_>) -> CommandResult<()>
where
    E: Executor<'e, Database = Sqlite>,
{
    let event_json = serde_json::to_string(event).map_err(to_string)?;
    sqlx::query("insert into ui_events (event_json) values ($1)")
        .bind(event_json)
        .execute(executor)
        .await
        .map_err(to_string)?;

    Ok(())
}

pub(crate) async fn enqueue_ui_event(pool: &SqlitePool, event: &UiEvent<'_>) -> CommandResult<()> {
    insert_ui_event(pool, event).await
}

pub(crate) async fn enqueue_ui_event_in_tx(
    transaction: &mut Transaction<'_, Sqlite>,
    event: &UiEvent<'_>,
) -> CommandResult<()> {
    insert_ui_event(&mut **transaction, event).await
}

pub(crate) async fn enqueue_ui_agent_run_changed_in_tx(
    transaction: &mut Transaction<'_, Sqlite>,
    run_id: Uuid,
    reason: &str,
) -> CommandResult<()> {
    let run = load_agent_run_patch_in_tx(transaction, run_id).await?;
    enqueue_ui_event_in_tx(transaction, &UiEvent::AgentRunUpsert { reason, run: &run }).await
}

pub(crate) async fn enqueue_ui_work_item_changed_in_tx(
    transaction: &mut Transaction<'_, Sqlite>,
    work_item_id: Uuid,
    reason: &str,
) -> CommandResult<()> {
    let work_item = load_agent_work_item_patch_in_tx(transaction, work_item_id).await?;
    enqueue_ui_event_in_tx(
        transaction,
        &UiEvent::WorkItemUpsert {
            reason,
            work_item: &work_item,
        },
    )
    .await
}

pub(crate) async fn reconcile_work_item_change(
    pool: &SqlitePool,
    work_item_id: Uuid,
    reason: &str,
) -> CommandResult<()> {
    // Inbox rollover and timeline narration may create more business records,
    // so they deliberately run after the work-item transaction. Preserve the
    // prior best-effort behavior: the already-committed lifecycle transition
    // must not be reported as failed because a follow-on reconciliation fails.
    let _ = sync_inbox_for_work_item(pool, work_item_id).await;
    if let Ok(work_item) = load_agent_work_item_patch(pool, work_item_id).await {
        let _ = maybe_insert_work_item_system_message(pool, &work_item, reason).await;
    }
    Ok(())
}

pub(crate) async fn insert_system_message(
    pool: &SqlitePool,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    body: impl AsRef<str>,
) -> CommandResult<Uuid> {
    let body = body.as_ref().trim();
    if body.is_empty() {
        return Err("system message body is empty".to_owned());
    }
    let mut transaction = pool.begin().await.map_err(to_string)?;
    let message_id: Uuid = sqlx::query_scalar(
        r#"
        insert into messages (channel_id, thread_root_id, sender_name, sender_role, body, is_task)
        values ($1, $2, 'Lantor', 'system', $3, false)
        returning id
        "#,
    )
    .bind(channel_id)
    .bind(thread_root_id)
    .bind(body)
    .fetch_one(&mut *transaction)
    .await
    .map_err(to_string)?;
    let message = load_message_patch_in_tx(&mut transaction, message_id).await?;
    enqueue_ui_event_in_tx(
        &mut transaction,
        &UiEvent::MessageUpsert {
            reason: "system_message",
            message: &message,
        },
    )
    .await?;
    transaction.commit().await.map_err(to_string)?;
    Ok(message_id)
}

async fn maybe_insert_work_item_system_message(
    pool: &SqlitePool,
    work_item: &AgentWorkItemPatch,
    reason: &str,
) -> CommandResult<()> {
    // Conversation-triggered agent turns are attention events, not timeline-level tasks.
    // Keep normal lifecycle messages for explicit task-backed work only; still surface
    // exceptional failures/cancellations for conversational turns.
    if work_item.task_number.is_none()
        && !matches!(reason, "work_item_failed" | "work_item_cancelled")
    {
        return Ok(());
    }
    if work_item.task_number.is_some()
        && matches!(
            reason,
            "work_item_created" | "work_item_queued" | "work_item_running"
        )
    {
        return Ok(());
    }
    if work_item.task_number.is_some() {
        if let Some(task_id) = work_item.task_id {
            let task_row = sqlx::query(
                "select coalesce(assignee_agent_id = $2, false) as is_assignee, status from tasks where id = $1",
            )
            .bind(task_id)
            .bind(work_item.agent_id)
            .fetch_optional(pool)
            .await
            .map_err(to_string)?;
            let Some(task_row) = task_row else {
                return Ok(());
            };
            if !task_row.get::<bool, _>("is_assignee") {
                return Ok(());
            }
            let task_status: String = task_row.get("status");
            if reason == "work_item_finished"
                && work_item.status == "done"
                && matches!(task_status.as_str(), "todo" | "in_progress")
            {
                return Ok(());
            }
        }
    }
    let Some(channel_id) = work_item.channel_id else {
        return Ok(());
    };
    let thread_root_id = work_item.thread_root_id.or(work_item.source_message_id);
    let object_label = work_item
        .task_number
        .map(|number| format!("task run for task #{number}"))
        .unwrap_or_else(|| "agent request".to_owned());
    let title = work_item.title.trim();
    let title_suffix = if title.is_empty() {
        String::new()
    } else {
        format!(": {title}")
    };
    let body = match reason {
        "work_item_created" | "work_item_queued" => {
            format!(
                "@{} queued {}{}",
                work_item.agent_handle, object_label, title_suffix
            )
        }
        "work_item_running" => {
            format!(
                "@{} started {}{}",
                work_item.agent_handle, object_label, title_suffix
            )
        }
        "work_item_cancelling" => {
            format!(
                "@{} is stopping {}{}",
                work_item.agent_handle, object_label, title_suffix
            )
        }
        "work_item_cancelled" => {
            format!(
                "@{} cancelled {}{}",
                work_item.agent_handle, object_label, title_suffix
            )
        }
        "work_item_failed" => {
            format!(
                "@{} failed {}{}",
                work_item.agent_handle, object_label, title_suffix
            )
        }
        "work_item_finished" => match work_item.status.as_str() {
            "done" => format!(
                "@{} completed {}{}",
                work_item.agent_handle, object_label, title_suffix
            ),
            "failed" => format!(
                "@{} failed {}{}",
                work_item.agent_handle, object_label, title_suffix
            ),
            "cancelled" => format!(
                "@{} cancelled {}{}",
                work_item.agent_handle, object_label, title_suffix
            ),
            "silent" => return Ok(()),
            _ => return Ok(()),
        },
        _ => return Ok(()),
    };
    insert_system_message(pool, channel_id, thread_root_id, body).await?;
    Ok(())
}

pub(crate) async fn notify_supervisor_wake(_pool: &SqlitePool) -> CommandResult<()> {
    // SQLite has no cross-process NOTIFY primitive. The supervisor observes
    // queued commands through its existing short poll interval.
    Ok(())
}

pub(crate) async fn load_ui_event_replay_from_cursor(
    pool: &SqlitePool,
    requested_cursor: i64,
) -> CommandResult<UiEventReplay> {
    let mut connection = pool.acquire().await.map_err(to_string)?;
    read_ui_event_replay(&mut connection, requested_cursor).await
}

pub(crate) async fn read_ui_event_replay(
    connection: &mut SqliteConnection,
    requested_cursor: i64,
) -> CommandResult<UiEventReplay> {
    let mut transaction = connection.begin().await.map_err(to_string)?;
    let row = sqlx::query(UI_EVENT_BOUNDS_SQL)
        .fetch_one(&mut *transaction)
        .await
        .map_err(to_string)?;
    let min_id: i64 = row.get("min_id");
    let max_id: i64 = row.get("max_id");
    let requested_cursor = requested_cursor.max(0);
    let replay_gap =
        (min_id > 0 && requested_cursor < min_id.saturating_sub(1)) || requested_cursor > max_id;
    if replay_gap {
        transaction.commit().await.map_err(to_string)?;
        return Ok(UiEventReplay {
            cursor: max_id,
            replay_gap: true,
            events: Vec::new(),
        });
    }

    let rows = sqlx::query(
        r#"
        select id, event_json
        from ui_events
        where id > $1
          and id <= $2
        order by id asc
        "#,
    )
    .bind(requested_cursor)
    .bind(max_id)
    .fetch_all(&mut *transaction)
    .await
    .map_err(to_string)?;
    transaction.commit().await.map_err(to_string)?;
    Ok(UiEventReplay {
        cursor: max_id,
        replay_gap: false,
        events: rows
            .into_iter()
            .map(|row| UiEventDelivery {
                cursor: row.get("id"),
                event: row.get("event_json"),
            })
            .collect(),
    })
}

#[tauri::command]
pub(crate) async fn replay_ui_events(
    cursor: i64,
    state: State<'_, AppState>,
) -> CommandResult<UiEventReplay> {
    load_ui_event_replay_from_cursor(&state.pool, cursor).await
}

pub(crate) fn spawn_ui_refresh_listener(app: tauri::AppHandle, pool: SqlitePool) {
    tauri::async_runtime::spawn(async move {
        let mut cursor = None;
        loop {
            let mut subscription =
                match crate::ui_event_hub::UiEventSubscription::connect(&pool, cursor).await {
                    Ok(subscription) => subscription,
                    Err(err) => {
                        eprintln!("Lantor UI event subscription failed: {err}");
                        sleep(Duration::from_secs(2)).await;
                        continue;
                    }
                };
            loop {
                match subscription.recv().await {
                    Ok(mut batch) => {
                        cursor = Some(batch.cursor);
                        if batch.replay_gap {
                            batch.events.push(UiEventDelivery {
                                cursor: batch.cursor,
                                event: r#"{"type":"refresh","reason":"event_replay_gap"}"#.into(),
                            });
                        }
                        if batch.events.is_empty() {
                            continue;
                        }
                        match serde_json::to_string(&UiEventDeliveryBatch {
                            event_type: "ui_event_delivery",
                            events: batch.events,
                        }) {
                            Ok(payload) => {
                                let _ = app.emit(UI_REFRESH_EVENT, payload);
                            }
                            Err(err) => eprintln!("Lantor UI event serialization failed: {err}"),
                        }
                    }
                    Err(err) => {
                        eprintln!("Lantor UI event delivery failed: {err}");
                        sleep(Duration::from_secs(2)).await;
                        break;
                    }
                }
            }
        }
    });
}

/// Number of most-recent `ui_events` rows to retain on each prune.
///
/// `ui_events` is an append-only UI-refresh notification queue. The desktop
/// event hub and owner-inbox consumers tail it, while desktop and web clients can
/// replay from their last delivered event id. Retaining several thousand rows
/// gives reconnecting clients a large replay window; if a cursor falls behind
/// this window, the transport explicitly requests a snapshot refresh. `id` is
/// `autoincrement` (monotonic, never reused), so pruning by id cannot duplicate a
/// consumer cursor.
const UI_EVENTS_RETAIN_ROWS: i64 = 5_000;

/// How often the background pruner runs after its initial pass.
const UI_EVENTS_PRUNE_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Reclaim the database file once the freelist holds at least this many bytes.
/// VACUUM rewrites the whole file, so it only fires when there is meaningful
/// slack to recover — in practice the first run after this ships, then dormant.
const UI_EVENTS_VACUUM_THRESHOLD_BYTES: i64 = 32 * 1024 * 1024;

/// Periodically trims the ephemeral `ui_events` queue so it cannot grow without
/// bound. Runs once shortly after startup (clearing any historical backlog) and
/// then once per day.
pub(crate) fn spawn_ui_events_pruner(pool: SqlitePool) {
    tauri::async_runtime::spawn(async move {
        loop {
            match prune_ui_events(&pool).await {
                Ok(deleted) => {
                    if deleted > 0 {
                        eprintln!("Lantor ui_events pruner removed {deleted} old rows");
                        maybe_vacuum_ui_events(&pool).await;
                        // Keep the -wal file bounded after a large delete.
                        let _ = sqlx::query("pragma wal_checkpoint(truncate)")
                            .execute(&pool)
                            .await;
                    }
                }
                Err(err) => eprintln!("Lantor ui_events pruner failed: {err}"),
            }
            sleep(UI_EVENTS_PRUNE_INTERVAL).await;
        }
    });
}

/// Delete all but the most recent `UI_EVENTS_RETAIN_ROWS` rows by id. Returns the
/// number of rows removed. A no-op when the table is already within the window
/// (the `max(id) - N` cutoff is then below every existing id).
async fn prune_ui_events(pool: &SqlitePool) -> Result<u64, sqlx::Error> {
    let result =
        sqlx::query("delete from ui_events where id <= (select max(id) from ui_events) - $1")
            .bind(UI_EVENTS_RETAIN_ROWS)
            .execute(pool)
            .await?;
    Ok(result.rows_affected())
}

/// Best-effort one-shot space reclaim: VACUUM only when the freelist is large.
/// Failures (e.g. transient lock) are logged and ignored — freed pages are still
/// reused by future inserts, so the table stays bounded regardless.
async fn maybe_vacuum_ui_events(pool: &SqlitePool) {
    let free_pages: i64 = sqlx::query_scalar("pragma freelist_count")
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    let page_size: i64 = sqlx::query_scalar("pragma page_size")
        .fetch_one(pool)
        .await
        .unwrap_or(4096);
    if free_pages.saturating_mul(page_size) < UI_EVENTS_VACUUM_THRESHOLD_BYTES {
        return;
    }
    if let Err(err) = sqlx::query("vacuum").execute(pool).await {
        eprintln!("Lantor ui_events VACUUM skipped: {err}");
    }
}

async fn load_agent_run_patch_in_tx(
    transaction: &mut Transaction<'_, Sqlite>,
    run_id: Uuid,
) -> CommandResult<AgentRunPatch> {
    let row = sqlx::query(
        r#"
        select
            r.id,
            r.agent_id,
            a.handle as agent_handle,
            r.work_item_id,
            r.command,
            r.working_directory,
            r.status,
            r.pid,
            r.exit_code,
            r.input_tokens,
            r.output_tokens,
            r.cost_micros,
            r.started_at,
            r.stopped_at
        from agent_runs r
        join agents a on a.id = r.agent_id
        where r.id = $1
        "#,
    )
    .bind(run_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(to_string)?;

    Ok(agent_run_patch_from_row(&row))
}

fn agent_run_patch_from_row(row: &SqliteRow) -> AgentRunPatch {
    AgentRunPatch {
        id: row.get("id"),
        agent_id: row.get("agent_id"),
        agent_handle: row.get("agent_handle"),
        work_item_id: row.get("work_item_id"),
        command: row.get("command"),
        working_directory: row.get("working_directory"),
        status: row.get("status"),
        pid: row.get("pid"),
        exit_code: row.get("exit_code"),
        input_tokens: row.get("input_tokens"),
        output_tokens: row.get("output_tokens"),
        cost_micros: row.get("cost_micros"),
        started_at: row.get("started_at"),
        stopped_at: row.get("stopped_at"),
    }
}

async fn load_agent_work_item_patch(
    pool: &SqlitePool,
    work_item_id: Uuid,
) -> CommandResult<AgentWorkItemPatch> {
    let row = sqlx::query(
        r#"
        select
            w.id,
            w.agent_id,
            a.handle as agent_handle,
            w.channel_id,
            c.name as channel_name,
            w.thread_root_id,
            w.source_message_id,
            w.inbox_item_id,
            w.task_id,
            t.number as task_number,
            w.source_kind,
            w.title,
            w.status,
            w.run_id,
            w.created_at,
            w.updated_at,
            w.completed_at
        from agent_work_items w
        join agents a on a.id = w.agent_id
        left join channels c on c.id = w.channel_id
        left join tasks t on t.id = w.task_id
        where w.id = $1
        "#,
    )
    .bind(work_item_id)
    .fetch_one(pool)
    .await
    .map_err(to_string)?;

    Ok(agent_work_item_patch_from_row(&row))
}

async fn load_agent_work_item_patch_in_tx(
    transaction: &mut Transaction<'_, Sqlite>,
    work_item_id: Uuid,
) -> CommandResult<AgentWorkItemPatch> {
    let row = sqlx::query(
        r#"
        select
            w.id,
            w.agent_id,
            a.handle as agent_handle,
            w.channel_id,
            c.name as channel_name,
            w.thread_root_id,
            w.source_message_id,
            w.inbox_item_id,
            w.task_id,
            t.number as task_number,
            w.source_kind,
            w.title,
            w.status,
            w.run_id,
            w.created_at,
            w.updated_at,
            w.completed_at
        from agent_work_items w
        join agents a on a.id = w.agent_id
        left join channels c on c.id = w.channel_id
        left join tasks t on t.id = w.task_id
        where w.id = $1
        "#,
    )
    .bind(work_item_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(to_string)?;

    Ok(agent_work_item_patch_from_row(&row))
}

fn agent_work_item_patch_from_row(row: &SqliteRow) -> AgentWorkItemPatch {
    AgentWorkItemPatch {
        id: row.get("id"),
        agent_id: row.get("agent_id"),
        agent_handle: row.get("agent_handle"),
        channel_id: row.get("channel_id"),
        channel_name: row.get("channel_name"),
        thread_root_id: row.get("thread_root_id"),
        source_message_id: row.get("source_message_id"),
        inbox_item_id: row.get("inbox_item_id"),
        task_id: row.get("task_id"),
        task_number: row.get("task_number"),
        source_kind: row.get("source_kind"),
        title: row.get("title"),
        status: row.get("status"),
        run_id: row.get("run_id"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        completed_at: row.get("completed_at"),
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::Value;
    use uuid::Uuid;

    use crate::models::Artifact;
    use crate::test_support::{drop_test_schema, insert_test_agent, test_pool};

    use super::{
        enqueue_ui_agent_run_changed_in_tx, enqueue_ui_event, enqueue_ui_event_in_tx,
        enqueue_ui_work_item_changed_in_tx, load_ui_event_replay_from_cursor, UiEvent,
    };

    #[tokio::test]
    async fn ui_event_outbox_commits_and_rolls_back_with_business_writes() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let mut transaction = pool.begin().await.map_err(|err| err.to_string())?;
            sqlx::query("update owner_profile set description = 'rolled back' where id = 1")
                .execute(&mut *transaction)
                .await
                .map_err(|err| err.to_string())?;
            enqueue_ui_event_in_tx(
                &mut transaction,
                &UiEvent::Refresh {
                    reason: "rolled_back",
                },
            )
            .await?;
            transaction
                .rollback()
                .await
                .map_err(|err| err.to_string())?;

            let description: String =
                sqlx::query_scalar("select description from owner_profile where id = 1")
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            let event_count: i64 = sqlx::query_scalar("select count(*) from ui_events")
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
            assert_eq!(description, "local owner");
            assert_eq!(event_count, 0);

            let mut transaction = pool.begin().await.map_err(|err| err.to_string())?;
            sqlx::query("update owner_profile set description = 'committed' where id = 1")
                .execute(&mut *transaction)
                .await
                .map_err(|err| err.to_string())?;
            enqueue_ui_event_in_tx(
                &mut transaction,
                &UiEvent::Refresh {
                    reason: "committed",
                },
            )
            .await?;
            transaction.commit().await.map_err(|err| err.to_string())?;

            let description: String =
                sqlx::query_scalar("select description from owner_profile where id = 1")
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            let event_json: String =
                sqlx::query_scalar("select event_json from ui_events order by id desc limit 1")
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(description, "committed");
            assert_eq!(
                serde_json::from_str::<Value>(&event_json).map_err(|err| err.to_string())?,
                serde_json::json!({
                    "type": "refresh",
                    "reason": "committed",
                }),
            );
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn desktop_ui_event_replay_uses_cursor_and_detects_pruned_gaps() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let mut event_ids = Vec::new();
            for reason in ["one", "two", "three"] {
                let event_id: i64 = sqlx::query_scalar(
                    "insert into ui_events (event_json) values ($1) returning id",
                )
                .bind(format!(r#"{{"type":"refresh","reason":"{reason}"}}"#))
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
                event_ids.push(event_id);
            }

            let replay = load_ui_event_replay_from_cursor(&pool, event_ids[0]).await?;
            assert!(!replay.replay_gap);
            assert_eq!(replay.cursor, event_ids[2]);
            assert_eq!(
                replay
                    .events
                    .iter()
                    .map(|event| event.cursor)
                    .collect::<Vec<_>>(),
                event_ids[1..],
            );

            sqlx::query("delete from ui_events where id < $1")
                .bind(event_ids[2])
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            let gap = load_ui_event_replay_from_cursor(&pool, event_ids[0]).await?;
            assert!(gap.replay_gap);
            assert_eq!(gap.cursor, event_ids[2]);
            assert!(gap.events.is_empty());
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn lifecycle_patch_events_share_the_business_transaction() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "outbox-lifecycle").await?;
            let work_item_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_work_items (agent_id, source_kind, title, context, status)
                values ($1, 'test', 'Lifecycle event', '', 'queued')
                returning id
                "#,
            )
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let run_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_runs (agent_id, work_item_id, command, working_directory, status)
                values ($1, $2, 'test', '', 'starting')
                returning id
                "#,
            )
            .bind(agent_id)
            .bind(work_item_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let event_cursor: i64 =
                sqlx::query_scalar("select coalesce(max(id), 0) from ui_events")
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;

            let mut transaction = pool.begin().await.map_err(|err| err.to_string())?;
            sqlx::query("update agent_runs set status = 'running' where id = $1")
                .bind(run_id)
                .execute(&mut *transaction)
                .await
                .map_err(|err| err.to_string())?;
            sqlx::query(
                "update agent_work_items set status = 'running', run_id = $2 where id = $1",
            )
            .bind(work_item_id)
            .bind(run_id)
            .execute(&mut *transaction)
            .await
            .map_err(|err| err.to_string())?;
            enqueue_ui_agent_run_changed_in_tx(&mut transaction, run_id, "rolled_back").await?;
            enqueue_ui_work_item_changed_in_tx(&mut transaction, work_item_id, "rolled_back")
                .await?;
            transaction
                .rollback()
                .await
                .map_err(|err| err.to_string())?;

            let run_status: String =
                sqlx::query_scalar("select status from agent_runs where id = $1")
                    .bind(run_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            let work_status: String =
                sqlx::query_scalar("select status from agent_work_items where id = $1")
                    .bind(work_item_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            let rolled_back_events: i64 =
                sqlx::query_scalar("select count(*) from ui_events where id > $1")
                    .bind(event_cursor)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(run_status, "starting");
            assert_eq!(work_status, "queued");
            assert_eq!(rolled_back_events, 0);

            let mut transaction = pool.begin().await.map_err(|err| err.to_string())?;
            sqlx::query("update agent_runs set status = 'running' where id = $1")
                .bind(run_id)
                .execute(&mut *transaction)
                .await
                .map_err(|err| err.to_string())?;
            sqlx::query(
                "update agent_work_items set status = 'running', run_id = $2 where id = $1",
            )
            .bind(work_item_id)
            .bind(run_id)
            .execute(&mut *transaction)
            .await
            .map_err(|err| err.to_string())?;
            enqueue_ui_agent_run_changed_in_tx(&mut transaction, run_id, "committed").await?;
            enqueue_ui_work_item_changed_in_tx(&mut transaction, work_item_id, "committed").await?;
            transaction.commit().await.map_err(|err| err.to_string())?;

            let event_json: Vec<String> = sqlx::query_scalar(
                "select event_json from ui_events where id > $1 order by id asc",
            )
            .bind(event_cursor)
            .fetch_all(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let events = event_json
                .iter()
                .map(|event| serde_json::from_str::<Value>(event))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|err| err.to_string())?;
            assert_eq!(events.len(), 2);
            assert_eq!(events[0]["type"], "agent_run_upsert");
            assert_eq!(events[0]["run"]["status"], "running");
            assert_eq!(events[1]["type"], "work_item_upsert");
            assert_eq!(events[1]["work_item"]["status"], "running");
            assert_eq!(events[1]["work_item"]["run_id"], run_id.to_string());
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn artifact_upsert_event_omits_artifact_content() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let artifact = Artifact {
                id: Uuid::new_v4(),
                message_id: Uuid::new_v4(),
                channel_id: Uuid::new_v4(),
                thread_root_id: None,
                creator_agent_id: None,
                creator_agent_handle: None,
                kind: "markdown".to_owned(),
                title: "Large report".to_owned(),
                summary: "short summary".to_owned(),
                content: "large artifact content".to_owned(),
                metadata: Value::Object(Default::default()),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            };

            enqueue_ui_event(
                &pool,
                &UiEvent::ArtifactUpsert {
                    reason: "test",
                    artifact: (&artifact).into(),
                },
            )
            .await
            .map_err(|err| err.to_string())?;
            let event_json: String =
                sqlx::query_scalar("select event_json from ui_events order by id desc limit 1")
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            let payload: Value =
                serde_json::from_str(&event_json).map_err(|err| err.to_string())?;
            assert_eq!(payload["artifact"]["title"], "Large report");
            assert_eq!(payload["artifact"]["summary"], "short summary");
            assert_eq!(payload["artifact"]["content"], "");
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }
}
