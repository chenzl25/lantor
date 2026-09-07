use serde_json::json;
use sqlx::{Row, SqlitePool};
use tauri::State;
use uuid::Uuid;

use crate::agent_inbox_wake::{
    attach_work_item_to_inbox_in_tx, create_agent_inbox_item, enqueue_agent_work_if_available,
    ensure_agent_inbox_wake_work_item, prepend_inbox_context, AgentInboxItemInput,
};
use crate::agent_routing::{resolve_agent_handle, upsert_agent_thread_subscription};
use crate::app::{to_string, AppState, CommandResult};
use crate::channels::add_agent_to_channel;
use crate::events::activity::record_agent_activity;
use crate::ui_notifications::{
    enqueue_ui_event_in_tx, enqueue_ui_work_item_changed_in_tx, notify_supervisor_wake,
    reconcile_work_item_change, UiEvent,
};

#[tauri::command]
pub(crate) async fn dispatch_agent_work(
    agent_id: Uuid,
    channel_id: Option<Uuid>,
    thread_root_id: Option<Uuid>,
    task_id: Option<Uuid>,
    title: String,
    context: String,
    state: State<'_, AppState>,
) -> CommandResult<Uuid> {
    let agent_handle: Option<String> =
        sqlx::query_scalar("select handle from agents where id = $1")
            .bind(agent_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(to_string)?;
    let Some(agent_handle) = agent_handle else {
        return Err("agent does not exist".to_owned());
    };
    let mut resolved_channel_id = channel_id;
    let mut resolved_thread_root_id = thread_root_id;
    let mut resolved_title = title.trim().to_owned();

    if let Some(task_id) = task_id {
        let row = sqlx::query(
            r#"
            select channel_id, message_id, title
            from tasks
            where id = $1
            "#,
        )
        .bind(task_id)
        .fetch_one(&state.pool)
        .await
        .map_err(to_string)?;
        resolved_channel_id = Some(row.get("channel_id"));
        resolved_thread_root_id = Some(row.get("message_id"));
        if resolved_title.is_empty() {
            resolved_title = row.get("title");
        }
    }

    if resolved_channel_id.is_none() {
        if let Some(thread_root_id) = resolved_thread_root_id {
            resolved_channel_id =
                sqlx::query_scalar("select channel_id from messages where id = $1")
                    .bind(thread_root_id)
                    .fetch_optional(&state.pool)
                    .await
                    .map_err(to_string)?;
        }
    }

    if resolved_title.is_empty() {
        resolved_title = match resolved_thread_root_id {
            Some(thread_root_id) => {
                let body: Option<String> =
                    sqlx::query_scalar("select body from messages where id = $1")
                        .bind(thread_root_id)
                        .fetch_optional(&state.pool)
                        .await
                        .map_err(to_string)?;
                body.and_then(|body| {
                    body.lines()
                        .next()
                        .map(|line| line.chars().take(120).collect())
                })
                .filter(|line: &String| !line.trim().is_empty())
                .unwrap_or_else(|| "Lantor agent request".to_owned())
            }
            None => "Lantor agent request".to_owned(),
        };
    }

    let source_kind = if task_id.is_some() { "task" } else { "manual" };
    let inbox_kind = if task_id.is_some() {
        "task_assigned"
    } else {
        "manual"
    };
    let inbox_item_id = create_agent_inbox_item(
        &state.pool,
        AgentInboxItemInput {
            agent_id,
            channel_id: resolved_channel_id,
            thread_root_id: resolved_thread_root_id,
            source_message_id: resolved_thread_root_id,
            task_id,
            kind: inbox_kind,
            priority: if task_id.is_some() { 90 } else { 60 },
            title: &resolved_title,
            body_preview: context.trim(),
            payload: json!({"source_kind": source_kind, "explicit_dispatch": true}),
        },
    )
    .await?;
    let work_context = prepend_inbox_context(inbox_item_id, inbox_kind, context.trim());
    let mut transaction = state.pool.begin().await.map_err(to_string)?;
    let work_item_id: Uuid = sqlx::query_scalar(
        r#"
        insert into agent_work_items (
            agent_id, channel_id, thread_root_id, inbox_item_id, task_id, source_kind, title, context, status
        )
        values ($1, $2, $3, $4, $5, $6, $7, $8, 'queued')
        returning id
        "#,
    )
    .bind(agent_id)
    .bind(resolved_channel_id)
    .bind(resolved_thread_root_id)
    .bind(inbox_item_id)
    .bind(task_id)
    .bind(source_kind)
    .bind(&resolved_title)
    .bind(&work_context)
    .fetch_one(&mut *transaction)
    .await
    .map_err(to_string)?;
    attach_work_item_to_inbox_in_tx(&mut transaction, inbox_item_id, work_item_id).await?;

    if let Some(task_id) = task_id {
        sqlx::query(
            r#"
            update tasks
            set assignee_agent_id = $2,
                status = 'in_progress',
                version = version + 1,
                updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
            where id = $1
            "#,
        )
        .bind(task_id)
        .bind(agent_id)
        .execute(&mut *transaction)
        .await
        .map_err(to_string)?;
        enqueue_ui_event_in_tx(
            &mut transaction,
            &UiEvent::Refresh {
                reason: "task_claimed",
            },
        )
        .await?;
    }
    enqueue_ui_work_item_changed_in_tx(&mut transaction, work_item_id, "work_item_created").await?;
    transaction.commit().await.map_err(to_string)?;
    reconcile_work_item_change(&state.pool, work_item_id, "work_item_created").await?;

    if let (Some(channel_id), Some(thread_root_id)) = (resolved_channel_id, resolved_thread_root_id)
    {
        upsert_agent_thread_subscription(
            &state.pool,
            agent_id,
            channel_id,
            thread_root_id,
            source_kind,
            None,
        )
        .await?;
    }

    let scheduled = enqueue_agent_work_if_available(&state.pool, agent_id, work_item_id).await?;
    record_agent_activity(
        &state.pool,
        Some(agent_id),
        None,
        "dispatch",
        if scheduled {
            "Agent request dispatched"
        } else {
            "Agent request queued"
        },
        format!("#{work_item_id} to @{agent_handle}: {resolved_title}"),
    )
    .await?;

    Ok(work_item_id)
}

pub(crate) async fn dispatch_task_assignment_to_agent(
    pool: &SqlitePool,
    task_id: Uuid,
    agent_id: Uuid,
    reason: &str,
) -> CommandResult<()> {
    let row = sqlx::query(
        r#"
        select t.channel_id, t.message_id, t.title, m.body
        from tasks t
        join messages m on m.id = t.message_id
        where t.id = $1
        "#,
    )
    .bind(task_id)
    .fetch_one(pool)
    .await
    .map_err(to_string)?;
    let channel_id: Uuid = row.get("channel_id");
    let message_id: Uuid = row.get("message_id");
    let title: String = row.get("title");
    let body: String = row.get("body");
    dispatch_task_context_to_agent(
        pool, task_id, agent_id, channel_id, message_id, message_id, &title, &body, reason,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_task_context_to_agent(
    pool: &SqlitePool,
    task_id: Uuid,
    agent_id: Uuid,
    channel_id: Uuid,
    thread_root_id: Uuid,
    source_message_id: Uuid,
    title: &str,
    body: &str,
    reason: &str,
) -> CommandResult<()> {
    add_agent_to_channel(pool, channel_id, agent_id).await?;

    let inbox_item_id = create_agent_inbox_item(
        pool,
        AgentInboxItemInput {
            agent_id,
            channel_id: Some(channel_id),
            thread_root_id: Some(thread_root_id),
            source_message_id: Some(source_message_id),
            task_id: Some(task_id),
            kind: "task_assigned",
            priority: 95,
            title,
            body_preview: body.trim(),
            payload: json!({"source_kind": "task", "reason": reason}),
        },
    )
    .await?;
    upsert_agent_thread_subscription(
        pool,
        agent_id,
        channel_id,
        thread_root_id,
        "task",
        Some(source_message_id),
    )
    .await?;
    let scheduled = ensure_agent_inbox_wake_work_item(pool, agent_id)
        .await?
        .is_some_and(|(_, scheduled)| scheduled);
    let agent_handle = resolve_agent_handle(pool, agent_id)
        .await
        .unwrap_or_else(|_| "unknown".to_owned());
    record_agent_activity(
        pool,
        Some(agent_id),
        None,
        "task",
        if scheduled {
            "Task assignment delivered to inbox"
        } else {
            "Task assignment queued in inbox"
        },
        json!({
            "target_agent": format!("@{agent_handle}"),
            "inbox_item_id": inbox_item_id,
            "task_id": task_id,
            "title": title,
            "source_message_id": source_message_id,
        })
        .to_string(),
    )
    .await?;
    Ok(())
}

pub(crate) async fn dispatch_task_followup_to_agent(
    pool: &SqlitePool,
    task_id: Uuid,
    agent_id: Uuid,
    source_message_id: Uuid,
    title: &str,
    body: &str,
    reason: &str,
) -> CommandResult<()> {
    let row = sqlx::query(
        r#"
        select t.channel_id, t.message_id
        from tasks t
        join messages source
          on source.id = $2
         and source.channel_id = t.channel_id
         and source.thread_root_id = t.message_id
        where t.id = $1
        "#,
    )
    .bind(task_id)
    .bind(source_message_id)
    .fetch_one(pool)
    .await
    .map_err(to_string)?;
    dispatch_task_context_to_agent(
        pool,
        task_id,
        agent_id,
        row.get("channel_id"),
        row.get("message_id"),
        source_message_id,
        title,
        body,
        reason,
    )
    .await
}

pub(crate) async fn dispatch_agent_restart_backlog(
    pool: &SqlitePool,
    agent_id: Uuid,
) -> CommandResult<(usize, bool)> {
    let rows = sqlx::query(
        r#"
        select id
        from tasks t
        where t.assignee_agent_id = $1
          and t.status in ('todo', 'in_progress')
          and not exists (
              select 1
              from agent_work_items w
              where w.agent_id = $1
                and w.task_id = t.id
                and w.status in ('queued', 'running', 'cancelling')
          )
        order by t.updated_at asc, t.created_at asc
        "#,
    )
    .bind(agent_id)
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    let mut task_count = 0;
    for row in rows {
        let task_id: Uuid = row.get("id");
        dispatch_task_assignment_to_agent(pool, task_id, agent_id, "agent_restart").await?;
        task_count += 1;
    }

    let inbox_wake = ensure_agent_inbox_wake_work_item(pool, agent_id)
        .await?
        .is_some();
    Ok((task_count, inbox_wake))
}

pub(crate) async fn dispatch_unassigned_task_availability(
    pool: &SqlitePool,
    task_id: Uuid,
) -> CommandResult<()> {
    let Some(row) = sqlx::query(
        r#"
        select t.channel_id, t.message_id, t.title, t.status, t.assignee_agent_id, m.body, c.name as channel_name
        from tasks t
        join messages m on m.id = t.message_id
        join channels c on c.id = t.channel_id
        where t.id = $1
        "#,
    )
    .bind(task_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?
    else {
        return Ok(());
    };
    let status: String = row.get("status");
    let assignee_agent_id: Option<Uuid> = row.get("assignee_agent_id");
    if status != "todo" || assignee_agent_id.is_some() {
        return Ok(());
    }

    let channel_id: Uuid = row.get("channel_id");
    let message_id: Uuid = row.get("message_id");
    let title: String = row.get("title");
    let body: String = row.get("body");
    let channel_name: String = row.get("channel_name");
    let agents = sqlx::query(
        r#"
        select a.id, a.handle
        from channel_members cm
        join agents a on a.id = cm.agent_id
        where cm.channel_id = $1
        order by cm.created_at, a.handle
        "#,
    )
    .bind(channel_id)
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    for agent in agents {
        let agent_id: Uuid = agent.get("id");
        let agent_handle: String = agent.get("handle");
        let inbox_item_id = create_agent_inbox_item(
            pool,
            AgentInboxItemInput {
                agent_id,
                channel_id: Some(channel_id),
                thread_root_id: Some(message_id),
                source_message_id: Some(message_id),
                task_id: Some(task_id),
                kind: "task_available",
                priority: 70,
                title: &title,
                body_preview: body.trim(),
                payload: json!({
                    "channel_name": &channel_name,
                    "source_kind": "task_available",
                    "claim_contract": "Emit LANTOR_EVENT task_claim only if you can start this task now. The backend will atomically accept one claimant and ignore stale claims.",
                }),
            },
        )
        .await?;
        let scheduled = ensure_agent_inbox_wake_work_item(pool, agent_id)
            .await?
            .is_some_and(|(_, scheduled)| scheduled);
        record_agent_activity(
            pool,
            Some(agent_id),
            None,
            "task",
            if scheduled {
                "Task claim opportunity delivered"
            } else {
                "Task claim opportunity queued"
            },
            json!({
                "target_agent": format!("@{agent_handle}"),
                "inbox_item_id": inbox_item_id,
                "task_id": task_id,
                "title": &title,
            })
            .to_string(),
        )
        .await?;
    }

    Ok(())
}

pub(crate) async fn try_claim_unassigned_task(
    pool: &SqlitePool,
    task_id: Uuid,
    agent_id: Uuid,
    expected_version: Option<i64>,
    reason: &str,
) -> CommandResult<Option<i64>> {
    let mut transaction = pool.begin().await.map_err(to_string)?;
    let claimed = sqlx::query(
        r#"
        update tasks as t
        set assignee_agent_id = $2,
            status = 'in_progress',
            version = t.version + 1,
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where t.id = $1
          and t.assignee_agent_id is null
          and t.status = 'todo'
          and ($3 is null or t.version = $3)
          and exists (
              select 1
              from channel_members cm
              join agents a on a.id = cm.agent_id
              where cm.channel_id = t.channel_id
                and cm.agent_id = $2
          )
          and not exists (
              select 1
              from tasks active
              where active.assignee_agent_id = $2
                and active.status in ('todo', 'in_progress')
                and active.id <> t.id
          )
        returning number
        "#,
    )
    .bind(task_id)
    .bind(agent_id)
    .bind(expected_version)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(to_string)?;

    let Some(row) = claimed else {
        return Ok(None);
    };
    let task_number: i64 = row.get("number");
    sqlx::query(
        r#"
        update agent_inbox_items
        set state = 'archived',
            archived_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where task_id = $1
          and kind = 'task_available'
          and state <> 'archived'
        "#,
    )
    .bind(task_id)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;
    enqueue_ui_event_in_tx(
        &mut transaction,
        &UiEvent::Refresh {
            reason: "task_claimed",
        },
    )
    .await?;
    transaction.commit().await.map_err(to_string)?;
    dispatch_task_assignment_to_agent(pool, task_id, agent_id, reason).await?;
    record_agent_activity(
        pool,
        Some(agent_id),
        None,
        "task",
        format!("Task #{task_number} claimed"),
        json!({
            "task_id": task_id,
            "reason": reason,
            "optimistic": expected_version.is_some(),
        })
        .to_string(),
    )
    .await?;
    Ok(Some(task_number))
}

#[tauri::command]
pub(crate) async fn claim_task(
    task_id: Uuid,
    agent_id: Option<Uuid>,
    expected_version: Option<i64>,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    claim_task_in_pool(&state.pool, task_id, agent_id, expected_version).await
}

pub(crate) async fn claim_task_in_pool(
    pool: &SqlitePool,
    task_id: Uuid,
    agent_id: Option<Uuid>,
    expected_version: Option<i64>,
) -> CommandResult<()> {
    let current_status: Option<String> =
        sqlx::query_scalar("select status from tasks where id = $1")
            .bind(task_id)
            .fetch_optional(pool)
            .await
            .map_err(to_string)?;
    let current_status = current_status.ok_or_else(|| "task does not exist".to_owned())?;
    if current_status == "done" {
        return Err("done tasks cannot be reassigned".to_owned());
    }

    if let (Some(agent_id), Some(expected_version)) = (agent_id, expected_version) {
        return try_claim_unassigned_task(
            pool,
            task_id,
            agent_id,
            Some(expected_version),
            "manual_claim",
        )
        .await?
        .map(|_| ())
        .ok_or_else(|| "task was already claimed or is no longer available".to_owned());
    }

    let mut transaction = pool.begin().await.map_err(to_string)?;
    sqlx::query_scalar::<_, Uuid>(
        r#"
        update tasks
        set assignee_agent_id = $2,
            status = case when $2 is null then status else 'in_progress' end,
            version = version + 1,
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1
        returning id
        "#,
    )
    .bind(task_id)
    .bind(agent_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(to_string)?
    .ok_or_else(|| "task does not exist".to_owned())?;
    enqueue_ui_event_in_tx(
        &mut transaction,
        &UiEvent::Refresh {
            reason: "task_claimed",
        },
    )
    .await?;
    transaction.commit().await.map_err(to_string)?;

    if let Some(agent_id) = agent_id {
        dispatch_task_assignment_to_agent(pool, task_id, agent_id, "manual_claim").await?;
    } else {
        record_agent_activity(
            pool,
            None,
            None,
            "task",
            "Task unassigned",
            json!({ "task_id": task_id }).to_string(),
        )
        .await?;
    }

    Ok(())
}

pub(crate) async fn mark_task_after_work_item_finished(
    pool: &SqlitePool,
    work_item_id: Uuid,
    agent_id: Uuid,
    run_id: Uuid,
    work_status: &str,
) -> CommandResult<()> {
    let task_row = sqlx::query(
        r#"
        select t.id, t.number, t.title, t.status
        from agent_work_items w
        join tasks t on t.id = w.task_id
        where w.id = $1
        "#,
    )
    .bind(work_item_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;
    let Some(task_row) = task_row else {
        return Ok(());
    };
    let task_id: Uuid = task_row.get("id");
    let task_number: i64 = task_row.get("number");
    let title: String = task_row.get("title");
    let current_status: String = task_row.get("status");

    if work_status == "done" && matches!(current_status.as_str(), "todo" | "in_progress") {
        let mut transaction = pool.begin().await.map_err(to_string)?;
        sqlx::query(
            r#"
            update tasks
            set status = 'in_review',
                updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
            where id = $1 and status in ('todo', 'in_progress')
            "#,
        )
        .bind(task_id)
        .execute(&mut *transaction)
        .await
        .map_err(to_string)?;
        enqueue_ui_event_in_tx(
            &mut transaction,
            &UiEvent::Refresh {
                reason: "task_ready_for_review",
            },
        )
        .await?;
        transaction.commit().await.map_err(to_string)?;
        record_agent_activity(
            pool,
            Some(agent_id),
            Some(run_id),
            "task",
            "Task ready for review",
            json!({
                "task_id": task_id,
                "task_number": task_number,
                "work_item_id": work_item_id,
                "title": title,
            })
            .to_string(),
        )
        .await?;
    } else if matches!(work_status, "failed" | "cancelled") {
        record_agent_activity(
            pool,
            Some(agent_id),
            Some(run_id),
            "task",
            if work_status == "failed" {
                "Task run failed"
            } else {
                "Task run cancelled"
            },
            json!({
                "task_id": task_id,
                "task_number": task_number,
                "work_item_id": work_item_id,
                "title": title,
            })
            .to_string(),
        )
        .await?;
    }

    Ok(())
}

#[tauri::command]
pub(crate) async fn cancel_agent_work(
    work_item_id: Uuid,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    cancel_agent_work_in_pool(&state.pool, work_item_id).await
}

pub(crate) async fn cancel_agent_work_in_pool(
    pool: &SqlitePool,
    work_item_id: Uuid,
) -> CommandResult<()> {
    // Serialize the status check with dispatch/completion. A queued request can
    // start while the user clicks Stop; its stop command must commit with the
    // cancelling state so a lost response cannot strand it.
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(to_string)?;
    let row = sqlx::query("select agent_id, run_id, status from agent_work_items where id = $1")
        .bind(work_item_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(to_string)?;
    let agent_id: Uuid = row.get("agent_id");
    let run_id: Option<Uuid> = row.get("run_id");
    let status: String = row.get("status");
    let next_status = match status.as_str() {
        "queued" => "cancelled",
        "cancelling" if run_id.is_none() => "cancelled",
        "running" | "cancelling" => {
            let run_id =
                run_id.ok_or_else(|| "running agent request does not have a run id".to_owned())?;
            let run_live: bool = sqlx::query_scalar(
                "select exists(select 1 from agent_runs where id = $1 and stopped_at is null)",
            )
            .bind(run_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(to_string)?;
            if run_live {
                sqlx::query(
                    r#"
                    insert into supervisor_commands (command_type, agent_id, run_id, work_item_id)
                    select 'stop_run', $1, $2, $3
                    where not exists (
                        select 1 from supervisor_commands
                        where command_type = 'stop_run' and run_id = $2
                          and status in ('pending', 'running')
                    )
                    "#,
                )
                .bind(agent_id)
                .bind(run_id)
                .bind(work_item_id)
                .execute(&mut *transaction)
                .await
                .map_err(to_string)?;
                "cancelling"
            } else {
                "cancelled"
            }
        }
        // A second click or a response lost after completion is harmless.
        "cancelled" | "done" | "failed" | "silent" | "held" => return Ok(()),
        other => return Err(format!("cannot cancel agent request with status {other}")),
    };
    sqlx::query(
        r#"
        update agent_work_items
        set status = $2,
            completed_at = case when $2 = 'cancelled' then strftime('%Y-%m-%dT%H:%M:%f+00:00','now') else completed_at end,
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1
        "#,
    )
    .bind(work_item_id)
    .bind(next_status)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;
    if next_status == "cancelled" {
        sqlx::query(
            "update supervisor_commands set status = 'done', error = 'cancelled', updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') where work_item_id = $1 and status = 'pending'",
        )
        .bind(work_item_id)
        .execute(&mut *transaction)
        .await
        .map_err(to_string)?;
    }
    let reason = if next_status == "cancelling" {
        "work_item_cancelling"
    } else {
        "work_item_cancelled"
    };
    enqueue_ui_work_item_changed_in_tx(&mut transaction, work_item_id, reason).await?;
    transaction.commit().await.map_err(to_string)?;
    let _ = notify_supervisor_wake(pool).await;
    reconcile_work_item_change(pool, work_item_id, reason).await?;
    record_agent_activity(
        pool,
        Some(agent_id),
        run_id,
        "dispatch",
        "Agent request cancel requested",
        work_item_id.to_string(),
    )
    .await?;
    Ok(())
}

#[tauri::command]
pub(crate) async fn retry_agent_work(
    work_item_id: Uuid,
    state: State<'_, AppState>,
) -> CommandResult<Uuid> {
    retry_agent_work_in_pool(&state.pool, work_item_id).await
}

pub(crate) async fn retry_agent_work_in_pool(
    pool: &SqlitePool,
    work_item_id: Uuid,
) -> CommandResult<Uuid> {
    // The source work item is the idempotency key, including across clients and
    // lost responses. Retrying a failed attempt again uses the new attempt ID.
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(to_string)?;
    let row = sqlx::query(
        r#"
        select agent_id, channel_id, thread_root_id, source_message_id, inbox_item_id, task_id,
               source_kind, title, context, context_max_seq, freshness_generation, status, retry_work_item_id
        from agent_work_items
        where id = $1
        "#,
    )
    .bind(work_item_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(to_string)?;
    if let Some(existing) = row.get::<Option<Uuid>, _>("retry_work_item_id") {
        transaction.commit().await.map_err(to_string)?;
        return Ok(existing);
    }
    let old_status: String = row.get("status");
    if matches!(old_status.as_str(), "queued" | "running" | "cancelling") {
        return Err(format!(
            "cannot retry agent request with status {old_status}"
        ));
    }

    let agent_id: Uuid = row.get("agent_id");
    let title: String = row.get("title");
    let context: String = row.get("context");
    let new_work_item_id: Uuid = sqlx::query_scalar(
        r#"
        insert into agent_work_items (
            agent_id, channel_id, thread_root_id, source_message_id, inbox_item_id, task_id,
            source_kind, title, context, context_max_seq, freshness_generation, status
        )
        values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 'queued')
        returning id
        "#,
    )
    .bind(agent_id)
    .bind(row.get::<Option<Uuid>, _>("channel_id"))
    .bind(row.get::<Option<Uuid>, _>("thread_root_id"))
    .bind(row.get::<Option<Uuid>, _>("source_message_id"))
    .bind(row.get::<Option<Uuid>, _>("inbox_item_id"))
    .bind(row.get::<Option<Uuid>, _>("task_id"))
    .bind(row.get::<String, _>("source_kind"))
    .bind(&title)
    .bind(&context)
    .bind(row.get::<i64, _>("context_max_seq"))
    .bind(row.get::<i64, _>("freshness_generation"))
    .fetch_one(&mut *transaction)
    .await
    .map_err(to_string)?;
    sqlx::query("update agent_work_items set retry_work_item_id = $2 where id = $1")
        .bind(work_item_id)
        .bind(new_work_item_id)
        .execute(&mut *transaction)
        .await
        .map_err(to_string)?;
    // Explicit retry bypasses the automatic launch cooldown for an agent left
    // in error after its previous run.
    sqlx::query("update agents set status = 'idle' where id = $1 and status = 'error'")
        .bind(agent_id)
        .execute(&mut *transaction)
        .await
        .map_err(to_string)?;
    enqueue_ui_work_item_changed_in_tx(&mut transaction, work_item_id, "work_item_retried").await?;
    if let Some(inbox_item_id) = row.get::<Option<Uuid>, _>("inbox_item_id") {
        attach_work_item_to_inbox_in_tx(&mut transaction, inbox_item_id, new_work_item_id).await?;
    }
    enqueue_ui_work_item_changed_in_tx(&mut transaction, new_work_item_id, "work_item_created")
        .await?;
    transaction.commit().await.map_err(to_string)?;
    reconcile_work_item_change(pool, new_work_item_id, "work_item_created").await?;

    let scheduled = enqueue_agent_work_if_available(pool, agent_id, new_work_item_id).await?;
    record_agent_activity(
        pool,
        Some(agent_id),
        None,
        "dispatch",
        if scheduled {
            "Agent request retried"
        } else {
            "Retried agent request queued"
        },
        format!("{work_item_id} -> {new_work_item_id}: {title}"),
    )
    .await?;

    Ok(new_work_item_id)
}

#[cfg(test)]
#[path = "tests/agent_work_dispatch.rs"]
mod relocated_tests;
