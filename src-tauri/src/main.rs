#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod activity_store;
mod agent_inbox_wake;
mod agent_memory;
mod agent_profile;
mod agent_routing;
mod agent_work_dispatch;
mod agent_workspace;
mod app;
mod artifact_store;
mod attachments;
mod bootstrap;
mod channels;
mod commands;
mod context_tool;
mod db;
mod domain;
mod events;
mod launch_agent;
mod lifecycle_commands;
mod message_store;
mod models;
mod owner_inbox;
mod prompts;
mod publish_guard;
mod runtime;
mod system_commands;
mod task_messages;
mod task_store;
#[cfg(test)]
mod test_support;
mod text;
mod ui_notifications;
mod usage;
mod web;

use std::env;

use sqlx::{Row, SqlitePool};
use tauri::Manager;
use uuid::Uuid;

use agent_inbox_wake::{
    build_steer_followup_prompt, create_agent_inbox_item, ensure_agent_inbox_wake_work_item,
    load_inbox_wake_items_for_work_item, AgentInboxItemInput,
};
use agent_work_dispatch::{
    cancel_agent_work, cancel_agent_work_in_pool, claim_task, claim_task_in_pool,
    dispatch_agent_work, dispatch_task_assignment_to_agent, mark_task_after_work_item_finished,
    retry_agent_work, retry_agent_work_in_pool, try_claim_unassigned_task,
};
use agent_workspace::{agent_workspace_list, agent_workspace_read_file};
use app::{to_string, AppState, CommandResult};
use channels::normalize_channel_name;
use commands::{
    agents::{create_agent, delete_agent, update_agent, update_owner_profile},
    artifacts::artifact_read,
    bootstrap::bootstrap,
    channels::{
        create_channel, delete_channel, open_dm_with_agent, set_channel_agent_membership,
        update_channel,
    },
    inbox::{
        dismiss_inbox_items, mark_all_inbox_read, mark_channel_read, mark_inbox_items_read,
        update_thread_followed,
    },
    messages::{delete_message, send_message, set_message_saved, update_message},
    tasks::{update_task_status, update_task_title},
};
use context_tool::run_agent_context_tool;
use db::{db_connect_with_url, db_url, migrate};
use domain::{
    reminders::{cancel_reminder, complete_reminder, create_reminder, snooze_reminder},
    schedules::{create_agent_schedule, update_agent_schedule_status},
    spawn_reminder_worker,
};
use lifecycle_commands::{
    install_supervisor_service, start_agent, stop_agent, uninstall_supervisor_service,
};
use runtime::supervisor::run_supervisor;
use system_commands::{check_runtime, open_external_url};
use ui_notifications::spawn_ui_refresh_listener;

async fn resolve_run_reminder_anchor(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
) -> CommandResult<(Option<Uuid>, Option<Uuid>, Option<Uuid>)> {
    let row = sqlx::query(
        r#"
        select w.channel_id, w.thread_root_id, w.source_message_id
        from agent_runs r
        left join agent_work_items w on w.id = r.work_item_id
        where r.id = $1 and r.agent_id = $2
        "#,
    )
    .bind(run_id)
    .bind(agent_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;
    Ok(row
        .map(|row| {
            (
                row.get("channel_id"),
                row.get("thread_root_id"),
                row.get("source_message_id"),
            )
        })
        .unwrap_or((None, None, None)))
}

async fn resolve_event_channel(
    pool: &SqlitePool,
    channel_id: Option<Uuid>,
    channel_name: Option<&str>,
) -> CommandResult<Uuid> {
    if let Some(channel_id) = channel_id {
        let resolved: Option<Uuid> = sqlx::query_scalar(
            r#"
            select id
            from channels
            where id = $1 or (kind = 'dm' and dm_agent_id = $1)
            order by case when id = $1 then 0 else 1 end
            limit 1
            "#,
        )
        .bind(channel_id)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?;
        return resolved.ok_or_else(|| format!("channel {channel_id} does not exist"));
    }

    let Some(name) = channel_name else {
        return Err("message event requires channel or channel_id".to_owned());
    };
    let normalized = normalize_channel_name(name);
    if normalized.is_empty() {
        return Err("message event channel is empty".to_owned());
    }
    sqlx::query_scalar("select id from channels where name = $1")
        .bind(&normalized)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?
        .ok_or_else(|| format!("channel #{normalized} does not exist"))
}

pub fn run() {
    let database_url = db_url();
    let pool = tauri::async_runtime::block_on(db_connect_with_url(&database_url, 5))
        .expect("failed to connect Lantor SQLite database");

    tauri::async_runtime::block_on(migrate(&pool)).expect("failed to initialize Lantor schema");
    launch_agent::spawn_supervisor_process(&database_url);
    let state_db_url = database_url.clone();
    let reminder_pool = pool.clone();

    tauri::Builder::default()
        .manage(AppState::new(pool, state_db_url))
        .setup(move |app| {
            spawn_ui_refresh_listener(app.handle().clone(), reminder_pool.clone());
            web::spawn_web_server_if_configured(reminder_pool.clone(), database_url.clone());
            spawn_reminder_worker(reminder_pool.clone());
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_title("Lantor");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            agent_workspace_list,
            agent_workspace_read_file,
            bootstrap,
            artifact_read,
            cancel_agent_work,
            cancel_reminder,
            check_runtime,
            complete_reminder,
            create_agent,
            create_agent_schedule,
            create_channel,
            create_reminder,
            claim_task,
            delete_agent,
            delete_channel,
            delete_message,
            dispatch_agent_work,
            install_supervisor_service,
            dismiss_inbox_items,
            mark_inbox_items_read,
            mark_all_inbox_read,
            mark_channel_read,
            open_dm_with_agent,
            open_external_url,
            retry_agent_work,
            send_message,
            set_message_saved,
            set_channel_agent_membership,
            snooze_reminder,
            start_agent,
            stop_agent,
            uninstall_supervisor_service,
            update_agent,
            update_agent_schedule_status,
            update_channel,
            update_message,
            update_owner_profile,
            update_thread_followed,
            update_task_title,
            update_task_status
        ])
        .run(tauri::generate_context!())
        .expect("error while running Lantor");
}

fn main() {
    let args = env::args().collect::<Vec<_>>();
    if let Some(tool_arg_index) = args.iter().position(|arg| arg == "--agent-context-tool") {
        let tool_args = args
            .get(tool_arg_index + 1..)
            .map(|args| args.to_vec())
            .unwrap_or_default();
        match tauri::async_runtime::block_on(run_agent_context_tool(&tool_args)) {
            Ok(output) => {
                println!("{output}");
                return;
            }
            Err(err) => {
                eprintln!("{err}");
                std::process::exit(1);
            }
        }
    }

    if args.iter().any(|arg| arg == "--supervisor") {
        if let Err(err) = tauri::async_runtime::block_on(run_supervisor()) {
            eprintln!("Lantor supervisor stopped: {err}");
            std::process::exit(1);
        }
    } else {
        run();
    }
}
