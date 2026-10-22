use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{Executor, Row, Sqlite, SqlitePool};
use tauri::State;
use uuid::Uuid;

use crate::agent_routing::ensure_agent_channel_member;
use crate::events::activity::record_agent_activity;
use crate::models::Reminder;
use crate::ui_notifications::{enqueue_ui_event_in_tx, insert_system_message, UiEvent};
use crate::{
    app::{to_string, AppState, CommandResult},
    create_agent_inbox_item, ensure_agent_inbox_wake_work_item, AgentInboxItemInput,
};

use super::parse_due_at;
use super::reminder_checks::{check_timeout, spawn_reminder_check, ReminderCheckJob};

pub(super) async fn process_due_reminders(pool: &SqlitePool) -> CommandResult<()> {
    let mut transaction = pool.begin().await.map_err(to_string)?;
    let rows = sqlx::query(
        r#"
        update reminders
        set status = case when recurrence = 'none' then 'fired' else 'scheduled' end,
            fired_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
            -- Anchor the next occurrence to the original due time instead of
            -- the processing time, so late processing (sleep, app closed)
            -- does not permanently drift the schedule. Skips missed periods.
            due_at = case
                when recurrence = 'daily' then
                    case
                        when julianday(coalesce(recurrence_anchor_at, due_at)) > julianday('now')
                            then coalesce(recurrence_anchor_at, due_at)
                        else strftime('%Y-%m-%dT%H:%M:%f+00:00',
                            coalesce(recurrence_anchor_at, due_at),
                            '+' || (
                                cast(
                                    julianday('now')
                                        - julianday(coalesce(recurrence_anchor_at, due_at))
                                    as integer
                                ) + 1
                            ) || ' days')
                    end
                when recurrence = 'weekly' then
                    case
                        when julianday(coalesce(recurrence_anchor_at, due_at)) > julianday('now')
                            then coalesce(recurrence_anchor_at, due_at)
                        else strftime('%Y-%m-%dT%H:%M:%f+00:00',
                            coalesce(recurrence_anchor_at, due_at),
                            '+' || (
                                (
                                    cast(
                                        (
                                            julianday('now')
                                                - julianday(coalesce(recurrence_anchor_at, due_at))
                                        ) / 7
                                        as integer
                                    ) + 1
                                ) * 7
                            ) || ' days')
                    end
                -- Loops keep a fixed cadence from their anchor and skip
                -- missed ticks instead of firing a burst after sleep.
                when recurrence = 'interval' then
                    case
                        when julianday(coalesce(recurrence_anchor_at, due_at)) > julianday('now')
                            then coalesce(recurrence_anchor_at, due_at)
                        else strftime('%Y-%m-%dT%H:%M:%f+00:00',
                            coalesce(recurrence_anchor_at, due_at),
                            '+' || (
                                (
                                    cast(
                                        (
                                            julianday('now')
                                                - julianday(coalesce(recurrence_anchor_at, due_at))
                                        ) * 86400.0 / max(coalesce(interval_seconds, 0), 60)
                                        as integer
                                    ) + 1
                                ) * max(coalesce(interval_seconds, 0), 60)
                            ) || ' seconds')
                    end
                else due_at
            end,
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id in (
            select id
            from reminders
            where status = 'scheduled'
              and due_at <= strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
            order by due_at asc
            limit 12
        )
        returning id, channel_id, creator_agent_id, thread_root_id, title, note, recurrence, status, due_at,
            interval_seconds, check_command, check_action
        "#,
    )
    .fetch_all(&mut *transaction)
    .await
    .map_err(to_string)?;

    if !rows.is_empty() {
        enqueue_ui_event_in_tx(
            &mut transaction,
            &UiEvent::Refresh {
                reason: "reminder_due",
            },
        )
        .await?;
    }
    transaction.commit().await.map_err(to_string)?;
    for row in rows {
        let reminder_id: Uuid = row.get("id");
        let channel_id: Option<Uuid> = row.get("channel_id");
        let creator_agent_id: Option<Uuid> = row.get("creator_agent_id");
        let thread_root_id: Option<Uuid> = row.get("thread_root_id");
        let title: String = row.get("title");
        let note: String = row.get("note");
        let recurrence: String = row.get("recurrence");
        let status: String = row.get("status");
        let next_due_at: DateTime<Utc> = row.get("due_at");
        let check_command: String = row.get("check_command");
        if !check_command.trim().is_empty() {
            let interval_seconds = row.get::<Option<i64>, _>("interval_seconds").unwrap_or(
                match recurrence.as_str() {
                    "weekly" => 7 * 24 * 60 * 60,
                    "daily" => 24 * 60 * 60,
                    _ => MIN_INTERVAL_SECONDS,
                },
            );
            // A loop runs its check command first; only output reaches the
            // thread or the agent, so quiet ticks leave no message or event.
            spawn_reminder_check(
                pool.clone(),
                ReminderCheckJob {
                    reminder_id,
                    creator_agent_id,
                    channel_id,
                    thread_root_id,
                    title,
                    check_command,
                    check_action: row.get("check_action"),
                    interval_seconds,
                    timeout: check_timeout(interval_seconds),
                },
            );
            continue;
        }
        let fired_detail = if recurrence == "none" {
            String::new()
        } else {
            format!("next_due_at={}", next_due_at.to_rfc3339())
        };
        insert_reminder_event(pool, reminder_id, "fired", &fired_detail).await?;

        if let Some(channel_id) = channel_id {
            let mut body = format!("Reminder: {title}");
            if !note.trim().is_empty() {
                body.push_str(&format!("\n{}", note.trim()));
            }
            if recurrence != "none" && status == "scheduled" {
                body.push_str(&format!("\nNext reminder: {}", next_due_at.to_rfc3339()));
            }
            if let Ok(message_id) =
                insert_system_message(pool, channel_id, thread_root_id, body).await
            {
                if let Some(agent_id) = creator_agent_id {
                    let _ = dispatch_due_reminder_to_agent(
                        pool,
                        reminder_id,
                        agent_id,
                        channel_id,
                        thread_root_id,
                        message_id,
                        &title,
                        &note,
                    )
                    .await;
                }
            }
        }
    }
    Ok(())
}

/// A one-shot reminder stays due until its agent follow-up has finished.
/// Also reconciles reminders fired before this lifecycle transition existed.
pub(crate) async fn complete_successful_agent_reminders(
    pool: &SqlitePool,
    work_item_id: Option<Uuid>,
) -> CommandResult<()> {
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(to_string)?;
    let rows = sqlx::query(
        r#"
        with completed_work as materialized (
            select r.id as reminder_id, max(w.completed_at) as completed_at
            from reminders r
            join agent_inbox_items i
              on i.agent_id = r.creator_agent_id
             and i.kind = 'reminder_due'
             and replace(json_extract(i.payload, '$.reminder_id'), '-', '') = lower(hex(r.id))
            join agent_work_items w on w.id = i.work_item_id
            where r.status = 'fired'
              and r.recurrence = 'none'
              and w.status in ('done', 'silent')
              and ($1 is null or w.id = $1)
            group by r.id
        )
        update reminders
        set status = 'done',
            completed_at = coalesce(
                (select completed_at from completed_work where reminder_id = reminders.id),
                strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
            ),
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id in (select reminder_id from completed_work)
          and status = 'fired'
          and recurrence = 'none'
        returning id
        "#,
    )
    .bind(work_item_id)
    .fetch_all(&mut *transaction)
    .await
    .map_err(to_string)?;

    for row in &rows {
        let reminder_id: Uuid = row.get("id");
        insert_reminder_event(
            &mut *transaction,
            reminder_id,
            "completed",
            "agent follow-up succeeded",
        )
        .await?;
    }
    if !rows.is_empty() {
        enqueue_ui_event_in_tx(
            &mut transaction,
            &UiEvent::Refresh {
                reason: "reminder_completed",
            },
        )
        .await?;
    }
    transaction.commit().await.map_err(to_string)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_due_reminder_to_agent(
    pool: &SqlitePool,
    reminder_id: Uuid,
    agent_id: Uuid,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    source_message_id: Uuid,
    title: &str,
    note: &str,
) -> CommandResult<()> {
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

    let work_thread_root_id = thread_root_id.or(Some(source_message_id));
    let inbox_item_id = create_agent_inbox_item(
        pool,
        AgentInboxItemInput {
            agent_id,
            channel_id: Some(channel_id),
            thread_root_id: work_thread_root_id,
            source_message_id: Some(source_message_id),
            task_id: None,
            kind: "reminder_due",
            priority: 90,
            title,
            body_preview: note,
            payload: json!({"reminder_id": reminder_id}),
        },
    )
    .await?;
    let wake = ensure_agent_inbox_wake_work_item(pool, agent_id).await?;
    let scheduled = wake.is_some_and(|(_, scheduled)| scheduled);
    record_agent_activity(
        pool,
        Some(agent_id),
        None,
        "reminder",
        if scheduled {
            "Reminder follow-up dispatched"
        } else {
            "Reminder follow-up queued"
        },
        json!({
            "reminder_id": reminder_id,
            "inbox_item_id": inbox_item_id,
            "source_message_id": source_message_id
        })
        .to_string(),
    )
    .await?;
    Ok(())
}

pub(crate) const MIN_INTERVAL_SECONDS: i64 = 60;
pub(crate) const MAX_INTERVAL_SECONDS: i64 = 7 * 24 * 60 * 60;
const MAX_CHECK_COMMAND_CHARS: usize = 4_000;

/// Optional loop settings for a reminder: a minute-level cadence and a check
/// command whose output is either posted as the agent or handed to the agent.
#[derive(Debug, Clone, Default)]
pub(crate) struct ReminderLoopOptions {
    pub(crate) interval_seconds: Option<i64>,
    pub(crate) check_command: String,
    pub(crate) check_action: String,
}

struct ValidatedLoop {
    interval_seconds: Option<i64>,
    check_command: String,
    check_action: String,
}

fn validate_loop_options(
    recurrence: &str,
    creator_agent_id: Option<Uuid>,
    channel_id: Option<Uuid>,
    options: ReminderLoopOptions,
) -> CommandResult<ValidatedLoop> {
    let interval_seconds = match (recurrence, options.interval_seconds) {
        ("interval", Some(seconds))
            if (MIN_INTERVAL_SECONDS..=MAX_INTERVAL_SECONDS).contains(&seconds) =>
        {
            Some(seconds)
        }
        ("interval", Some(_)) => {
            return Err("loop interval must be between 1 minute and 7 days".to_owned())
        }
        ("interval", None) => return Err("interval reminders require interval_minutes".to_owned()),
        (_, Some(_)) => return Err("interval_minutes requires recurrence interval".to_owned()),
        (_, None) => None,
    };
    let check_command = options.check_command.trim().to_owned();
    let check_action = options.check_action.trim().to_ascii_lowercase();
    if check_command.is_empty() {
        if !check_action.is_empty() {
            return Err("check_action requires check_command".to_owned());
        }
        return Ok(ValidatedLoop {
            interval_seconds,
            check_command,
            check_action,
        });
    }
    if check_command.chars().count() > MAX_CHECK_COMMAND_CHARS {
        return Err(format!(
            "check_command is longer than {MAX_CHECK_COMMAND_CHARS} characters"
        ));
    }
    if !matches!(check_action.as_str(), "post" | "wake") {
        return Err("check_command requires check_action post or wake".to_owned());
    }
    if recurrence == "none" {
        return Err("check_command requires a repeating reminder".to_owned());
    }
    if creator_agent_id.is_none() {
        return Err("check_command reminders must be created by an agent".to_owned());
    }
    if channel_id.is_none() {
        return Err("check_command reminders require a channel".to_owned());
    }
    Ok(ValidatedLoop {
        interval_seconds,
        check_command,
        check_action,
    })
}

fn normalize_recurrence(value: &str) -> CommandResult<String> {
    let recurrence = value.trim();
    if matches!(recurrence, "none" | "daily" | "weekly" | "interval") {
        Ok(recurrence.to_owned())
    } else if matches!(recurrence, "one_shot" | "one-shot" | "once") {
        Ok("none".to_owned())
    } else {
        Err(format!("unsupported reminder recurrence: {recurrence}"))
    }
}

async fn insert_reminder_event<'e, E>(
    executor: E,
    reminder_id: Uuid,
    event_type: &str,
    detail: &str,
) -> CommandResult<()>
where
    E: Executor<'e, Database = Sqlite>,
{
    sqlx::query(
        r#"
        insert into reminder_events (reminder_id, event_type, detail)
        values ($1, $2, $3)
        "#,
    )
    .bind(reminder_id)
    .bind(event_type)
    .bind(detail)
    .execute(executor)
    .await
    .map_err(to_string)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn create_reminder_in_pool(
    pool: &SqlitePool,
    creator_agent_id: Option<Uuid>,
    channel_id: Option<Uuid>,
    thread_root_id: Option<Uuid>,
    message_id: Option<Uuid>,
    title: &str,
    note: &str,
    due_at: DateTime<Utc>,
    recurrence: &str,
    loop_options: ReminderLoopOptions,
) -> CommandResult<Uuid> {
    let title = title.trim();
    if title.is_empty() {
        return Err("reminder title is empty".to_owned());
    }
    let recurrence = normalize_recurrence(recurrence)?;
    let loop_options =
        validate_loop_options(&recurrence, creator_agent_id, channel_id, loop_options)?;
    if let (Some(agent_id), Some(channel_id), false) = (
        creator_agent_id,
        channel_id,
        loop_options.check_command.is_empty(),
    ) {
        ensure_agent_channel_member(pool, agent_id, channel_id, "reminder_create check_command")
            .await?;
    }

    if let Some(thread_root_id) = thread_root_id {
        let exists: bool = sqlx::query_scalar(
            "select exists(select 1 from messages where id = $1 and thread_root_id is null)",
        )
        .bind(thread_root_id)
        .fetch_one(pool)
        .await
        .map_err(to_string)?;
        if !exists {
            return Err("thread root does not exist".to_owned());
        }
    }

    let mut transaction = pool.begin().await.map_err(to_string)?;
    let reminder_id: Uuid = sqlx::query_scalar(
        r#"
        insert into reminders (
            channel_id, creator_agent_id, thread_root_id, message_id, title, note,
            due_at, recurrence_anchor_at, recurrence, status,
            interval_seconds, check_command, check_action
        )
        values ($1, $2, $3, $4, $5, $6, $7, $7, $8, 'scheduled', $9, $10, $11)
        returning id
        "#,
    )
    .bind(channel_id)
    .bind(creator_agent_id)
    .bind(thread_root_id)
    .bind(message_id)
    .bind(title)
    .bind(note.trim())
    .bind(due_at)
    .bind(&recurrence)
    .bind(loop_options.interval_seconds)
    .bind(&loop_options.check_command)
    .bind(&loop_options.check_action)
    .fetch_one(&mut *transaction)
    .await
    .map_err(to_string)?;
    let created_detail = due_at.to_rfc3339();
    insert_reminder_event(&mut *transaction, reminder_id, "created", &created_detail).await?;
    enqueue_ui_event_in_tx(
        &mut transaction,
        &UiEvent::Refresh {
            reason: "reminder_created",
        },
    )
    .await?;
    transaction.commit().await.map_err(to_string)?;
    Ok(reminder_id)
}

pub(crate) async fn cancel_reminder_in_pool(
    pool: &SqlitePool,
    reminder_id: Uuid,
) -> CommandResult<()> {
    let mut transaction = pool.begin().await.map_err(to_string)?;
    let affected = sqlx::query(
        r#"
        update reminders
        set status = 'cancelled',
            completed_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1 and status in ('scheduled', 'fired')
        "#,
    )
    .bind(reminder_id)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?
    .rows_affected();
    if affected == 0 {
        return Err("reminder does not exist or is not active".to_owned());
    }
    insert_reminder_event(&mut *transaction, reminder_id, "cancelled", "").await?;
    enqueue_ui_event_in_tx(
        &mut transaction,
        &UiEvent::Refresh {
            reason: "reminder_cancelled",
        },
    )
    .await?;
    transaction.commit().await.map_err(to_string)?;
    Ok(())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn create_reminder(
    channel_id: Option<Uuid>,
    thread_root_id: Option<Uuid>,
    message_id: Option<Uuid>,
    title: String,
    note: String,
    due_at: String,
    recurrence: String,
    state: State<'_, AppState>,
) -> CommandResult<Uuid> {
    let due_at = parse_due_at(&due_at)?;
    create_reminder_in_pool(
        &state.pool,
        None,
        channel_id,
        thread_root_id,
        message_id,
        &title,
        &note,
        due_at,
        &recurrence,
        ReminderLoopOptions::default(),
    )
    .await
}

#[tauri::command]
pub(crate) async fn snooze_reminder(
    reminder_id: Uuid,
    minutes: i64,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    if !(1..=10_080).contains(&minutes) {
        return Err("snooze minutes must be between 1 and 10080".to_owned());
    }
    let mut transaction = state.pool.begin().await.map_err(to_string)?;
    sqlx::query(
        r#"
        update reminders
        set status = 'scheduled',
            due_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now','+' || $2 || ' minutes'),
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1 and status in ('scheduled', 'fired')
        "#,
    )
    .bind(reminder_id)
    .bind(minutes)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;
    let snooze_detail = format!("{minutes} minutes");
    insert_reminder_event(&mut *transaction, reminder_id, "snoozed", &snooze_detail).await?;
    enqueue_ui_event_in_tx(
        &mut transaction,
        &UiEvent::Refresh {
            reason: "reminder_snoozed",
        },
    )
    .await?;
    transaction.commit().await.map_err(to_string)?;
    Ok(())
}

#[tauri::command]
pub(crate) async fn complete_reminder(
    reminder_id: Uuid,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    complete_reminder_in_pool(&state.pool, reminder_id).await
}

pub(crate) async fn complete_reminder_in_pool(
    pool: &SqlitePool,
    reminder_id: Uuid,
) -> CommandResult<()> {
    let mut transaction = pool.begin().await.map_err(to_string)?;
    sqlx::query(
        r#"
        update reminders
        set status = 'done',
            completed_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1 and status in ('scheduled', 'fired')
        "#,
    )
    .bind(reminder_id)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;
    insert_reminder_event(&mut *transaction, reminder_id, "completed", "").await?;
    enqueue_ui_event_in_tx(
        &mut transaction,
        &UiEvent::Refresh {
            reason: "reminder_completed",
        },
    )
    .await?;
    transaction.commit().await.map_err(to_string)?;
    Ok(())
}

#[tauri::command]
pub(crate) async fn cancel_reminder(
    reminder_id: Uuid,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    cancel_reminder_in_pool(&state.pool, reminder_id).await
}

pub(crate) async fn load_reminders(pool: &SqlitePool) -> CommandResult<Vec<Reminder>> {
    let rows = sqlx::query(
        r#"
        select
            r.id,
            r.channel_id,
            c.name as channel_name,
            r.creator_agent_id,
            a.handle as creator_agent_handle,
            r.thread_root_id,
            r.message_id,
            r.title,
            r.note,
            r.status,
            r.recurrence,
            r.interval_seconds,
            r.check_command,
            r.check_action,
            r.last_checked_at,
            r.last_check_status,
            r.last_check_detail,
            r.check_failures,
            r.due_at,
            r.fired_at,
            r.completed_at,
            r.created_at,
            r.updated_at
        from reminders r
        left join channels c on c.id = r.channel_id
        left join agents a on a.id = r.creator_agent_id
        where r.status in ('scheduled', 'fired')
        order by
            case r.status when 'fired' then 0 else 1 end,
            r.due_at asc
        limit 100
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    Ok(rows
        .into_iter()
        .map(|row| Reminder {
            id: row.get("id"),
            channel_id: row.get("channel_id"),
            channel_name: row.get("channel_name"),
            creator_agent_id: row.get("creator_agent_id"),
            creator_agent_handle: row.get("creator_agent_handle"),
            thread_root_id: row.get("thread_root_id"),
            message_id: row.get("message_id"),
            title: row.get("title"),
            note: row.get("note"),
            status: row.get("status"),
            recurrence: row.get("recurrence"),
            interval_seconds: row.get("interval_seconds"),
            check_command: row.get("check_command"),
            check_action: row.get("check_action"),
            last_checked_at: row.get("last_checked_at"),
            last_check_status: row.get("last_check_status"),
            last_check_detail: row.get("last_check_detail"),
            check_failures: row.get("check_failures"),
            due_at: row.get("due_at"),
            fired_at: row.get("fired_at"),
            completed_at: row.get("completed_at"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use std::fs as std_fs;

    use serde_json::json;
    use sqlx::SqlitePool;
    use uuid::Uuid;

    use super::{complete_successful_agent_reminders, process_due_reminders};
    use crate::agent_inbox_wake::sync_inbox_for_work_item;
    use crate::db::{db_connect_with_url, migrate};
    use crate::test_support::insert_test_agent;

    #[tokio::test]
    async fn successful_agent_follow_up_completes_only_its_one_shot_reminders() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "reminder-agent").await?;
            let mut cases = Vec::new();
            for (title, work_status, recurrence) in [
                ("finished", "done", "none"),
                ("quietly finished", "silent", "none"),
                ("failed", "failed", "none"),
                ("recurring", "done", "daily"),
            ] {
                let reminder_id: Uuid = sqlx::query_scalar(
                    r#"
                    insert into reminders
                        (creator_agent_id, title, due_at, fired_at, recurrence, status)
                    values ($1, $2, '2026-01-01T00:00:00+00:00',
                            '2026-01-01T00:00:01+00:00', $3, 'fired')
                    returning id
                    "#,
                )
                .bind(agent_id)
                .bind(title)
                .bind(recurrence)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
                let work_item_id: Uuid = sqlx::query_scalar(
                    r#"
                    insert into agent_work_items (agent_id, title, status, completed_at)
                    values ($1, $2, $3, '2026-01-01T00:00:02+00:00')
                    returning id
                    "#,
                )
                .bind(agent_id)
                .bind(title)
                .bind(work_status)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
                sqlx::query(
                    r#"
                    insert into agent_inbox_items
                        (agent_id, kind, title, payload, work_item_id)
                    values ($1, 'reminder_due', $2, $3, $4)
                    "#,
                )
                .bind(agent_id)
                .bind(title)
                .bind(json!({"reminder_id": reminder_id}))
                .bind(work_item_id)
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
                cases.push((reminder_id, work_item_id));
            }

            sync_inbox_for_work_item(&pool, cases[0].1).await?;
            sync_inbox_for_work_item(&pool, cases[2].1).await?;
            let status: String = sqlx::query_scalar("select status from reminders where id = $1")
                .bind(cases[0].0)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
            assert_eq!(status, "done");
            let failed_status: String =
                sqlx::query_scalar("select status from reminders where id = $1")
                    .bind(cases[2].0)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(failed_status, "fired");

            complete_successful_agent_reminders(&pool, None).await?;
            complete_successful_agent_reminders(&pool, None).await?;
            for (index, (reminder_id, _)) in cases.iter().enumerate() {
                let (status, completed_at): (String, Option<String>) = sqlx::query_as(
                    "select status, completed_at from reminders where id = $1",
                )
                .bind(reminder_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
                if index < 2 {
                    assert_eq!(status, "done");
                    assert_eq!(completed_at.as_deref(), Some("2026-01-01T00:00:02+00:00"));
                    let count: i64 = sqlx::query_scalar(
                        "select count(*) from reminder_events where reminder_id = $1 and event_type = 'completed'",
                    )
                    .bind(reminder_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
                    assert_eq!(count, 1);
                } else {
                    assert_eq!(status, "fired");
                    assert!(completed_at.is_none());
                }
            }
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn due_reminder_fires_system_message() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let channel_id = insert_test_channel(&pool, "reminders").await?;
            let reminder_id: Uuid = sqlx::query_scalar(
                r#"
                insert into reminders (channel_id, title, note, due_at, recurrence, status)
                values ($1, 'Check thread', 'Follow up with Dylan', strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-1 minute'), 'none', 'scheduled')
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            process_due_reminders(&pool).await?;

            let status: String = sqlx::query_scalar("select status from reminders where id = $1")
                .bind(reminder_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
            assert_eq!(status, "fired");
            let system_messages: i64 = sqlx::query_scalar(
                r#"
                select count(*)
                from messages
                where channel_id = $1
                  and sender_role = 'system'
                  and body like 'Reminder:%'
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(system_messages, 1);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn recurring_reminder_next_due_stays_anchored_to_original_time() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let channel_id = insert_test_channel(&pool, "reminder-anchor").await?;
            // Due 3 days ago at a fixed wall-clock time: late processing must
            // keep the original time-of-day and land strictly in the future.
            let reminder_id: Uuid = sqlx::query_scalar(
                r#"
                insert into reminders (
                    channel_id, title, note, due_at, recurrence_anchor_at, recurrence, status
                )
                values (
                    $1,
                    'Daily memory pass',
                    '',
                    strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-1 minute'),
                    strftime('%Y-%m-%dT03:00:00.000+00:00','now','-3 days'),
                    'daily',
                    'scheduled'
                )
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            process_due_reminders(&pool).await?;

            let (status, due_at): (String, String) =
                sqlx::query_as("select status, due_at from reminders where id = $1")
                    .bind(reminder_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(status, "scheduled");
            assert!(
                due_at.contains("T03:00:00"),
                "next due should keep original time-of-day, got {due_at}"
            );
            let now: String =
                sqlx::query_scalar("select strftime('%Y-%m-%dT%H:%M:%f+00:00','now')")
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert!(due_at > now, "next due {due_at} should be after now {now}");
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
