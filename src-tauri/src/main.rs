#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod activity_store;
mod agent_environment;
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

use std::{env, fs, path::PathBuf};

use sqlx::{Row, SqlitePool};
use tauri::{LogicalSize, Manager, PhysicalPosition, WebviewWindow, WindowEvent};
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
use system_commands::{
    check_runtime, complete_startup_splash, download_attachment, open_external_url,
};
use ui_notifications::{spawn_ui_events_pruner, spawn_ui_refresh_listener};

const WINDOW_STATE_FILE: &str = "window-state.json";
const MIN_RESTORED_WINDOW_WIDTH: f64 = 1180.0;
const MIN_RESTORED_WINDOW_HEIGHT: f64 = 760.0;

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct WindowState {
    width: f64,
    height: f64,
    #[serde(default)]
    x: Option<f64>,
    #[serde(default)]
    y: Option<f64>,
}

impl WindowState {
    fn has_valid_size(&self) -> bool {
        self.width.is_finite()
            && self.height.is_finite()
            && self.width >= MIN_RESTORED_WINDOW_WIDTH
            && self.height >= MIN_RESTORED_WINDOW_HEIGHT
    }

    fn physical_position(&self) -> Option<PhysicalPosition<i32>> {
        let x = self.x?;
        let y = self.y?;
        if !x.is_finite()
            || !y.is_finite()
            || x < f64::from(i32::MIN)
            || x > f64::from(i32::MAX)
            || y < f64::from(i32::MIN)
            || y > f64::from(i32::MAX)
        {
            return None;
        }
        Some(PhysicalPosition::new(x.round() as i32, y.round() as i32))
    }

    fn has_valid_position(&self) -> bool {
        self.physical_position().is_some()
    }
}

fn physical_rect_fits_within_monitor(
    rect: (f64, f64, f64, f64),
    monitor: (f64, f64, f64, f64),
) -> bool {
    let (left, top, width, height) = rect;
    let (monitor_left, monitor_top, monitor_width, monitor_height) = monitor;
    let right = left + width;
    let bottom = top + height;
    let monitor_right = monitor_left + monitor_width;
    let monitor_bottom = monitor_top + monitor_height;
    left >= monitor_left && right <= monitor_right && top >= monitor_top && bottom <= monitor_bottom
}

fn window_state_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join(WINDOW_STATE_FILE))
}

fn load_window_state(path: &PathBuf) -> Option<WindowState> {
    let value = fs::read_to_string(path).ok()?;
    let state = serde_json::from_str::<WindowState>(&value).ok()?;
    state.has_valid_size().then_some(state)
}

fn can_restore_window_position(window: &WebviewWindow, state: &WindowState) -> bool {
    if !state.has_valid_position() {
        return false;
    }
    let position = state
        .physical_position()
        .expect("valid position should convert to physical position");
    let Ok(monitors) = window.available_monitors() else {
        return true;
    };
    if monitors.is_empty() {
        return true;
    }
    let window_size = window.outer_size().or_else(|_| window.inner_size()).ok();
    let width = window_size
        .map(|size| f64::from(size.width))
        .unwrap_or(state.width);
    let height = window_size
        .map(|size| f64::from(size.height))
        .unwrap_or(state.height);
    let left = f64::from(position.x);
    let top = f64::from(position.y);
    monitors.iter().any(|monitor| {
        let position = monitor.position();
        let size = monitor.size();
        physical_rect_fits_within_monitor(
            (left, top, width, height),
            (
                f64::from(position.x),
                f64::from(position.y),
                f64::from(size.width),
                f64::from(size.height),
            ),
        )
    })
}

fn save_window_state(window: &WebviewWindow, path: &PathBuf) {
    let Ok(size) = window.inner_size() else {
        return;
    };
    let scale_factor = window.scale_factor().unwrap_or(1.0).max(0.1);
    let position = window.outer_position().ok();
    let state = WindowState {
        width: f64::from(size.width) / scale_factor,
        height: f64::from(size.height) / scale_factor,
        x: position.map(|position| f64::from(position.x)),
        y: position.map(|position| f64::from(position.y)),
    };
    if !state.has_valid_size() {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(value) = serde_json::to_string_pretty(&state) {
        let _ = fs::write(path, value);
    }
}

fn restore_window_state(window: &WebviewWindow, path: &PathBuf) {
    let Some(state) = load_window_state(path) else {
        return;
    };
    let _ = window.set_size(LogicalSize::new(state.width, state.height));
    if can_restore_window_position(window, &state) {
        let _ = window.set_position(state.physical_position().expect("position should be valid"));
    } else {
        let _ = window.center();
    }
}

fn install_window_state_persistence(window: &WebviewWindow, path: PathBuf) {
    restore_window_state(window, &path);
    let window_for_events = window.clone();
    window.on_window_event(move |event| match event {
        WindowEvent::CloseRequested { .. } | WindowEvent::Destroyed => {
            save_window_state(&window_for_events, &path);
        }
        _ => {}
    });
}

fn initialize_backend() -> (String, SqlitePool) {
    let database_url = db_url();
    let pool = tauri::async_runtime::block_on(db_connect_with_url(&database_url, 5))
        .expect("failed to connect Lantor SQLite database");

    tauri::async_runtime::block_on(migrate(&pool)).expect("failed to initialize Lantor schema");
    launch_agent::spawn_supervisor_process(&database_url);
    (database_url, pool)
}

fn spawn_shared_background_workers(pool: SqlitePool, database_url: String) {
    spawn_ui_events_pruner(pool.clone());
    web::spawn_web_server_if_configured(pool.clone(), database_url);
    spawn_reminder_worker(pool);
}

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
    let (database_url, pool) = initialize_backend();
    let state_db_url = database_url.clone();
    let reminder_pool = pool.clone();

    tauri::Builder::default()
        .manage(AppState::new(pool, state_db_url))
        .setup(move |app| {
            spawn_ui_refresh_listener(app.handle().clone(), reminder_pool.clone());
            spawn_shared_background_workers(reminder_pool.clone(), database_url.clone());
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_title("Lantor");
                if let Some(path) = window_state_path(app.handle()) {
                    install_window_state_persistence(&window, path);
                }
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
            complete_startup_splash,
            download_attachment,
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

fn run_web_only() {
    let (database_url, pool) = initialize_backend();
    spawn_shared_background_workers(pool, database_url);
    eprintln!("Lantor web-only mode is running. Press Ctrl-C to stop.");
    tauri::async_runtime::block_on(async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            eprintln!("failed to listen for Ctrl-C: {err}");
            std::future::pending::<()>().await;
        }
    });
    eprintln!("Lantor web-only mode stopped.");
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
    } else if args.iter().any(|arg| arg == "--web-only") {
        run_web_only();
    } else {
        run();
    }
}

#[cfg(test)]
mod window_state_tests {
    use super::{
        load_window_state, physical_rect_fits_within_monitor, WindowState,
        MIN_RESTORED_WINDOW_HEIGHT, MIN_RESTORED_WINDOW_WIDTH,
    };
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_window_state_path() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after the unix epoch")
            .as_nanos();
        let count = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "lantor-window-state-{}-{nanos}-{count}.json",
            std::process::id()
        ))
    }

    #[test]
    fn loads_valid_saved_window_state() {
        let path = temp_window_state_path();
        let value = serde_json::to_string(&WindowState {
            width: MIN_RESTORED_WINDOW_WIDTH + 120.0,
            height: MIN_RESTORED_WINDOW_HEIGHT + 80.0,
            x: Some(-240.0),
            y: Some(80.0),
        })
        .expect("window state should serialize");
        fs::write(&path, value).expect("window state should be written");

        let state = load_window_state(&path).expect("valid window state should load");
        assert!(state.has_valid_size());
        assert!(state.has_valid_position());
        assert_eq!(state.x, Some(-240.0));
        assert_eq!(state.y, Some(80.0));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn loads_legacy_size_only_window_state() {
        let path = temp_window_state_path();
        let value = serde_json::json!({
            "width": MIN_RESTORED_WINDOW_WIDTH + 120.0,
            "height": MIN_RESTORED_WINDOW_HEIGHT + 80.0
        })
        .to_string();
        fs::write(&path, value).expect("window state should be written");

        let state = load_window_state(&path).expect("legacy window state should load");
        assert!(state.has_valid_size());
        assert!(!state.has_valid_position());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn ignores_saved_window_state_below_minimums() {
        let path = temp_window_state_path();
        let value = serde_json::to_string(&WindowState {
            width: MIN_RESTORED_WINDOW_WIDTH - 1.0,
            height: MIN_RESTORED_WINDOW_HEIGHT,
            x: Some(120.0),
            y: Some(80.0),
        })
        .expect("window state should serialize");
        fs::write(&path, value).expect("window state should be written");

        assert!(load_window_state(&path).is_none());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn detects_when_window_rect_fits_monitor_bounds() {
        assert!(physical_rect_fits_within_monitor(
            (100.0, 100.0, 1200.0, 800.0),
            (0.0, 0.0, 1440.0, 900.0),
        ));
        assert!(!physical_rect_fits_within_monitor(
            (2000.0, 100.0, 1200.0, 800.0),
            (0.0, 0.0, 1440.0, 900.0),
        ));
        assert!(!physical_rect_fits_within_monitor(
            (-1000.0, 100.0, 1200.0, 800.0),
            (0.0, 0.0, 1440.0, 900.0),
        ));
    }

    #[test]
    fn rejects_non_finite_or_out_of_range_window_position() {
        assert!(!WindowState {
            width: MIN_RESTORED_WINDOW_WIDTH,
            height: MIN_RESTORED_WINDOW_HEIGHT,
            x: Some(f64::NAN),
            y: Some(0.0),
        }
        .has_valid_position());
        assert!(!WindowState {
            width: MIN_RESTORED_WINDOW_WIDTH,
            height: MIN_RESTORED_WINDOW_HEIGHT,
            x: Some(f64::from(i32::MAX) + 1.0),
            y: Some(0.0),
        }
        .has_valid_position());
    }
}
