#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod activity_store;
mod agent_inbox_wake;
mod agent_memory;
mod agent_profile;
mod agent_routing;
mod agent_work_dispatch;
mod agent_workspace;
mod artifact_store;
mod attachments;
mod channels;
mod context_tool;
mod domain;
mod events;
mod launch_agent;
mod message_store;
mod models;
mod owner_inbox;
mod prompts;
mod runtime;
mod task_messages;
mod task_store;
mod text;
mod ui_notifications;
mod usage;
mod web;

use std::{
    env, fs,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    process::{Command as StdCommand, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    Row, SqlitePool,
};
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::str::FromStr;
use tauri::{Manager, State};
use tokio::{sync::Semaphore, time::sleep};
use uuid::Uuid;

use activity_store::{load_agent_activities, load_agent_runs, load_agent_work_items};
use agent_inbox_wake::{
    agent_has_active_or_pending_start, agent_runtime, build_steer_followup_prompt,
    create_agent_inbox_item, enqueue_agent_work_if_available, ensure_agent_inbox_wake_work_item,
    load_inbox_wake_items_for_work_item, AgentInboxItemInput,
};
#[cfg(test)]
use agent_inbox_wake::{inbox_wake_context, InboxWakeItem, InboxWakeSummary};
use agent_memory::append_run_log;
use agent_profile::{
    create_agent_in_pool, delete_agent_in_pool, load_agents, load_owner_profile,
    update_agent_in_pool, update_owner_profile_in_pool,
};
#[cfg(test)]
use agent_routing::{
    extract_agent_mentions, queue_mentions_as_work_items, upsert_agent_thread_subscription,
    MentionDispatchOrigin,
};
use agent_work_dispatch::{
    cancel_agent_work, cancel_agent_work_in_pool, claim_task, claim_task_in_pool,
    dispatch_agent_work, dispatch_task_assignment_to_agent, mark_task_after_work_item_finished,
    retry_agent_work, retry_agent_work_in_pool, try_claim_unassigned_task,
};
use agent_workspace::{agent_workspace_list, agent_workspace_read_file};
#[cfg(test)]
use attachments::AgentAttachmentFile;
use channels::{
    create_channel_with_members, delete_channel_in_pool, load_channel_members, load_channels,
    normalize_channel_name, open_dm_with_agent_in_pool, set_channel_agent_membership_in_pool,
    update_channel_in_pool,
};
use context_tool::run_agent_context_tool;
use domain::{
    reminders::{
        cancel_reminder, complete_reminder, create_reminder, load_reminders, snooze_reminder,
    },
    schedules::{create_agent_schedule, load_agent_schedules, update_agent_schedule_status},
    spawn_reminder_worker,
};
#[cfg(test)]
use events::activity::activity_status;
use events::activity::record_agent_activity;
#[cfg(test)]
use events::control::{
    claim_agent_event, extract_agent_event_json, handle_agent_event, AgentEvent,
};
use message_store::{
    delete_message_in_pool, load_artifact, load_artifacts, load_messages, load_saved_messages,
    send_owner_message_in_pool, set_message_saved_in_pool, update_message_in_pool,
};
use models::{
    Artifact, AttachmentUpload, Bootstrap, LaunchAgentStatus, RuntimeCheck, SupervisorCommand,
    SupervisorStatus,
};
use owner_inbox::{
    dismiss_inbox_items_in_pool, load_dismissed_inbox_items, load_read_inbox_items,
    mark_all_owner_inbox_read_in_pool, mark_channel_read_in_pool, mark_inbox_items_read_in_pool,
    update_thread_followed_in_pool,
};
use prompts::{
    build_streaming_work_item_prompt, build_work_item_prompt, load_agent_memory_context,
    prepend_memory_context,
};
#[cfg(test)]
use prompts::{ensure_agent_workspace, AGENT_MEMORY_CONTEXT_LIMIT, WORK_ITEM_FINISH_PROMPT};
use runtime::claude::{self, WarmClaudeRegistry};
use runtime::codex::{self, WarmCodexRegistry};
use runtime::process::{
    effective_launch_command, start_process_agent, terminate_process_group, ProcessAgentLaunch,
};
use runtime::streaming::mark_run_work_item_silent;
use runtime::supervisor::{
    claim_next_supervisor_command, cleanup_supervisor_commands, finish_supervisor_command,
    mark_orphaned_agent_runs, recover_supervisor_commands_at_startup, write_supervisor_heartbeat,
};
use runtime::surface::{
    append_claude_thread_context, same_codex_surface, CodexActiveTurnScheduleState,
};
use task_store::{load_tasks, update_task_status_in_pool, update_task_title_in_pool};
use ui_notifications::{
    notify_supervisor_wake, notify_ui_agent_run_changed, notify_ui_refresh,
    notify_ui_work_item_changed, spawn_ui_refresh_listener,
};
use usage::{agent_budget_exhausted, backfill_agent_run_usage_from_logs};

const DEFAULT_DATABASE_URL: &str = "sqlite://~/Library/Application Support/Lantor/lantor.sqlite";
const AGENT_CONTEXT_TOOL_MESSAGE_LIMIT: usize = 2_000;
const SUPERVISOR_COMMAND_CONCURRENCY: usize = 4;
const SUPERVISOR_IDLE_SLEEP: Duration = Duration::from_secs(2);
const SUPERVISOR_ERROR_BACKOFF_INITIAL: Duration = Duration::from_secs(1);
const SUPERVISOR_ERROR_BACKOFF_MAX: Duration = Duration::from_secs(10);
pub(crate) fn expand_home_path(value: &str) -> String {
    let value = value.trim();
    if value == "~" {
        return env::var("HOME").unwrap_or_else(|_| value.to_owned());
    }
    if let Some(rest) = value.strip_prefix("~/") {
        if let Ok(home) = env::var("HOME") {
            return PathBuf::from(home).join(rest).to_string_lossy().to_string();
        }
    }
    value.to_owned()
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) pool: SqlitePool,
    db_url: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DismissInboxItemInput {
    item_id: String,
    dismissed_until: DateTime<Utc>,
}

pub(crate) type CommandResult<T> = Result<T, String>;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateChannelResult {
    channel_id: Uuid,
}

fn db_url() -> String {
    let configured = env::var("LANTOR_DATABASE_URL").unwrap_or_else(|_| {
        env::var("DATABASE_URL")
            .ok()
            .filter(|url| url.trim_start().starts_with("sqlite:"))
            .unwrap_or_else(|| DEFAULT_DATABASE_URL.to_owned())
    });
    if let Some(path) = configured.strip_prefix("sqlite://") {
        return format!("sqlite://{}", expand_home_path(path));
    }
    if let Some(path) = configured.strip_prefix("sqlite:") {
        return format!("sqlite:{}", expand_home_path(path));
    }
    configured
}

pub(crate) async fn db_connect_with_url(
    database_url: &str,
    max_connections: u32,
) -> Result<SqlitePool, sqlx::Error> {
    let options = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(10));
    if !database_url.contains(":memory:") {
        if let Some(parent) = options.get_filename().parent() {
            fs::create_dir_all(parent)?;
        }
    }
    SqlitePoolOptions::new()
        .max_connections(max_connections)
        .connect_with(options)
        .await
}

pub(crate) async fn db_connect(max_connections: u32) -> Result<SqlitePool, sqlx::Error> {
    db_connect_with_url(&db_url(), max_connections).await
}

fn sqlite_database_file_path(database_url: &str) -> CommandResult<Option<PathBuf>> {
    if database_url.contains(":memory:") {
        return Ok(None);
    }
    let options = SqliteConnectOptions::from_str(database_url).map_err(to_string)?;
    Ok(Some(options.get_filename().to_path_buf()))
}

#[cfg(unix)]
fn try_lock_supervisor_file(file: &fs::File) -> std::io::Result<()> {
    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;
    extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }
    let result = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn try_lock_supervisor_file(_file: &fs::File) -> std::io::Result<()> {
    Ok(())
}

fn acquire_supervisor_lock(database_url: &str) -> CommandResult<Option<fs::File>> {
    let Some(database_path) = sqlite_database_file_path(database_url)? else {
        return Ok(None);
    };
    let lock_path = PathBuf::from(format!("{}.supervisor.lock", database_path.display()));
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).map_err(to_string)?;
    }
    let file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(to_string)?;
    match try_lock_supervisor_file(&file) {
        Ok(()) => Ok(Some(file)),
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => Err(format!(
            "another Lantor supervisor is already running for {}",
            database_path.display()
        )),
        Err(err) => Err(err.to_string()),
    }
}

pub(crate) async fn migrate(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    for statement in [
        r#"
        create table if not exists owner_profile (
            id integer primary key default 1 check (id = 1),
            display_name text not null default 'Me',
            avatar text not null default 'dicebear:dylan:owner',
            description text not null default 'local owner',
            updated_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
        )
        "#,
        r#"
        create table if not exists agents (
            id blob primary key not null default (randomblob(16)),
            handle text not null unique,
            display_name text not null,
            role text not null default 'agent',
            status text not null default 'idle',
            runtime text not null default 'codex',
            model text not null default 'gpt-5.5',
            avatar text not null default '',
            description text not null default '',
            launch_command text not null default '',
            working_directory text not null default '',
            daily_budget_micros integer not null default 0,
            reasoning_effort text not null default 'medium',
            service_tier text not null default '',
            created_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
        )
        "#,
        r#"
        create table if not exists channels (
            id blob primary key not null default (randomblob(16)),
            name text not null unique,
            description text not null default '',
            unread_count integer not null default 0,
            kind text not null default 'channel',
            dm_agent_id blob references agents(id) on delete cascade,
            created_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
        )
        "#,
        r#"
        create table if not exists messages (
            id blob primary key not null default (randomblob(16)),
            channel_id blob not null references channels(id) on delete cascade,
            thread_root_id blob references messages(id) on delete cascade,
            sender_agent_id blob references agents(id) on delete set null,
            sender_name text not null,
            sender_role text not null default 'human',
            body text not null,
            is_task boolean not null default 0,
            thread_followed boolean not null default 1,
            delivery_state text not null default 'complete',
            stream_key text not null default '',
            created_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
            updated_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
        )
        "#,
        r#"
        create table if not exists message_attachments (
            id blob primary key not null default (randomblob(16)),
            message_id blob not null references messages(id) on delete cascade,
            original_name text not null,
            mime_type text not null,
            size_bytes integer not null,
            storage_path text not null,
            created_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
        )
        "#,
        r#"
        create table if not exists saved_messages (
            id blob primary key not null default (randomblob(16)),
            message_id blob not null unique references messages(id) on delete cascade,
            created_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
        )
        "#,
        r#"
        create table if not exists artifacts (
            id blob primary key not null default (randomblob(16)),
            message_id blob not null references messages(id) on delete cascade,
            channel_id blob not null references channels(id) on delete cascade,
            thread_root_id blob references messages(id) on delete set null,
            creator_agent_id blob references agents(id) on delete set null,
            kind text not null,
            title text not null,
            summary text not null default '',
            content text not null,
            metadata text not null default '{}',
            created_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
            updated_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
        )
        "#,
        r#"
        create table if not exists runtime_sessions (
            id blob primary key not null default (randomblob(16)),
            agent_id blob not null references agents(id) on delete cascade,
            runtime text not null,
            provider_thread_id text not null,
            status text not null default 'idle',
            created_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
            updated_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
            unique(agent_id, runtime)
        )
        "#,
        r#"
        create table if not exists tasks (
            number integer primary key autoincrement,
            id blob not null unique default (randomblob(16)),
            message_id blob not null unique references messages(id) on delete cascade,
            channel_id blob not null references channels(id) on delete cascade,
            title text not null,
            status text not null default 'todo',
            assignee_agent_id blob references agents(id) on delete set null,
            version integer not null default 0,
            created_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
            updated_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
        )
        "#,
        r#"
        create table if not exists reminders (
            id blob primary key not null default (randomblob(16)),
            channel_id blob references channels(id) on delete set null,
            creator_agent_id blob references agents(id) on delete set null,
            thread_root_id blob references messages(id) on delete set null,
            message_id blob references messages(id) on delete set null,
            title text not null,
            note text not null default '',
            status text not null default 'scheduled',
            recurrence text not null default 'none',
            due_at text not null,
            fired_at text,
            completed_at text,
            created_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
            updated_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
        )
        "#,
        r#"
        create table if not exists reminder_events (
            id blob primary key not null default (randomblob(16)),
            reminder_id blob not null references reminders(id) on delete cascade,
            event_type text not null,
            detail text not null default '',
            created_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
        )
        "#,
        r#"
        create table if not exists agent_schedules (
            id blob primary key not null default (randomblob(16)),
            agent_id blob not null references agents(id) on delete cascade,
            channel_id blob not null references channels(id) on delete cascade,
            thread_root_id blob references messages(id) on delete set null,
            title text not null,
            prompt text not null default '',
            cadence text not null default 'daily',
            status text not null default 'active',
            next_run_at text not null,
            last_run_at text,
            last_work_item_id blob,
            created_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
            updated_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
        )
        "#,
        r#"
        create table if not exists agent_runs (
            id blob primary key not null default (randomblob(16)),
            agent_id blob not null references agents(id) on delete cascade,
            work_item_id blob references agent_work_items(id) on delete set null,
            command text not null,
            working_directory text not null default '',
            status text not null default 'starting',
            pid integer,
            exit_code integer,
            log text not null default '',
            input_tokens integer not null default 0,
            output_tokens integer not null default 0,
            cost_micros integer not null default 0,
            started_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
            stopped_at text
        )
        "#,
        r#"
        create table if not exists agent_activities (
            id blob primary key not null default (randomblob(16)),
            agent_id blob references agents(id) on delete set null,
            agent_handle text not null default '',
            run_id blob references agent_runs(id) on delete set null,
            kind text not null,
            phase text not null default 'event',
            status text not null default 'info',
            title text not null,
            summary text not null default '',
            detail text not null default '',
            metadata text not null default '{}',
            created_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
        )
        "#,
        r#"
        create table if not exists agent_work_items (
            id blob primary key not null default (randomblob(16)),
            agent_id blob not null references agents(id) on delete cascade,
            channel_id blob references channels(id) on delete set null,
            thread_root_id blob references messages(id) on delete set null,
            source_message_id blob references messages(id) on delete set null,
            inbox_item_id blob,
            task_id blob references tasks(id) on delete set null,
            source_kind text not null default 'manual',
            title text not null,
            context text not null default '',
            status text not null default 'queued',
            run_id blob references agent_runs(id) on delete set null,
            created_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
            updated_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
            completed_at text
        )
        "#,
        r#"
        create table if not exists agent_inbox_items (
            id blob primary key not null default (randomblob(16)),
            agent_id blob not null references agents(id) on delete cascade,
            channel_id blob references channels(id) on delete set null,
            thread_root_id blob references messages(id) on delete set null,
            source_message_id blob references messages(id) on delete set null,
            task_id blob references tasks(id) on delete set null,
            kind text not null,
            priority integer not null default 50,
            state text not null default 'unread',
            title text not null,
            body_preview text not null default '',
            payload text not null default '{}',
            work_item_id blob references agent_work_items(id) on delete set null,
            created_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
            updated_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
            archived_at text
        )
        "#,
        r#"
        create table if not exists agent_thread_subscriptions (
            agent_id blob not null references agents(id) on delete cascade,
            channel_id blob not null references channels(id) on delete cascade,
            thread_root_id blob not null references messages(id) on delete cascade,
            source_kind text not null default 'manual',
            last_source_message_id blob references messages(id) on delete set null,
            created_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
            updated_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
            primary key (agent_id, thread_root_id)
        )
        "#,
        r#"
        create table if not exists agent_event_receipts (
            run_id blob not null references agent_runs(id) on delete cascade,
            event_json text not null,
            event_hash text not null,
            created_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
            primary key (run_id, event_hash)
        )
        "#,
        r#"
        create table if not exists supervisor_commands (
            id blob primary key not null default (randomblob(16)),
            command_type text not null,
            agent_id blob references agents(id) on delete cascade,
            run_id blob references agent_runs(id) on delete cascade,
            work_item_id blob references agent_work_items(id) on delete set null,
            status text not null default 'pending',
            error text not null default '',
            created_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
            updated_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
        )
        "#,
        r#"
        create table if not exists supervisor_state (
            id integer primary key default 1 check (id = 1),
            pid integer,
            status text not null default 'offline',
            updated_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
        )
        "#,
        r#"
        create table if not exists channel_read_state (
            channel_id blob primary key references channels(id) on delete cascade,
            last_read_at text not null default '0001-01-01T00:00:00+00:00'
        )
        "#,
        r#"
        create table if not exists owner_inbox_dismissals (
            item_id text primary key,
            dismissed_until text not null,
            dismissed_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
        )
        "#,
        r#"
        create table if not exists owner_inbox_read_state (
            item_id text primary key,
            read_until text not null,
            read_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
        )
        "#,
        r#"
        create table if not exists owner_inbox_hidden_items (
            item_id text primary key,
            hidden_until text not null,
            hidden_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
        )
        "#,
        r#"
        create table if not exists channel_members (
            channel_id blob not null references channels(id) on delete cascade,
            agent_id blob not null references agents(id) on delete cascade,
            created_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
            primary key (channel_id, agent_id)
        )
        "#,
        r#"
        create table if not exists ui_events (
            id integer primary key autoincrement,
            event_json text not null,
            created_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
        )
        "#,
    ] {
        sqlx::query(statement).execute(pool).await?;
    }

    for statement in [
        "create unique index if not exists channels_dm_unique on channels(dm_agent_id) where kind = 'dm' and dm_agent_id is not null",
        "create unique index if not exists messages_stream_key_unique on messages(stream_key) where stream_key <> ''",
        "create index if not exists message_attachments_message_id_idx on message_attachments(message_id)",
        "create index if not exists saved_messages_created_at_idx on saved_messages(created_at desc)",
        "create index if not exists artifacts_message_id_idx on artifacts(message_id)",
        "create index if not exists artifacts_channel_id_idx on artifacts(channel_id)",
        "create index if not exists reminders_due_idx on reminders(status, due_at)",
        "create index if not exists agent_schedules_due_idx on agent_schedules(status, next_run_at)",
        "create index if not exists agent_inbox_items_agent_state_idx on agent_inbox_items(agent_id, state, priority desc, created_at)",
        "create unique index if not exists agent_inbox_items_source_unique on agent_inbox_items(agent_id, source_message_id, kind) where source_message_id is not null",
        "create index if not exists ui_events_created_idx on ui_events(created_at)",
    ] {
        sqlx::query(statement).execute(pool).await?;
    }

    sqlx::query(
        r#"
        insert into owner_profile (id, display_name, avatar, description)
        values (1, 'Me', 'dicebear:dylan:owner', 'local owner')
        on conflict (id) do nothing
        "#,
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"
        update owner_profile
        set avatar = 'dicebear:dylan:owner'
        where id = 1 and avatar = 'M'
        "#,
    )
    .execute(pool)
    .await?;
    backfill_agent_run_usage_from_logs(pool).await?;

    Ok(())
}

#[tauri::command]
async fn bootstrap(state: State<'_, AppState>) -> CommandResult<Bootstrap> {
    load_bootstrap(&state.pool, state.db_url.clone()).await
}

fn configured_web_base_url() -> Option<String> {
    if let Ok(value) = env::var("LANTOR_WEB_PUBLIC_URL") {
        let trimmed = value.trim().trim_end_matches('/').to_owned();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }
    let bind = crate::web::resolve_web_bind()?;
    let addr = bind.parse::<SocketAddr>().ok()?;
    let host = match addr.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => "127.0.0.1".to_owned(),
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) if ip.is_unspecified() => "[::1]".to_owned(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    };
    Some(format!("http://{host}:{}", addr.port()))
}

fn normalize_external_url(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once(':')?;
    let scheme = scheme.to_ascii_lowercase();
    match scheme.as_str() {
        "http" | "https" if rest.starts_with("//") => Some(url.to_owned()),
        "mailto" if !rest.is_empty() => Some(url.to_owned()),
        "file" if rest.starts_with("//") => Some(url.to_owned()),
        _ => None,
    }
}

fn strip_local_path_line_suffix(value: &str) -> &str {
    if Path::new(value).exists() {
        return value;
    }

    if let Some((path, line)) = value.rsplit_once(':') {
        if !line.is_empty() && line.chars().all(|c| c.is_ascii_digit()) && Path::new(path).exists()
        {
            return path;
        }
    }

    if let Some((path, line)) = value.rsplit_once("#L") {
        if !line.is_empty() && line.chars().all(|c| c.is_ascii_digit()) && Path::new(path).exists()
        {
            return path;
        }
    }

    value
}

fn normalize_local_path_link(url: &str) -> Option<String> {
    let expanded = expand_home_path(url);
    let without_line_suffix = strip_local_path_line_suffix(&expanded);
    let path = Path::new(without_line_suffix);
    if path.is_absolute() && path.exists() {
        return Some(path.to_string_lossy().to_string());
    }
    None
}

fn normalize_open_link_target(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() || trimmed.len() > 4096 || trimmed.chars().any(char::is_control) {
        return None;
    }

    normalize_external_url(trimmed).or_else(|| normalize_local_path_link(trimmed))
}

fn open_link_target_with_system(target: &str) -> CommandResult<()> {
    #[cfg(target_os = "macos")]
    let status = StdCommand::new("open")
        .arg(target)
        .status()
        .map_err(to_string)?;

    #[cfg(target_os = "windows")]
    let status = StdCommand::new("cmd")
        .args(["/C", "start", "", target])
        .status()
        .map_err(to_string)?;

    #[cfg(all(unix, not(target_os = "macos")))]
    let status = StdCommand::new("xdg-open")
        .arg(target)
        .status()
        .map_err(to_string)?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("failed to open link target: {status}"))
    }
}

#[tauri::command]
async fn open_external_url(url: String) -> CommandResult<()> {
    let target = normalize_open_link_target(&url).ok_or_else(|| {
        "only http, https, mailto, file, and existing local file links can be opened".to_owned()
    })?;
    tauri::async_runtime::spawn_blocking(move || open_link_target_with_system(&target))
        .await
        .map_err(to_string)?
}

pub(crate) async fn load_bootstrap(pool: &SqlitePool, db_url: String) -> CommandResult<Bootstrap> {
    let owner_profile = load_owner_profile(pool).await?;
    let channels = load_channels(pool).await?;
    let channel_members = load_channel_members(pool).await?;
    let agents = load_agents(pool).await?;
    let messages = load_messages(pool).await?;
    let saved_messages = load_saved_messages(pool).await?;
    let dismissed_inbox_items = load_dismissed_inbox_items(pool).await?;
    let read_inbox_items = load_read_inbox_items(pool).await?;
    let artifacts = load_artifacts(pool).await?;
    let tasks = load_tasks(pool).await?;
    let reminders = load_reminders(pool).await?;
    let agent_schedules = load_agent_schedules(pool).await?;
    let agent_runs = load_agent_runs(pool).await?;
    let agent_work_items = load_agent_work_items(pool).await?;
    let agent_activities = load_agent_activities(pool).await?;
    let supervisor = load_supervisor_status(pool).await?;
    let launch_agent = launch_agent::load_launch_agent_status()?;

    Ok(Bootstrap {
        db_url,
        web_base_url: configured_web_base_url(),
        owner_profile,
        channels,
        channel_members,
        agents,
        messages,
        saved_messages,
        dismissed_inbox_items,
        read_inbox_items,
        artifacts,
        tasks,
        reminders,
        agent_schedules,
        agent_runs,
        agent_work_items,
        agent_activities,
        supervisor,
        launch_agent,
    })
}

#[tauri::command]
async fn check_runtime(runtime: String) -> CommandResult<RuntimeCheck> {
    check_runtime_in_env(runtime).await
}

pub(crate) async fn check_runtime_in_env(runtime: String) -> CommandResult<RuntimeCheck> {
    let runtime = runtime.trim().to_owned();
    let command = match runtime.as_str() {
        "codex" => "codex",
        "claude" => "claude",
        _ => {
            return Ok(RuntimeCheck {
                runtime,
                command: String::new(),
                available: false,
                detail: "Unknown runtime".to_owned(),
            });
        }
    };

    let script = format!(
        "if command -v {command} >/dev/null 2>&1; then {command} --version 2>&1 | head -n 1; else echo '{command} not found in PATH' >&2; exit 127; fi"
    );
    let output = StdCommand::new("/bin/zsh")
        .arg("-lc")
        .arg(script)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(to_string)?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let detail = if !stdout.is_empty() {
        stdout
    } else if !stderr.is_empty() {
        stderr
    } else if output.status.success() {
        format!("{command} found")
    } else {
        format!("{command} unavailable")
    };

    Ok(RuntimeCheck {
        runtime,
        command: command.to_owned(),
        available: output.status.success(),
        detail,
    })
}

#[tauri::command]
async fn create_channel(
    name: String,
    description: Option<String>,
    agent_ids: Option<Vec<Uuid>>,
    state: State<'_, AppState>,
) -> CommandResult<CreateChannelResult> {
    let channel_id = create_channel_with_members(
        &state.pool,
        &name,
        description.as_deref().unwrap_or(""),
        agent_ids,
    )
    .await?;
    Ok(CreateChannelResult { channel_id })
}

#[tauri::command]
async fn update_channel(
    channel_id: Uuid,
    name: String,
    description: String,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    update_channel_in_pool(&state.pool, channel_id, name, description).await
}

#[tauri::command]
async fn set_channel_agent_membership(
    channel_id: Uuid,
    agent_id: Uuid,
    member: bool,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    set_channel_agent_membership_in_pool(&state.pool, channel_id, agent_id, member).await
}

#[tauri::command]
async fn update_owner_profile(
    display_name: String,
    avatar: String,
    description: String,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    update_owner_profile_in_pool(&state.pool, display_name, avatar, description).await
}

#[tauri::command]
async fn delete_channel(channel_id: Uuid, state: State<'_, AppState>) -> CommandResult<()> {
    delete_channel_in_pool(&state.pool, channel_id).await
}

#[tauri::command]
async fn open_dm_with_agent(agent_id: Uuid, state: State<'_, AppState>) -> CommandResult<String> {
    open_dm_with_agent_in_pool(&state.pool, agent_id).await
}

#[tauri::command]
async fn artifact_read(artifact_id: Uuid, state: State<'_, AppState>) -> CommandResult<Artifact> {
    load_artifact(&state.pool, artifact_id).await
}

#[tauri::command]
async fn create_agent(
    handle: String,
    display_name: String,
    role: Option<String>,
    runtime: String,
    model: String,
    reasoning_effort: Option<String>,
    service_tier: Option<String>,
    avatar: Option<String>,
    description: Option<String>,
    launch_command: String,
    working_directory: String,
    daily_budget_micros: Option<i64>,
    state: State<'_, AppState>,
) -> CommandResult<String> {
    create_agent_in_pool(
        &state.pool,
        handle,
        display_name,
        role,
        runtime,
        model,
        reasoning_effort,
        service_tier,
        avatar,
        description,
        launch_command,
        working_directory,
        daily_budget_micros,
    )
    .await
    .map(|agent_id| agent_id.to_string())
}

#[tauri::command]
async fn update_agent(
    agent_id: Uuid,
    handle: String,
    display_name: String,
    role: Option<String>,
    runtime: String,
    model: String,
    reasoning_effort: Option<String>,
    service_tier: Option<String>,
    avatar: Option<String>,
    description: String,
    launch_command: String,
    working_directory: String,
    daily_budget_micros: Option<i64>,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    update_agent_in_pool(
        &state.pool,
        agent_id,
        handle,
        display_name,
        role,
        runtime,
        model,
        reasoning_effort,
        service_tier,
        avatar,
        description,
        launch_command,
        working_directory,
        daily_budget_micros,
    )
    .await
}

#[tauri::command]
async fn delete_agent(agent_id: Uuid, state: State<'_, AppState>) -> CommandResult<()> {
    delete_agent_in_pool(&state.pool, agent_id).await
}

#[tauri::command]
async fn start_agent(agent_id: Uuid, state: State<'_, AppState>) -> CommandResult<()> {
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
    .fetch_optional(&state.pool)
    .await
    .map_err(to_string)?;

    if active_run.is_some() {
        return Err("agent already has an active run".to_owned());
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
    .fetch_optional(&state.pool)
    .await
    .map_err(to_string)?;

    if pending_start.is_some() {
        return Err("agent already has a pending start command".to_owned());
    }

    sqlx::query(
        r#"
        insert into supervisor_commands (command_type, agent_id)
        values ('start_agent', $1)
        "#,
    )
    .bind(agent_id)
    .execute(&state.pool)
    .await
    .map_err(to_string)?;
    let _ = notify_supervisor_wake(&state.pool).await;
    let _ = notify_ui_refresh(&state.pool, "supervisor_command").await;
    sqlx::query("update agents set status = 'queued' where id = $1")
        .bind(agent_id)
        .execute(&state.pool)
        .await
        .map_err(to_string)?;
    record_agent_activity(
        &state.pool,
        Some(agent_id),
        None,
        "run",
        "Start queued",
        "Waiting for supervisor to launch the agent",
    )
    .await?;

    Ok(())
}

#[tauri::command]
async fn stop_agent(run_id: Uuid, state: State<'_, AppState>) -> CommandResult<()> {
    let row = sqlx::query(
        r#"
        select agent_id, pid, work_item_id
        from agent_runs
        where id = $1 and stopped_at is null
        "#,
    )
    .bind(run_id)
    .fetch_one(&state.pool)
    .await
    .map_err(to_string)?;

    let agent_id: Uuid = row.get("agent_id");
    sqlx::query(
        r#"
        insert into supervisor_commands (command_type, agent_id, run_id)
        values ('stop_run', $1, $2)
        "#,
    )
    .bind(agent_id)
    .bind(run_id)
    .execute(&state.pool)
    .await
    .map_err(to_string)?;
    let _ = notify_supervisor_wake(&state.pool).await;
    let _ = notify_ui_refresh(&state.pool, "supervisor_command").await;
    sqlx::query("update agent_runs set status = 'stopping' where id = $1")
        .bind(run_id)
        .execute(&state.pool)
        .await
        .map_err(to_string)?;
    notify_ui_agent_run_changed(&state.pool, run_id, "run_stopping").await;
    sqlx::query("update agents set status = 'stopping' where id = $1")
        .bind(agent_id)
        .execute(&state.pool)
        .await
        .map_err(to_string)?;
    record_agent_activity(
        &state.pool,
        Some(agent_id),
        Some(run_id),
        "run",
        "Stop requested",
        "Stop command queued for supervisor",
    )
    .await?;

    Ok(())
}

#[tauri::command]
async fn install_supervisor_service(
    state: State<'_, AppState>,
) -> CommandResult<LaunchAgentStatus> {
    let status = launch_agent::install_supervisor_service(&state.db_url)?;
    let _ = notify_ui_refresh(&state.pool, "supervisor_service_installed").await;
    Ok(status)
}

#[tauri::command]
async fn uninstall_supervisor_service(
    state: State<'_, AppState>,
) -> CommandResult<LaunchAgentStatus> {
    let status = launch_agent::uninstall_supervisor_service()?;

    sqlx::query("update supervisor_state set status = 'offline', updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') where id = 1")
        .execute(&state.pool)
        .await
        .map_err(to_string)?;

    let _ = notify_ui_refresh(&state.pool, "supervisor_service_uninstalled").await;
    Ok(status)
}

#[tauri::command]
async fn send_message(
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    body: String,
    as_task: bool,
    attachments: Option<Vec<AttachmentUpload>>,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    send_owner_message_in_pool(
        &state.pool,
        channel_id,
        thread_root_id,
        &body,
        as_task,
        attachments.unwrap_or_default(),
    )
    .await
}

#[tauri::command]
async fn update_message(
    message_id: Uuid,
    body: String,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    update_message_in_pool(&state.pool, message_id, &body).await
}

#[tauri::command]
async fn delete_message(message_id: Uuid, state: State<'_, AppState>) -> CommandResult<()> {
    delete_message_in_pool(&state.pool, message_id).await
}

#[tauri::command]
async fn set_message_saved(
    message_id: Uuid,
    saved: bool,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    set_message_saved_in_pool(&state.pool, message_id, saved).await
}

#[tauri::command]
async fn update_task_status(
    task_id: Uuid,
    status: String,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    update_task_status_in_pool(&state.pool, task_id, status).await
}

#[tauri::command]
async fn update_task_title(
    task_id: Uuid,
    title: String,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    update_task_title_in_pool(&state.pool, task_id, title).await
}

#[tauri::command]
async fn mark_channel_read(channel_id: Uuid, state: State<'_, AppState>) -> CommandResult<()> {
    mark_channel_read_in_pool(&state.pool, channel_id).await
}

#[tauri::command]
async fn dismiss_inbox_items(
    items: Vec<DismissInboxItemInput>,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    dismiss_inbox_items_in_pool(
        &state.pool,
        items
            .into_iter()
            .map(|item| (item.item_id, item.dismissed_until)),
    )
    .await
}

#[tauri::command]
async fn mark_inbox_items_read(
    items: Vec<DismissInboxItemInput>,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    mark_inbox_items_read_in_pool(
        &state.pool,
        items
            .into_iter()
            .map(|item| (item.item_id, item.dismissed_until)),
    )
    .await
}

#[tauri::command]
async fn mark_all_inbox_read(state: State<'_, AppState>) -> CommandResult<()> {
    mark_all_owner_inbox_read_in_pool(&state.pool).await
}

#[tauri::command]
async fn update_thread_followed(
    thread_root_id: Uuid,
    followed: bool,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    update_thread_followed_in_pool(&state.pool, thread_root_id, followed).await
}

async fn load_supervisor_status(pool: &SqlitePool) -> CommandResult<SupervisorStatus> {
    let row = sqlx::query(
        r#"
        select pid, status, updated_at
        from supervisor_state
        where id = 1
        "#,
    )
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;

    let Some(row) = row else {
        return Ok(SupervisorStatus {
            pid: None,
            status: "offline".to_owned(),
            updated_at: None,
        });
    };

    let updated_at: DateTime<Utc> = row.get("updated_at");
    let status = if Utc::now().signed_duration_since(updated_at).num_seconds() > 10 {
        "stale".to_owned()
    } else {
        row.get("status")
    };

    Ok(SupervisorStatus {
        pid: row.get("pid"),
        status,
        updated_at: Some(updated_at),
    })
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
        let exists: Option<Uuid> = sqlx::query_scalar("select id from channels where id = $1")
            .bind(channel_id)
            .fetch_optional(pool)
            .await
            .map_err(to_string)?;
        return exists.ok_or_else(|| format!("channel {channel_id} does not exist"));
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

async fn load_channel_agent_roster(
    pool: &SqlitePool,
    channel_id: Option<Uuid>,
    current_agent_id: Uuid,
) -> CommandResult<Vec<String>> {
    let Some(channel_id) = channel_id else {
        return Ok(vec![]);
    };
    let rows = sqlx::query(
        r#"
        select a.handle, a.display_name, a.runtime, a.model, a.status
        from channels c
        join channel_members cm on cm.channel_id = c.id
        join agents a on a.id = cm.agent_id
        where c.id = $1
          and c.kind = 'channel'
          and a.id <> $2
        order by lower(a.handle)
        "#,
    )
    .bind(channel_id)
    .bind(current_agent_id)
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    Ok(rows
        .into_iter()
        .map(|row| {
            let handle: String = row.get("handle");
            let display_name: String = row.get("display_name");
            let runtime: String = row.get("runtime");
            let model: String = row.get("model");
            let status: String = row.get("status");
            format!("@{handle} - {display_name} - {runtime}/{model} - {status}")
        })
        .collect())
}

async fn run_supervisor() -> CommandResult<()> {
    let database_url = db_url();
    let _supervisor_lock = acquire_supervisor_lock(&database_url)?;
    let pool = db_connect_with_url(&database_url, 5)
        .await
        .map_err(to_string)?;
    migrate(&pool).await.map_err(to_string)?;

    mark_orphaned_agent_runs(&pool).await?;
    recover_supervisor_commands_at_startup(&pool).await?;
    let codex_registry = WarmCodexRegistry::default();
    let claude_registry = WarmClaudeRegistry::default();
    let command_semaphore = Arc::new(Semaphore::new(SUPERVISOR_COMMAND_CONCURRENCY));
    let mut last_command_cleanup = Instant::now() - Duration::from_secs(3600);
    let mut error_backoff = SUPERVISOR_ERROR_BACKOFF_INITIAL;

    loop {
        match run_supervisor_iteration(
            &pool,
            &codex_registry,
            &claude_registry,
            &command_semaphore,
            &mut last_command_cleanup,
        )
        .await
        {
            Ok(processed_command) => {
                error_backoff = SUPERVISOR_ERROR_BACKOFF_INITIAL;
                if processed_command {
                    continue;
                }
                sleep(SUPERVISOR_IDLE_SLEEP).await;
            }
            Err(err) => {
                eprintln!(
                    "Lantor supervisor loop failed; retrying in {:?}: {err}",
                    error_backoff
                );
                sleep(error_backoff).await;
                error_backoff = error_backoff
                    .saturating_mul(2)
                    .min(SUPERVISOR_ERROR_BACKOFF_MAX);
            }
        }
    }
}

async fn run_supervisor_iteration(
    pool: &SqlitePool,
    codex_registry: &WarmCodexRegistry,
    claude_registry: &WarmClaudeRegistry,
    command_semaphore: &Arc<Semaphore>,
    last_command_cleanup: &mut Instant,
) -> CommandResult<bool> {
    write_supervisor_heartbeat(pool).await?;
    if last_command_cleanup.elapsed() >= Duration::from_secs(3600) {
        cleanup_supervisor_commands(pool).await?;
        *last_command_cleanup = Instant::now();
    }
    schedule_queued_work_items(pool, codex_registry).await?;
    let mut processed_command = false;
    loop {
        let Ok(permit) = command_semaphore.clone().try_acquire_owned() else {
            break;
        };
        let Some(command) = claim_next_supervisor_command(pool).await? else {
            drop(permit);
            break;
        };
        processed_command = true;
        let command_id = command.id;
        let pool = pool.clone();
        let codex_registry = codex_registry.clone();
        let claude_registry = claude_registry.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let result =
                process_supervisor_command(&pool, &codex_registry, &claude_registry, command).await;
            if let Err(err) = finish_supervisor_command(&pool, command_id, result.err()).await {
                eprintln!("failed to finish supervisor command {command_id}: {err}");
            }
        });
    }

    Ok(processed_command)
}

async fn should_schedule_queued_work_item(
    pool: &SqlitePool,
    registry: &WarmCodexRegistry,
    agent_id: Uuid,
    channel_id: Option<Uuid>,
    thread_root_id: Option<Uuid>,
) -> CommandResult<bool> {
    let runtime = agent_runtime(pool, agent_id).await?;
    if !runtime
        .as_deref()
        .is_some_and(|runtime| runtime.eq_ignore_ascii_case("codex"))
    {
        return Ok(!agent_has_active_or_pending_start(pool, agent_id).await?);
    }

    let Some((active_channel_id, active_thread_root_id, schedule_state)) =
        codex::active_codex_turn_surface(registry, agent_id).await
    else {
        return Ok(true);
    };
    match schedule_state {
        CodexActiveTurnScheduleState::ReadyForSteer => {}
        CodexActiveTurnScheduleState::WaitingForTurnId => return Ok(false),
        CodexActiveTurnScheduleState::StuckBeforeTurnId => {
            codex::reap_stuck_codex_turn(pool, registry, agent_id, "scheduler").await?;
            return Ok(true);
        }
    }
    same_codex_surface(
        pool,
        channel_id,
        thread_root_id,
        active_channel_id,
        active_thread_root_id,
    )
    .await
}

async fn schedule_queued_work_items(
    pool: &SqlitePool,
    registry: &WarmCodexRegistry,
) -> CommandResult<()> {
    let rows = sqlx::query(
        r#"
        select w.id, w.agent_id, w.channel_id, w.thread_root_id
        from agent_work_items w
        where w.status = 'queued'
          and not exists (
              select 1
              from supervisor_commands c
              where c.command_type = 'start_agent'
                and c.work_item_id = w.id
                and c.status in ('pending', 'running')
          )
        order by w.created_at asc
        limit 16
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    for row in rows {
        let work_item_id: Uuid = row.get("id");
        let agent_id: Uuid = row.get("agent_id");
        let channel_id: Option<Uuid> = row.get("channel_id");
        let thread_root_id: Option<Uuid> = row.get("thread_root_id");
        if !should_schedule_queued_work_item(pool, registry, agent_id, channel_id, thread_root_id)
            .await?
        {
            continue;
        }
        if enqueue_agent_work_if_available(pool, agent_id, work_item_id).await? {
            record_agent_activity(
                pool,
                Some(agent_id),
                None,
                "dispatch",
                "Backlog agent request scheduled",
                work_item_id.to_string(),
            )
            .await?;
        }
    }

    Ok(())
}

async fn process_supervisor_command(
    pool: &SqlitePool,
    codex_registry: &WarmCodexRegistry,
    claude_registry: &WarmClaudeRegistry,
    command: SupervisorCommand,
) -> CommandResult<()> {
    match command.command_type.as_str() {
        "start_agent" => {
            let Some(agent_id) = command.agent_id else {
                return Err("start_agent command missing agent_id".to_owned());
            };
            supervisor_start_agent(
                pool,
                codex_registry,
                claude_registry,
                agent_id,
                command.work_item_id,
            )
            .await
        }
        "stop_run" => {
            let Some(run_id) = command.run_id else {
                return Err("stop_run command missing run_id".to_owned());
            };
            supervisor_stop_run(pool, codex_registry, run_id).await
        }
        other => Err(format!("unknown supervisor command: {other}")),
    }
}

async fn supervisor_start_agent(
    pool: &SqlitePool,
    codex_registry: &WarmCodexRegistry,
    claude_registry: &WarmClaudeRegistry,
    agent_id: Uuid,
    work_item_id: Option<Uuid>,
) -> CommandResult<()> {
    if let Some(work_item_id) = work_item_id {
        let status: Option<String> =
            sqlx::query_scalar("select status from agent_work_items where id = $1")
                .bind(work_item_id)
                .fetch_optional(pool)
                .await
                .map_err(to_string)?;
        if status.as_deref() == Some("cancelled") {
            record_agent_activity(
                pool,
                Some(agent_id),
                None,
                "dispatch",
                "Cancelled agent request skipped",
                work_item_id.to_string(),
            )
            .await?;
            return Ok(());
        }
    }

    if let Some(reason) = agent_budget_exhausted(pool, agent_id).await? {
        if let Some(work_item_id) = work_item_id {
            sqlx::query(
                r#"
                update agent_work_items
                set status = 'failed',
                    completed_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
                    updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
                where id = $1
                "#,
            )
            .bind(work_item_id)
            .execute(pool)
            .await
            .map_err(to_string)?;
            notify_ui_work_item_changed(pool, work_item_id, "budget_exhausted").await;
        }
        record_agent_activity(
            pool,
            Some(agent_id),
            None,
            "usage",
            "Budget reached",
            reason,
        )
        .await?;
        return Ok(());
    }

    let row = sqlx::query(
        r#"
        select handle, runtime, model, reasoning_effort, service_tier, launch_command, working_directory, avatar
        from agents
        where id = $1
        "#,
    )
    .bind(agent_id)
    .fetch_one(pool)
    .await
    .map_err(to_string)?;

    let handle: String = row.get("handle");
    let runtime: String = row.get("runtime");
    let model: String = row.get("model");
    let reasoning_effort: String = row.get("reasoning_effort");
    let service_tier: String = row.get("service_tier");
    let launch_command: String = row.get("launch_command");
    let working_directory: String = row.get::<String, _>("working_directory").trim().to_owned();
    let avatar: Option<String> = row.get("avatar");
    let is_warm_streaming_runtime =
        runtime.eq_ignore_ascii_case("codex") || runtime.eq_ignore_ascii_case("claude");
    let memory_context = match load_agent_memory_context(&working_directory) {
        Ok(memory_context) => memory_context,
        Err(err) => {
            record_agent_activity(
                pool,
                Some(agent_id),
                None,
                "profile",
                "Memory context skipped",
                err,
            )
            .await?;
            None
        }
    };
    let work_item_prompt = match work_item_id {
        Some(work_item_id) => {
            let row = sqlx::query(
                r#"
                select
                    w.channel_id,
                    w.title,
                    w.context,
                    c.name as channel_name,
                    t.number as task_number,
                    w.thread_root_id
                from agent_work_items w
                left join channels c on c.id = w.channel_id
                left join tasks t on t.id = w.task_id
                where w.id = $1 and w.agent_id = $2
                "#,
            )
            .bind(work_item_id)
            .bind(agent_id)
            .fetch_one(pool)
            .await
            .map_err(to_string)?;
            let title: String = row.get("title");
            let context: String = row.get("context");
            let channel_id: Option<Uuid> = row.get("channel_id");
            let channel_name: Option<String> = row.get("channel_name");
            let task_number: Option<i64> = row.get("task_number");
            let thread_root_id: Option<Uuid> = row.get("thread_root_id");
            let context = if runtime.eq_ignore_ascii_case("claude") {
                append_claude_thread_context(
                    pool,
                    &context,
                    channel_id,
                    channel_name.as_deref(),
                    thread_root_id,
                )
                .await?
            } else {
                context
            };
            let available_agents = load_channel_agent_roster(pool, channel_id, agent_id).await?;
            let agent_profile_hint = avatar
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_none()
                .then(|| {
                    format!(
                        "Your agent profile currently has no avatar. If your handle or MEMORY.md gives you a stable identity, you may emit one standalone LANTOR_EVENT profile_update with an avatar like `dicebear:dylan:{handle}`. Keep handling the user's request normally and do not send visible chat only for avatar setup."
                    )
                });
            if is_warm_streaming_runtime {
                build_streaming_work_item_prompt(
                    work_item_id,
                    &title,
                    &context,
                    channel_name.as_deref(),
                    task_number,
                    thread_root_id,
                    &available_agents,
                    agent_profile_hint.as_deref(),
                )
            } else {
                build_work_item_prompt(
                    work_item_id,
                    &title,
                    &context,
                    channel_name.as_deref(),
                    task_number,
                    thread_root_id,
                    &available_agents,
                    agent_profile_hint.as_deref(),
                )
            }
        }
        None => String::new(),
    };
    if runtime.eq_ignore_ascii_case("codex") {
        return codex::supervisor_start_codex_streaming_agent(
            pool,
            codex_registry,
            agent_id,
            work_item_id,
            handle,
            model,
            reasoning_effort,
            service_tier,
            working_directory,
            work_item_prompt,
            memory_context,
        )
        .await;
    }
    if runtime.eq_ignore_ascii_case("claude") {
        return claude::supervisor_start_claude_streaming_agent(
            pool,
            claude_registry,
            agent_id,
            work_item_id,
            handle,
            model,
            working_directory,
            work_item_prompt,
            memory_context,
        )
        .await;
    }
    let work_item_prompt = prepend_memory_context(work_item_prompt, memory_context.as_deref());
    let command_text = effective_launch_command(launch_command, runtime, model, handle.clone());
    start_process_agent(
        pool,
        ProcessAgentLaunch {
            agent_id,
            work_item_id,
            handle,
            working_directory,
            command_text,
            work_item_prompt,
        },
    )
    .await
}

async fn supervisor_stop_run(
    pool: &SqlitePool,
    codex_registry: &WarmCodexRegistry,
    run_id: Uuid,
) -> CommandResult<()> {
    let row = sqlx::query(
        r#"
        select r.agent_id, r.pid, r.work_item_id, a.runtime
        from agent_runs r
        join agents a on a.id = r.agent_id
        where r.id = $1 and r.stopped_at is null
        "#,
    )
    .bind(run_id)
    .fetch_one(pool)
    .await
    .map_err(to_string)?;

    let agent_id: Uuid = row.get("agent_id");
    let pid: Option<i32> = row.get("pid");
    let work_item_id: Option<Uuid> = row.get("work_item_id");
    let runtime: String = row.get("runtime");
    let Some(pid) = pid else {
        return Err("agent run does not have a pid yet".to_owned());
    };

    sqlx::query("update agent_runs set status = 'stopping' where id = $1")
        .bind(run_id)
        .execute(pool)
        .await
        .map_err(to_string)?;
    notify_ui_agent_run_changed(pool, run_id, "run_stopping").await;
    sqlx::query("update agents set status = 'stopping' where id = $1")
        .bind(agent_id)
        .execute(pool)
        .await
        .map_err(to_string)?;
    if let Some(work_item_id) = work_item_id {
        sqlx::query(
            r#"
            update agent_work_items
            set status = 'cancelling',
                updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
            where id = $1 and status in ('queued', 'running')
            "#,
        )
        .bind(work_item_id)
        .execute(pool)
        .await
        .map_err(to_string)?;
        notify_ui_work_item_changed(pool, work_item_id, "work_item_cancelling").await;
    }

    if runtime.eq_ignore_ascii_case("codex")
        && codex::interrupt_warm_codex_run(pool, codex_registry, agent_id, run_id).await?
    {
        record_agent_activity(
            pool,
            Some(agent_id),
            Some(run_id),
            "run",
            "Stop requested",
            "Codex accepted stop request",
        )
        .await?;
        return Ok(());
    }

    terminate_process_group(pid).await?;

    append_run_log(
        pool,
        run_id,
        format!("sent SIGTERM to process group {pid}\n"),
    )
    .await?;
    record_agent_activity(
        pool,
        Some(agent_id),
        Some(run_id),
        "run",
        "Stop requested",
        format!("process_group={pid}"),
    )
    .await?;
    Ok(())
}

pub(crate) fn to_string(error: impl std::fmt::Display) -> String {
    error.to_string()
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
        .manage(AppState {
            pool,
            db_url: state_db_url,
        })
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

#[cfg(test)]
mod tests {
    use crate::agent_memory::{format_memory_index_entry, insert_memory_index_entry};
    use crate::message_store::{insert_agent_message, load_messages, send_owner_message_in_pool};
    use crate::prompts::{build_codex_streaming_prompt, claude_system_prompt};
    use crate::runtime::{
        process::{
            classify_agent_output_activity, load_runtime_thread_id, upsert_runtime_thread_id,
        },
        streaming::{
            adopt_streaming_agent_message_key, append_streaming_agent_message,
            consume_streaming_agent_control_lines, ensure_streaming_agent_message,
            finish_streaming_agent_message, maybe_hide_silent_streaming_reply,
            streaming_message_body_is_empty,
        },
    };
    use crate::ui_notifications::notify_ui_work_item_changed;

    use super::{
        activity_status, append_claude_thread_context, build_steer_followup_prompt,
        build_streaming_work_item_prompt, build_work_item_prompt, claim_agent_event,
        context_tool::{
            agent_context_agent_inspect, agent_context_artifact_read_in_pool,
            agent_context_attachment_info, agent_context_history_read, agent_context_inbox_archive,
            agent_context_inbox_list, agent_context_inbox_read, agent_context_memory_read,
            agent_context_message_search, agent_context_workspace_info,
            agent_context_workspace_list, short_id,
        },
        create_agent_inbox_item, db_connect_with_url,
        domain::reminders::load_reminders,
        ensure_agent_workspace, extract_agent_event_json, extract_agent_mentions,
        handle_agent_event, inbox_wake_context, load_agent_memory_context,
        load_channel_agent_roster, migrate, normalize_open_link_target, open_dm_with_agent_in_pool,
        queue_mentions_as_work_items, record_agent_activity, same_codex_surface,
        try_claim_unassigned_task, upsert_agent_thread_subscription, AgentAttachmentFile,
        AgentEvent, AgentInboxItemInput, InboxWakeItem, InboxWakeSummary, MentionDispatchOrigin,
        AGENT_MEMORY_CONTEXT_LIMIT, WORK_ITEM_FINISH_PROMPT,
    };
    use chrono::{Duration as ChronoDuration, Utc};
    use serde_json::{json, Value};
    use sqlx::{Row, SqlitePool};
    use std::fs as std_fs;
    use uuid::Uuid;

    #[test]
    fn extracts_unique_agent_mentions() {
        let mentions = extract_agent_mentions("ping @Hancock and @agent-2, then @Hancock again");
        assert_eq!(mentions, vec!["Hancock", "agent-2"]);
    }

    #[test]
    fn extracts_mentions_after_non_ascii_text_and_punctuation() {
        let mentions =
            extract_agent_mentions("请@agent看一下，或者（@reviewer）再看 end.@observer");
        assert_eq!(mentions, vec!["agent", "reviewer", "observer"]);
    }

    #[test]
    fn ignores_empty_or_email_like_at_signs() {
        let mentions = extract_agent_mentions("email a@b.com and a lone @ sign");
        assert!(mentions.is_empty());
    }

    #[test]
    fn open_link_target_normalization_allows_safe_schemes() {
        assert_eq!(
            normalize_open_link_target(" https://example.com/path?q=1 "),
            Some("https://example.com/path?q=1".to_owned())
        );
        assert_eq!(
            normalize_open_link_target("mailto:hello@example.com"),
            Some("mailto:hello@example.com".to_owned())
        );
        assert_eq!(
            normalize_open_link_target("file:///tmp/report.txt"),
            Some("file:///tmp/report.txt".to_owned())
        );
        assert!(normalize_open_link_target("javascript:alert(1)").is_none());
        assert!(normalize_open_link_target("https://example.com/\nopen").is_none());
    }

    #[test]
    fn open_link_target_normalization_allows_existing_local_paths_with_line_suffixes() {
        let dir = std::env::temp_dir().join(format!("lantor-link-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();
        let file = file.to_string_lossy().to_string();

        assert_eq!(normalize_open_link_target(&file), Some(file.clone()));
        assert_eq!(
            normalize_open_link_target(&format!("{file}:42")),
            Some(file.clone())
        );
        assert_eq!(
            normalize_open_link_target(&format!("{file}#L42")),
            Some(file.clone())
        );
        assert!(normalize_open_link_target("/definitely/not/a/lantor/file.rs:1").is_none());

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn memory_context_is_bounded_and_preserves_tail() {
        let dir = std::env::temp_dir().join(format!("lantor-memory-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp memory dir");
        let memory_path = dir.join("MEMORY.md");
        let memory = format!(
            "# Agent\n\n{}\n\n## Active Context\nimportant tail survives",
            "context ".repeat(AGENT_MEMORY_CONTEXT_LIMIT)
        );
        std::fs::write(&memory_path, memory).expect("write memory");

        let context =
            load_agent_memory_context(dir.to_str().expect("utf8 temp dir")).expect("load memory");
        std::fs::remove_dir_all(&dir).ok();

        let context = context.expect("memory should load");
        assert!(context.contains("Lantor omitted"));
        assert!(context.contains("important tail survives"));
        assert!(context.chars().count() < AGENT_MEMORY_CONTEXT_LIMIT + 1_000);
    }

    #[test]
    fn runtime_standing_prompt_carries_memory_once() {
        let prompt =
            claude_system_prompt("tester", Some("Persistent memory: prefer concise replies"));
        assert!(prompt.contains("one warm runtime session per agent"));
        assert!(prompt.contains("channel and thread are delivered as message envelope fields"));
        assert!(prompt.contains("Treat messages as conversation"));
        assert!(prompt.contains("Activity events are the short progress notes"));
        assert!(prompt.contains("MEMORY.md is the compact index"));
        assert!(prompt.contains("raw conversation/tool logs should stay out of memory"));
        assert!(prompt.contains("notes/<topic>.md"));
        assert!(prompt.contains("not replay past turns"));
        assert!(prompt.contains("timestamp-log-like"));
        assert!(prompt.contains("stable user preferences"));
        assert!(prompt.contains("Before long-running work, update Active Context"));
        assert!(prompt.contains("Turn startup sequence:"));
        assert!(
            prompt.contains("Use history-read or message-search when older channel/thread context")
        );
        assert!(prompt.contains("Reply briefly to direct greetings"));
        assert!(prompt.contains("Agent context tools"));
        assert!(prompt.contains("inbox-list"));
        assert!(prompt.contains("[target=... msg=... time=... type=...]"));
        assert!(prompt.contains("Live inbox delivery"));
        assert!(prompt.contains("Persistent memory: prefer concise replies"));
    }

    #[test]
    fn ensure_agent_workspace_creates_index_memory_template_and_notes_dir() {
        let dir = std::env::temp_dir().join(format!("lantor-memory-template-{}", Uuid::new_v4()));
        ensure_agent_workspace(dir.to_str().expect("utf8 temp dir"), "template-agent")
            .expect("ensure workspace");

        let memory = std::fs::read_to_string(dir.join("MEMORY.md")).expect("read memory");
        assert!(dir.join("notes").is_dir());
        assert!(memory.contains("# @template-agent"));
        assert!(memory.contains("## Key Knowledge"));
        assert!(memory.contains("## Memory Map"));
        assert!(memory.contains("notes/user-preferences.md"));
        assert!(memory.contains("notes/work-log.md"));
        assert!(memory.contains("## Active Context"));
        assert!(memory.contains("Keep this file concise and index-like"));
        assert!(memory.contains("Do not use MEMORY.md as a chronological log"));

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn memory_append_can_add_work_log_link_without_timestamp_log() {
        let memory = "# @agent\n\n## Role\nLantor agent.\n\n## Key Knowledge\n- Add stable facts and links that help a restarted agent recover quickly.\n\n## Active Context\n- Currently working on: none.";

        let updated = insert_memory_index_entry(
            memory,
            &format_memory_index_entry("`notes/work-log.md` - staged durable updates."),
        );

        assert!(
            updated.contains("## Key Knowledge\n- `notes/work-log.md` - staged durable updates.")
        );
        assert!(updated.contains("\n## Active Context"));
        assert!(!updated.contains("Memory update"));
        assert!(!updated.contains("Add stable facts and links"));
    }

    #[test]
    fn streaming_prompt_replaces_stdout_finish_contract() {
        let prompt = build_work_item_prompt(
            Uuid::nil(),
            "Review the change",
            "Latest user message: please review",
            Some("lantor"),
            None,
            Some(Uuid::nil()),
            &[],
            None,
        );
        assert!(prompt.contains("Treat messages as conversation"));
        assert!(prompt.contains(WORK_ITEM_FINISH_PROMPT));

        let streaming = build_codex_streaming_prompt(&prompt);
        assert!(streaming.contains("will stream your Codex assistant text"));
        assert!(streaming.contains("Reply briefly to direct greetings"));
        assert!(streaming.contains("pure acknowledgement"));
        assert!(streaming.contains("you may emit standalone LANTOR_EVENT control lines"));
        assert!(streaming.contains("Activity progress: before your final reply"));
        assert!(streaming.contains("activity is not only for reasoning"));
        assert!(streaming.contains("what you are doing or what you just learned"));
        assert!(streaming.contains("artifact_create"));
        assert!(streaming.contains("attachment_create"));
        assert!(streaming.contains("channel_message_create"));
        assert!(streaming.contains("handoff_create"));
        assert!(streaming.contains("task_handoff"));
        assert!(streaming.contains("task_claim"));
        assert!(streaming.contains("Do not narrate every intermediate step in chat"));
        assert!(!streaming.contains(WORK_ITEM_FINISH_PROMPT));
    }

    #[test]
    fn streaming_work_item_prompt_omits_repeated_standing_context() {
        let prompt = build_streaming_work_item_prompt(
            Uuid::nil(),
            "Review the change",
            "Latest user message: please review",
            Some("lantor"),
            None,
            Some(Uuid::nil()),
            &[],
            None,
        );

        assert!(prompt.contains("Standing instructions are already installed"));
        assert!(prompt.contains("authoritative over older warm-runtime context"));
        assert!(prompt.contains("Same-channel/thread follow-ups may be delivered"));
        assert!(prompt.contains("Latest user message: please review"));
        assert!(prompt.contains(WORK_ITEM_FINISH_PROMPT));
        assert!(!prompt.contains("Operating policy:"));
        assert!(!prompt.contains("Agent context tools:"));
        assert!(!prompt.contains("Standalone LANTOR_EVENT control lines:"));
    }

    #[test]
    fn work_item_prompt_includes_agent_profile_hint_when_present() {
        let prompt = build_work_item_prompt(
            Uuid::nil(),
            "Handle inbox",
            "Latest user message: hello",
            Some("lantor"),
            None,
            Some(Uuid::nil()),
            &[],
            Some("Pick a stable DiceBear avatar if the profile is empty."),
        );

        assert!(prompt.contains("agent_profile_hint:"));
        assert!(prompt.contains("Pick a stable DiceBear avatar if the profile is empty."));
    }

    #[test]
    fn inbox_wake_context_includes_message_headers_and_other_active_summary() {
        let channel_id = Uuid::new_v4();
        let thread_root_id = Uuid::new_v4();
        let source_message_id = Uuid::new_v4();
        let inbox_id = Uuid::new_v4();
        let context = inbox_wake_context(
            &[InboxWakeItem {
                id: inbox_id,
                channel_id: Some(channel_id),
                channel_name: Some("support".to_owned()),
                channel_kind: Some("channel".to_owned()),
                thread_root_id: Some(thread_root_id),
                source_message_id: Some(source_message_id),
                task_id: None,
                kind: "owner_thread_followup".to_owned(),
                priority: 90,
                title: "Handle follow-up".to_owned(),
                body_preview: "please use the latest numbers\nand reply directly".to_owned(),
                message_created_at: Some(Utc::now()),
                sender_name: Some("Dylan".to_owned()),
                sender_role: Some("owner".to_owned()),
            }],
            &[InboxWakeSummary {
                target: "dm:Hancock".to_owned(),
                count: 2,
            }],
        );

        assert!(context.contains("[target=#support:"));
        assert!(context.contains(&format!("msg={}", short_id(source_message_id))));
        assert!(context.contains("type=owner"));
        assert!(context.contains("Dylan: please use the latest numbers and reply directly"));
        assert!(context.contains("Warm-runtime guard"));
        assert!(context.contains("use history-read on the default reply target"));
        assert!(context.contains(&format!("inbox_id: {inbox_id}")));
        assert!(context.contains("Other active inbox targets:"));
        assert!(context.contains("- dm:Hancock: 2 active"));
    }

    #[test]
    fn inbox_wake_context_tells_task_available_agents_to_claim_silently() {
        let context = inbox_wake_context(
            &[InboxWakeItem {
                id: Uuid::new_v4(),
                channel_id: Some(Uuid::new_v4()),
                channel_name: Some("builders".to_owned()),
                channel_kind: Some("channel".to_owned()),
                thread_root_id: Some(Uuid::new_v4()),
                source_message_id: Some(Uuid::new_v4()),
                task_id: Some(Uuid::new_v4()),
                kind: "task_available".to_owned(),
                priority: 70,
                title: "Implement queue behavior".to_owned(),
                body_preview: "Implement queue behavior".to_owned(),
                message_created_at: Some(Utc::now()),
                sender_name: Some("Dylan".to_owned()),
                sender_role: Some("owner".to_owned()),
            }],
            &[],
        );

        assert!(context.contains("Task claim opportunity mode:"));
        assert!(context.contains("competitive, unassigned task opportunity"));
        assert!(context.contains(r#"LANTOR_EVENT {"type":"task_claim","task_number":...}"#));
        assert!(context.contains("LANTOR_SILENT_REPLY: claim attempted"));
        assert!(context.contains("do not emit activity events"));
        assert!(context.contains("task_assigned inbox turn"));
    }

    #[test]
    fn steer_followup_prompt_uses_compact_inbox_headers() {
        let thread_root_id = Uuid::new_v4();
        let source_message_id = Uuid::new_v4();
        let inbox_id = Uuid::new_v4();
        let prompt = build_steer_followup_prompt(&[InboxWakeItem {
            id: inbox_id,
            channel_id: Some(Uuid::new_v4()),
            channel_name: Some("support".to_owned()),
            channel_kind: Some("channel".to_owned()),
            thread_root_id: Some(thread_root_id),
            source_message_id: Some(source_message_id),
            task_id: None,
            kind: "owner_thread_followup".to_owned(),
            priority: 90,
            title: "Handle follow-up".to_owned(),
            body_preview: "please use the latest numbers\nand reply directly".to_owned(),
            message_created_at: Some(Utc::now()),
            sender_name: Some("Dylan".to_owned()),
            sender_role: Some("owner".to_owned()),
        }]);

        assert!(prompt.contains("Same-channel/thread live inbox follow-up."));
        assert!(prompt.contains("Default reply target for normal assistant text: #support:"));
        assert!(prompt.contains(&format!("msg={}", short_id(source_message_id))));
        assert!(prompt.contains(&format!("inbox_id: {inbox_id}")));
        assert!(prompt.contains("archived automatically"));
        assert!(!prompt.contains("inbox-archive --inbox-id <id>"));
        assert!(!prompt.contains("Current Lantor inbox processing turn:"));
        assert!(!prompt.contains("title: Handle follow-up"));
        assert!(!prompt.contains("kind: owner_thread_followup"));
        assert!(!prompt.contains(WORK_ITEM_FINISH_PROMPT));
    }

    #[tokio::test]
    async fn claude_thread_context_injects_only_current_thread_messages() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let channel_id = insert_test_channel(&pool, "claude-context").await?;
            let thread_a: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'thread A root', false)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let thread_b: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'thread B root with forbidden bleed', false)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            sqlx::query(
                r#"
                insert into messages (channel_id, thread_root_id, sender_name, sender_role, body, is_task)
                values ($1, $2, 'Agent', 'agent', 'thread A reply evidence', false),
                       ($1, $3, 'Agent', 'agent', 'thread B reply forbidden bleed', false)
                "#,
            )
            .bind(channel_id)
            .bind(thread_a)
            .bind(thread_b)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let context = append_claude_thread_context(
                &pool,
                "Default inbox item: summarize",
                Some(channel_id),
                Some("claude-context"),
                Some(thread_a),
            )
            .await?;
            assert!(context.contains("Same-thread recent context"));
            assert!(context.contains("Resolve contextual follow-ups from this block"));
            assert!(context.contains("thread A root"));
            assert!(context.contains("thread A reply evidence"));
            assert!(!context.contains("thread B root with forbidden bleed"));
            assert!(!context.contains("thread B reply forbidden bleed"));
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn agent_context_tool_reads_thread_history_and_searches_messages() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let channel_id = insert_test_channel(&pool, "context-tools").await?;
            let root_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'root message with needle', false)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            sqlx::query(
                r#"
                insert into messages (channel_id, thread_root_id, sender_name, sender_role, body, is_task)
                values ($1, $2, 'agent', 'agent', 'reply inside thread', false),
                       ($1, null, 'Dylan', 'owner', 'separate root message', false)
                "#,
            )
            .bind(channel_id)
            .bind(root_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let history_args = vec![
                "history-read".to_owned(),
                "--target".to_owned(),
                format!("#context-tools:{}", short_id(root_id)),
                "--limit".to_owned(),
                "10".to_owned(),
            ];
            let history = agent_context_history_read(&pool, &history_args).await?;
            assert!(history.contains("root message with needle"));
            assert!(history.contains("reply inside thread"));
            assert!(history.contains("[target=#context-tools:"));
            assert!(history.contains(" type=owner] Dylan: root message with needle"));
            assert!(!history.contains("separate root message"));

            let search_args = vec![
                "message-search".to_owned(),
                "--query".to_owned(),
                "needle".to_owned(),
                "--target".to_owned(),
                "#context-tools".to_owned(),
            ];
            let search = agent_context_message_search(&pool, &search_args).await?;
            assert!(search.contains("root message with needle"));
            assert!(search.contains("[target=#context-tools msg="));
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn agent_context_tool_reads_workspace_and_memory() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let workspace =
            std::env::temp_dir().join(format!("lantor-workspace-tool-{}", Uuid::new_v4()));
        let result: Result<(), String> = async {
            std::fs::create_dir_all(workspace.join("notes")).map_err(|err| err.to_string())?;
            std::fs::write(
                workspace.join("MEMORY.md"),
                "# @workspace-agent\n\n## Role\nWorkspace-aware test agent.\n",
            )
            .map_err(|err| err.to_string())?;
            std::fs::write(workspace.join("notes").join("handoff.md"), "handoff note")
                .map_err(|err| err.to_string())?;

            let agent_id = insert_test_agent(&pool, "workspace-agent").await?;
            sqlx::query("update agents set working_directory = $2 where id = $1")
                .bind(agent_id)
                .bind(workspace.to_string_lossy().to_string())
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;

            let target_args = vec![
                "workspace-info".to_owned(),
                "--target".to_owned(),
                "@workspace-agent".to_owned(),
            ];
            let workspace_info = agent_context_workspace_info(&pool, &target_args).await?;
            assert!(workspace_info.contains("Lantor workspace for @workspace-agent"));
            assert!(workspace_info.contains("memory_exists=true"));
            assert!(workspace_info.contains("MEMORY.md"));

            let memory_args = vec![
                "memory-read".to_owned(),
                "--target".to_owned(),
                "@workspace-agent".to_owned(),
            ];
            let memory = agent_context_memory_read(&pool, &memory_args).await?;
            assert!(memory.contains("Workspace-aware test agent"));

            let list_args = vec![
                "workspace-list".to_owned(),
                "--target".to_owned(),
                "@workspace-agent".to_owned(),
                "--max-depth".to_owned(),
                "2".to_owned(),
            ];
            let listing = agent_context_workspace_list(&pool, &list_args).await?;
            assert!(listing.contains("notes/"));
            assert!(listing.contains("notes/handoff.md"));
            Ok(())
        }
        .await;
        let _ = std::fs::remove_dir_all(&workspace);
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn artifact_create_event_persists_artifact_and_message_card() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "artifact-agent").await?;
            let channel_id = insert_test_channel(&pool, "artifacts").await?;
            let run_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_runs (agent_id, command, status)
                values ($1, 'codex app-server', 'running')
                returning id
                "#,
            )
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            handle_agent_event(
                &pool,
                agent_id,
                run_id,
                AgentEvent::ArtifactCreate {
                    channel: None,
                    channel_id: Some(channel_id),
                    thread_root_id: None,
                    kind: "markdown".to_owned(),
                    title: "Review report".to_owned(),
                    summary: Some("Two findings and one follow-up.".to_owned()),
                    content: "# Review report\n\n- finding".to_owned(),
                    metadata: Some(json!({"source": "test"})),
                },
            )
            .await?;

            let artifact = sqlx::query(
                r#"
                select id, message_id, kind, title, summary, content, metadata
                from artifacts
                where channel_id = $1
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let artifact_id: Uuid = artifact.get("id");
            let message_id: Uuid = artifact.get("message_id");
            assert_eq!(artifact.get::<String, _>("kind"), "markdown");
            assert_eq!(artifact.get::<String, _>("title"), "Review report");
            assert_eq!(
                artifact.get::<String, _>("summary"),
                "Two findings and one follow-up."
            );

            let messages = load_messages(&pool).await?;
            let message = messages
                .iter()
                .find(|message| message.id == message_id)
                .expect("artifact message should be loaded");
            assert!(message.body.contains("Created artifact: Review report"));
            assert_eq!(message.artifacts.len(), 1);
            assert_eq!(message.artifacts[0].id, artifact_id);

            let tool_output = agent_context_artifact_read_in_pool(
                &pool,
                &[
                    "artifact-read".to_owned(),
                    "--artifact-id".to_owned(),
                    artifact_id.to_string(),
                ],
            )
            .await?;
            assert!(tool_output.contains("kind=markdown"));
            assert!(tool_output.contains("# Review report"));
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn attachment_create_event_imports_local_file_as_message_attachment() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "attachment-agent").await?;
            let channel_id = insert_test_channel(&pool, "attachments").await?;
            let run_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_runs (agent_id, command, status)
                values ($1, 'codex app-server', 'running')
                returning id
                "#,
            )
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let dir =
                std::env::temp_dir().join(format!("lantor-attachment-test-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
            let source_path = dir.join("generated.png");
            let source_bytes = b"\x89PNG\r\n\x1a\nlantor-test-image";
            std::fs::write(&source_path, source_bytes).map_err(|err| err.to_string())?;

            handle_agent_event(
                &pool,
                agent_id,
                run_id,
                AgentEvent::AttachmentCreate {
                    channel: None,
                    channel_id: Some(channel_id),
                    thread_root_id: None,
                    body: Some("Generated architecture diagram".to_owned()),
                    files: vec![AgentAttachmentFile {
                        path: source_path.to_string_lossy().to_string(),
                        name: Some("architecture.png".to_owned()),
                        mime_type: Some("image/png".to_owned()),
                    }],
                },
            )
            .await?;

            let messages = load_messages(&pool).await?;
            let message = messages
                .iter()
                .find(|message| message.body == "Generated architecture diagram")
                .expect("attachment message should be loaded");
            assert_eq!(message.attachments.len(), 1);
            let attachment = &message.attachments[0];
            assert_eq!(attachment.original_name, "architecture.png");
            assert_eq!(attachment.mime_type, "image/png");
            assert_eq!(attachment.size_bytes, source_bytes.len() as i64);
            assert_ne!(
                attachment.storage_path.as_str(),
                source_path.to_string_lossy().as_ref()
            );
            assert_eq!(
                std::fs::read(&attachment.storage_path).map_err(|err| err.to_string())?,
                source_bytes
            );

            let stored_path = std::path::PathBuf::from(&attachment.storage_path);
            let _ = std::fs::remove_file(&stored_path);
            if let Some(parent) = stored_path.parent() {
                let _ = std::fs::remove_dir(parent);
            }
            let _ = std::fs::remove_dir_all(&dir);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn handoff_create_dispatches_target_agent_to_existing_thread() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let source_agent_id = insert_test_agent(&pool, "source").await?;
            let target_agent_id = insert_test_agent(&pool, "target").await?;
            let channel_id = insert_test_channel(&pool, "handoff").await?;
            sqlx::query(
                r#"
                insert into channel_members (channel_id, agent_id)
                values ($1, $2)
                "#,
            )
            .bind(channel_id)
            .bind(source_agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let root_message_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'Please investigate this thread', false)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let run_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_runs (agent_id, command, status)
                values ($1, 'codex app-server', 'running')
                returning id
                "#,
            )
            .bind(source_agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            handle_agent_event(
                &pool,
                source_agent_id,
                run_id,
                AgentEvent::HandoffCreate {
                    target_agent: "@target".to_owned(),
                    channel: None,
                    channel_id: Some(channel_id),
                    thread_root_id: root_message_id,
                    reason: Some("User asked target to continue".to_owned()),
                    body: "@target Please continue the implementation from this thread.".to_owned(),
                },
            )
            .await?;

            let handoff_message = sqlx::query(
                r#"
                select id, sender_agent_id, sender_name, sender_role, body
                from messages
                where channel_id = $1
                  and thread_root_id = $2
                  and sender_agent_id = $3
                  and sender_role = 'agent'
                "#,
            )
            .bind(channel_id)
            .bind(root_message_id)
            .bind(source_agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let handoff_message_id: Uuid = handoff_message.get("id");
            let handoff_body: String = handoff_message.get("body");
            assert_eq!(handoff_message.get::<String, _>("sender_name"), "source");
            assert_eq!(
                handoff_body,
                "@target Please continue the implementation from this thread."
            );
            assert!(!handoff_body.contains("Reason:"));

            let target_is_member: bool = sqlx::query_scalar(
                r#"
                select exists (
                    select 1 from channel_members
                    where channel_id = $1 and agent_id = $2
                )
                "#,
            )
            .bind(channel_id)
            .bind(target_agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert!(target_is_member);

            let work_item = sqlx::query(
                r#"
                select
                    w.agent_id,
                    w.thread_root_id,
                    w.source_message_id,
                    w.source_kind,
                    w.title,
                    w.context,
                    w.status,
                    i.kind as inbox_kind,
                    i.state as inbox_state
                from agent_work_items w
                join agent_inbox_items i on i.work_item_id = w.id
                where w.source_message_id = $1
                "#,
            )
            .bind(handoff_message_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(work_item.get::<Uuid, _>("agent_id"), target_agent_id);
            assert_eq!(
                work_item.get::<Option<Uuid>, _>("thread_root_id"),
                Some(root_message_id)
            );
            assert_eq!(
                work_item.get::<Option<Uuid>, _>("source_message_id"),
                Some(handoff_message_id)
            );
            assert_eq!(work_item.get::<String, _>("source_kind"), "inbox_wake");
            assert_eq!(work_item.get::<String, _>("status"), "queued");
            assert!(work_item
                .get::<String, _>("context")
                .contains("Lantor agent inbox wake"));
            assert_eq!(work_item.get::<String, _>("inbox_kind"), "handoff");
            assert_eq!(work_item.get::<String, _>("inbox_state"), "processing");
            let target_work_items: i64 =
                sqlx::query_scalar("select count(*) from agent_work_items where agent_id = $1")
                    .bind(target_agent_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(target_work_items, 1);

            let pending_start: i64 = sqlx::query_scalar(
                r#"
                select count(*)
                from supervisor_commands
                where agent_id = $1 and work_item_id in (
                    select id from agent_work_items where source_message_id = $2
                )
                "#,
            )
            .bind(target_agent_id)
            .bind(handoff_message_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(pending_start, 1);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn task_handoff_reassigns_task_and_wakes_new_assignee() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let source_agent_id = insert_test_agent(&pool, "task-source").await?;
            let target_agent_id = insert_test_agent(&pool, "task-target").await?;
            let channel_id = insert_test_channel(&pool, "task-handoff").await?;
            sqlx::query(
                r#"
                insert into channel_members (channel_id, agent_id)
                values ($1, $2)
                "#,
            )
            .bind(channel_id)
            .bind(source_agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let root_message_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'Implement task handoff', true)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let task_row = sqlx::query(
                r#"
                insert into tasks (message_id, channel_id, title, status, assignee_agent_id)
                values ($1, $2, 'Implement task handoff', 'in_progress', $3)
                returning id, number
                "#,
            )
            .bind(root_message_id)
            .bind(channel_id)
            .bind(source_agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let task_id: Uuid = task_row.get("id");
            let task_number: i64 = task_row.get("number");
            let run_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_runs (agent_id, command, status)
                values ($1, 'codex app-server', 'running')
                returning id
                "#,
            )
            .bind(source_agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            sqlx::query(
                r#"
                insert into agent_work_items (
                    agent_id, channel_id, thread_root_id, task_id, source_kind, title, context, status, run_id
                )
                values ($1, $2, $3, $4, 'task', 'Implement task handoff', '', 'running', $5)
                "#,
            )
            .bind(source_agent_id)
            .bind(channel_id)
            .bind(root_message_id)
            .bind(task_id)
            .bind(run_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            handle_agent_event(
                &pool,
                source_agent_id,
                run_id,
                AgentEvent::TaskHandoff {
                    target_agent: "@task-target".to_owned(),
                    task_number: None,
                    reason: "Target owns the UI side".to_owned(),
                    body: Some("@task-target please continue the UI wiring.".to_owned()),
                },
            )
            .await?;

            let task = sqlx::query("select status, assignee_agent_id from tasks where id = $1")
                .bind(task_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
            assert_eq!(task.get::<String, _>("status"), "in_progress");
            assert_eq!(
                task.get::<Option<Uuid>, _>("assignee_agent_id"),
                Some(target_agent_id)
            );

            let handoff_message_body: String = sqlx::query_scalar(
                r#"
                select body
                from messages
                where channel_id = $1
                  and thread_root_id = $2
                  and sender_agent_id = $3
                "#,
            )
            .bind(channel_id)
            .bind(root_message_id)
            .bind(source_agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(
                handoff_message_body,
                "@task-target please continue the UI wiring."
            );

            let inbox = sqlx::query(
                r#"
                select kind, task_id, payload->>'reason' as reason
                from agent_inbox_items
                where agent_id = $1
                order by created_at desc
                limit 1
                "#,
            )
            .bind(target_agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(inbox.get::<String, _>("kind"), "task_assigned");
            assert_eq!(inbox.get::<Option<Uuid>, _>("task_id"), Some(task_id));
            assert_eq!(
                inbox.get::<Option<String>, _>("reason").as_deref(),
                Some("Target owns the UI side")
            );
            let target_is_member: bool = sqlx::query_scalar(
                r#"
                select exists (
                    select 1 from channel_members
                    where channel_id = $1 and agent_id = $2
                )
                "#,
            )
            .bind(channel_id)
            .bind(target_agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert!(target_is_member);
            let target_work_items: i64 = sqlx::query_scalar(
                "select count(*) from agent_work_items where agent_id = $1 and task_id = $2",
            )
            .bind(target_agent_id)
            .bind(task_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(target_work_items, 1);
            let activity_count: i64 = sqlx::query_scalar(
                r#"
                select count(*)
                from agent_activities
                where agent_id = $1
                  and title = $2
                  and metadata->>'target_agent' = '@task-target'
                "#,
            )
            .bind(source_agent_id)
            .bind(format!("Task #{task_number} handed off"))
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(activity_count, 1);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn channel_message_create_posts_agent_message_and_dispatches_mentions() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let source_agent_id = insert_test_agent(&pool, "source").await?;
            let target_agent_id = insert_test_agent(&pool, "target").await?;
            let channel_id = insert_test_channel(&pool, "channel-message").await?;
            sqlx::query(
                r#"
                insert into channel_members (channel_id, agent_id)
                values ($1, $2)
                "#,
            )
            .bind(channel_id)
            .bind(source_agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let run_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_runs (agent_id, command, status)
                values ($1, 'codex app-server', 'running')
                returning id
                "#,
            )
            .bind(source_agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            handle_agent_event(
                &pool,
                source_agent_id,
                run_id,
                AgentEvent::ChannelMessageCreate {
                    channel: None,
                    channel_id: Some(channel_id),
                    thread_root_id: None,
                    body: "@target please start this task in this channel.".to_owned(),
                },
            )
            .await?;

            let message = sqlx::query(
                r#"
                select id, thread_root_id, sender_agent_id, sender_name, sender_role, body, is_task
                from messages
                where channel_id = $1 and sender_agent_id = $2
                "#,
            )
            .bind(channel_id)
            .bind(source_agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let message_id: Uuid = message.get("id");
            assert_eq!(message.get::<Option<Uuid>, _>("thread_root_id"), None);
            assert_eq!(
                message.get::<Option<Uuid>, _>("sender_agent_id"),
                Some(source_agent_id)
            );
            assert_eq!(message.get::<String, _>("sender_name"), "source");
            assert_eq!(message.get::<String, _>("sender_role"), "agent");
            assert_eq!(
                message.get::<String, _>("body"),
                "@target please start this task in this channel."
            );
            assert!(!message.get::<bool, _>("is_task"));

            let target_is_member: bool = sqlx::query_scalar(
                r#"
                select exists (
                    select 1 from channel_members
                    where channel_id = $1 and agent_id = $2
                )
                "#,
            )
            .bind(channel_id)
            .bind(target_agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert!(target_is_member);

            let work_item = sqlx::query(
                r#"
                select w.agent_id, w.thread_root_id, w.source_message_id, w.source_kind, i.kind as inbox_kind
                from agent_work_items w
                join agent_inbox_items i on i.work_item_id = w.id
                where w.source_message_id = $1 and w.agent_id = $2
                "#,
            )
            .bind(message_id)
            .bind(target_agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(work_item.get::<Uuid, _>("agent_id"), target_agent_id);
            assert_eq!(
                work_item.get::<Option<Uuid>, _>("thread_root_id"),
                Some(message_id)
            );
            assert_eq!(
                work_item.get::<Option<Uuid>, _>("source_message_id"),
                Some(message_id)
            );
            assert_eq!(work_item.get::<String, _>("source_kind"), "inbox_wake");
            assert_eq!(work_item.get::<String, _>("inbox_kind"), "collaboration");
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn channel_message_create_requires_source_channel_membership() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> =
            async {
                let source_agent_id = insert_test_agent(&pool, "source").await?;
                let channel_id = insert_test_channel(&pool, "channel-message-denied").await?;
                let run_id: Uuid = sqlx::query_scalar(
                    r#"
                insert into agent_runs (agent_id, command, status)
                values ($1, 'codex app-server', 'running')
                returning id
                "#,
                )
                .bind(source_agent_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;

                let err = handle_agent_event(
                    &pool,
                    source_agent_id,
                    run_id,
                    AgentEvent::ChannelMessageCreate {
                        channel: None,
                        channel_id: Some(channel_id),
                        thread_root_id: None,
                        body: "I should not be posted.".to_owned(),
                    },
                )
                .await
                .expect_err("non-member channel_message_create should be rejected");
                assert!(
                    err.contains("channel_message_create requires source agent channel membership")
                );

                let message_count: i64 =
                    sqlx::query_scalar("select count(*) from messages where channel_id = $1")
                        .bind(channel_id)
                        .fetch_one(&pool)
                        .await
                        .map_err(|err| err.to_string())?;
                assert_eq!(message_count, 0);
                Ok(())
            }
            .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn agent_event_receipts_hash_large_control_events() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "large-event-agent").await?;
            let run_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_runs (agent_id, command, status)
                values ($1, 'codex app-server', 'running')
                returning id
                "#,
            )
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let markdown = format!(
                "# Large report\n\n{}",
                "- large artifact payload\n".repeat(400)
            );
            let event = json!({
                "type": "artifact_create",
                "channel_id": Uuid::new_v4(),
                "kind": "markdown",
                "title": "Large markdown artifact",
                "content": markdown
            })
            .to_string();

            assert!(claim_agent_event(&pool, run_id, &event).await?);
            assert!(!claim_agent_event(&pool, run_id, &event).await?);
            let (count, max_len): (i64, i64) = sqlx::query_as(
                "select count(*), max(length(event_json)) from agent_event_receipts where run_id = $1",
            )
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(count, 1);
            assert!(max_len > 2704);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[test]
    fn extracts_plain_agent_event_lines() {
        assert_eq!(
            extract_agent_event_json(r#"LANTOR_EVENT {"type":"message","body":"ok"}"#),
            Some(r#"{"type":"message","body":"ok"}"#)
        );
    }

    #[test]
    fn extracts_codex_wrapped_agent_event_lines() {
        assert_eq!(
            extract_agent_event_json(
                r#"[stderr] LANTOR_EVENT {"type":"message","body":"from tool output"}"#
            ),
            Some(r#"{"type":"message","body":"from tool output"}"#)
        );
    }

    #[test]
    fn extracts_stdout_wrapped_agent_event_lines() {
        assert_eq!(
            extract_agent_event_json(
                r#"[stdout] LANTOR_EVENT {"type":"message","body":"from final output"}"#
            ),
            Some(r#"{"type":"message","body":"from final output"}"#)
        );
    }

    #[test]
    fn extracts_agent_event_json_before_trailing_text() {
        assert_eq!(
            extract_agent_event_json(
                r#"LANTOR_EVENT {"type":"activity","title":"Done","detail":"ok"} ## Review"#
            ),
            Some(r#"{"type":"activity","title":"Done","detail":"ok"}"#)
        );
    }

    #[test]
    fn extracts_agent_event_json_with_braces_inside_strings() {
        assert_eq!(
            extract_agent_event_json(
                r#"LANTOR_EVENT {"type":"message","body":"text with { braces } and \"quotes\""} trailing"#
            ),
            Some(r#"{"type":"message","body":"text with { braces } and \"quotes\""}"#)
        );
    }

    #[test]
    fn ignores_event_examples_embedded_in_instructions() {
        assert!(extract_agent_event_json(
            r#"[stderr] Reply with: LANTOR_EVENT {"type":"message","body":"..."}"#
        )
        .is_none());
    }

    #[test]
    fn ignores_known_codex_manifest_default_prompt_warning() {
        let line = json!({
            "timestamp": "2026-05-14T13:05:55.340546Z",
            "level": "WARN",
            "fields": {
                "message": "ignoring interface.defaultPrompt: maximum length exceeded"
            },
            "target": "codex_core_plugins::manifest"
        })
        .to_string();

        assert_eq!(classify_agent_output_activity("stderr", &line), None);
        assert_eq!(classify_agent_output_activity("stdout", &line), None);
    }

    #[test]
    fn ignores_known_codex_skill_loader_icon_warning() {
        for message in [
            "ignoring interface.icon_small: icon path must not contain '..'",
            "ignoring interface.icon_large: icon path must not contain '..'",
        ] {
            let line = json!({
                "timestamp": "2026-05-14T13:05:55.340546Z",
                "level": "WARN",
                "fields": {
                    "message": message
                },
                "target": "codex_core_skills::loader"
            })
            .to_string();

            assert_eq!(classify_agent_output_activity("stderr", &line), None);
            assert_eq!(classify_agent_output_activity("stdout", &line), None);
        }
    }

    #[test]
    fn maps_structured_stderr_warning_to_runtime_warning() {
        let line = json!({
            "timestamp": "2026-05-14T13:05:55.340546Z",
            "level": "WARN",
            "fields": {
                "message": "plugin manifest used a deprecated field"
            },
            "target": "codex_core_plugins::manifest"
        })
        .to_string();
        let activity =
            classify_agent_output_activity("stderr", &line).expect("warning should be classified");

        assert_eq!(activity.0, "run");
        assert_eq!(activity.1, "Runtime warning");
        let detail: Value = serde_json::from_str(&activity.2).expect("structured detail");
        assert_eq!(detail["level"], "WARN");
        assert_eq!(detail["target"], "codex_core_plugins::manifest");
        assert_eq!(detail["message"], "plugin manifest used a deprecated field");

        let stdout_activity =
            classify_agent_output_activity("stdout", &line).expect("warning should be classified");
        assert_eq!(stdout_activity.0, "run");
        assert_eq!(stdout_activity.1, "Runtime warning");
    }

    #[test]
    fn ignores_known_codex_legacy_notify_hook_warning() {
        let line = json!({
            "timestamp": "2026-05-14T13:16:30.388210Z",
            "level": "WARN",
            "fields": {
                "error": "No such file or directory (os error 2)",
                "hook_name": "legacy_notify",
                "message": "after_agent hook failed; continuing",
                "turn_id": "019e26a1-7ad5-7642-af8a-e042a0738a84"
            },
            "target": "codex_core::session::turn"
        })
        .to_string();

        assert_eq!(classify_agent_output_activity("stderr", &line), None);
    }

    #[test]
    fn maps_structured_warning_with_error_words_to_runtime_warning() {
        let line = json!({
            "timestamp": "2026-05-14T13:16:30.388210Z",
            "level": "WARN",
            "fields": {
                "error": "retryable operation failed once",
                "message": "operation failed; continuing"
            },
            "target": "codex_core::session::turn"
        })
        .to_string();
        let activity =
            classify_agent_output_activity("stderr", &line).expect("warning should be classified");

        assert_eq!(activity.0, "run");
        assert_eq!(activity.1, "Runtime warning");
        assert_eq!(activity_status(activity.0, activity.1), "warning");
    }

    #[test]
    fn downgrades_retryable_codex_infra_stderr_to_warning() {
        for line in [
            "\u{1b}[2m2026-05-18T14:32:02.340702Z\u{1b}[0m \u{1b}[31mERROR\u{1b}[0m \u{1b}[2mcodex_api::endpoint::responses_websocket\u{1b}[0m\u{1b}[2m:\u{1b}[0m failed to connect to websocket: IO error: tls handshake eof",
            "\u{1b}[2m2026-05-18T14:32:21.393770Z\u{1b}[0m \u{1b}[31mERROR\u{1b}[0m \u{1b}[2mcodex_models_manager::manager\u{1b}[0m\u{1b}[2m:\u{1b}[0m failed to refresh available models: timeout waiting for child process to exit",
            "\u{1b}[2m2026-05-18T14:31:57.219245Z\u{1b}[0m \u{1b}[31mERROR\u{1b}[0m \u{1b}[2mrmcp::transport::worker\u{1b}[0m\u{1b}[2m:\u{1b}[0m worker quit with fatal: Transport channel closed, when Client(HttpRequest(HttpRequest(\"http/request failed: error sending request for url (https://chatgpt.com/backend-api/wham/apps)\")))",
        ] {
            let activity = classify_agent_output_activity("stderr", line)
                .expect("retryable stderr should remain visible");
            assert_eq!(activity.0, "run");
            assert_eq!(activity.1, "Runtime warning");
            assert_eq!(activity_status(activity.0, activity.1), "warning");
        }
    }

    #[test]
    fn maps_unclassified_stderr_to_runtime_output_not_thinking() {
        assert_eq!(
            classify_agent_output_activity("stderr", "runtime heartbeat"),
            Some(("run", "Runtime output", "runtime heartbeat".to_owned()))
        );
        assert_eq!(
            classify_agent_output_activity("stdout", "model is considering options"),
            Some((
                "thinking",
                "Thinking",
                "model is considering options".to_owned()
            ))
        );
    }

    async fn test_pool() -> Option<(SqlitePool, String)> {
        test_pool_with_connections(1).await
    }

    async fn test_pool_with_connections(max_connections: u32) -> Option<(SqlitePool, String)> {
        let database_path =
            std::env::temp_dir().join(format!("lantor-test-{}.sqlite", Uuid::new_v4().simple()));
        let database_path = database_path.to_string_lossy().into_owned();
        let database_url = format!("sqlite://{database_path}");
        let pool = match db_connect_with_url(&database_url, max_connections).await {
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

    #[tokio::test]
    async fn usage_event_updates_run_tokens_and_cost() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "usage-agent").await?;
            let run_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_runs (agent_id, command, status)
                values ($1, 'codex app-server', 'running')
                returning id
                "#,
            )
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            handle_agent_event(
                &pool,
                agent_id,
                run_id,
                AgentEvent::Usage {
                    input_tokens: Some(1000),
                    output_tokens: Some(200),
                    total_tokens: None,
                    cost_micros: Some(1234),
                    cost_usd: None,
                },
            )
            .await?;
            let row = sqlx::query(
                "select input_tokens, output_tokens, cost_micros from agent_runs where id = $1",
            )
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(row.get::<i64, _>("input_tokens"), 1000);
            assert_eq!(row.get::<i64, _>("output_tokens"), 200);
            assert_eq!(row.get::<i64, _>("cost_micros"), 1234);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn memory_events_append_and_compact_memory_file() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "memory-agent").await?;
            let run_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_runs (agent_id, command, status)
                values ($1, 'codex app-server', 'running')
                returning id
                "#,
            )
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let dir = std::env::temp_dir().join(format!("lantor-memory-write-{}", Uuid::new_v4()));
            sqlx::query("update agents set working_directory = $2 where id = $1")
                .bind(agent_id)
                .bind(dir.to_string_lossy().to_string())
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            handle_agent_event(
                &pool,
                agent_id,
                run_id,
                AgentEvent::MemoryAppend {
                    body: "Remember: concise replies.".to_owned(),
                },
            )
            .await?;
            let memory_path = dir.join("MEMORY.md");
            let memory = std::fs::read_to_string(&memory_path).map_err(|err| err.to_string())?;
            let work_log_path = dir.join("notes").join("work-log.md");
            let work_log =
                std::fs::read_to_string(&work_log_path).map_err(|err| err.to_string())?;
            assert!(memory.contains("notes/work-log.md"));
            assert!(memory.contains("## Memory Map"));
            assert!(!memory.contains("## Memory update"));
            assert!(work_log.contains("## Memory update"));
            assert!(work_log.contains("Remember: concise replies."));
            handle_agent_event(
                &pool,
                agent_id,
                run_id,
                AgentEvent::MemoryCompact {
                    body: "# @memory-agent\n\n## Role\nCompact memory.\n".to_owned(),
                },
            )
            .await?;
            let memory = std::fs::read_to_string(&memory_path).map_err(|err| err.to_string())?;
            assert_eq!(memory, "# @memory-agent\n\n## Role\nCompact memory.\n");
            let _ = std::fs::remove_dir_all(dir);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn channel_events_create_channel_and_invite_agents() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "creator-agent").await?;
            let reviewer_id = insert_test_agent(&pool, "reviewer-agent").await?;
            let run_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_runs (agent_id, command, status)
                values ($1, 'codex app-server', 'running')
                returning id
                "#,
            )
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            handle_agent_event(
                &pool,
                agent_id,
                run_id,
                AgentEvent::ChannelCreate {
                    name: "Feature Room".to_owned(),
                    description: Some("long-lived feature coordination".to_owned()),
                    agent_handles: Some(vec!["@reviewer-agent".to_owned()]),
                },
            )
            .await?;
            let channel_id: Uuid =
                sqlx::query_scalar("select id from channels where name = 'feature-room'")
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            let members: i64 = sqlx::query_scalar(
                r#"
                select count(*)
                from channel_members
                where channel_id = $1 and agent_id in ($2, $3)
                "#,
            )
            .bind(channel_id)
            .bind(agent_id)
            .bind(reviewer_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(members, 2);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn profile_update_event_updates_agent_profile() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "profile-agent").await?;
            let run_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_runs (agent_id, command, status)
                values ($1, 'codex app-server', 'running')
                returning id
                "#,
            )
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            handle_agent_event(
                &pool,
                agent_id,
                run_id,
                AgentEvent::ProfileUpdate {
                    display_name: Some("Profile Agent".to_owned()),
                    role: Some("vision reviewer".to_owned()),
                    avatar: Some("P".to_owned()),
                    description: Some("Reviews screenshots and agent handoffs.".to_owned()),
                },
            )
            .await?;

            let row = sqlx::query(
                "select display_name, role, avatar, description from agents where id = $1",
            )
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(row.get::<String, _>("display_name"), "Profile Agent");
            assert_eq!(row.get::<String, _>("role"), "vision reviewer");
            assert_eq!(row.get::<String, _>("avatar"), "P");
            assert_eq!(
                row.get::<String, _>("description"),
                "Reviews screenshots and agent handoffs."
            );
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn history_context_and_attachment_tool_expose_attachment_paths() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let channel_id = insert_test_channel(&pool, "vision-channel").await?;
            let message_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'Please inspect this screenshot', false)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let image_path =
                std::env::temp_dir().join(format!("lantor-vision-{}.png", Uuid::new_v4()));
            std::fs::write(&image_path, b"fake image bytes").map_err(|err| err.to_string())?;
            let attachment_id: Uuid = sqlx::query_scalar(
                r#"
                insert into message_attachments (
                    message_id, original_name, mime_type, size_bytes, storage_path
                )
                values ($1, 'screen.png', 'image/png', 16, $2)
                returning id
                "#,
            )
            .bind(message_id)
            .bind(image_path.to_string_lossy().to_string())
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let history = agent_context_history_read(
                &pool,
                &[
                    "history-read".to_owned(),
                    "--target".to_owned(),
                    "#vision-channel".to_owned(),
                ],
            )
            .await?;
            assert!(history.contains(&attachment_id.to_string()));
            assert!(history.contains("local_path="));
            assert!(history.contains("attachment-info"));

            let info = agent_context_attachment_info(
                &pool,
                &[
                    "attachment-info".to_owned(),
                    "--attachment-id".to_owned(),
                    attachment_id.to_string(),
                ],
            )
            .await?;
            assert!(info.contains("mime=image/png"));
            assert!(info.contains("file_exists=true"));
            assert!(info.contains("vision_hint="));
            let _ = std::fs::remove_file(image_path);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn agent_inspect_tool_summarizes_profile_runs_requests_and_activity() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "inspectable").await?;
            let channel_id = insert_test_channel(&pool, "inspect-channel").await?;
            sqlx::query(
                r#"
                update agents
                set display_name = 'Inspectable Agent',
                    role = 'review specialist',
                    description = 'Reviews work from other agents.',
                    daily_budget_micros = 2500000
                where id = $1
                "#,
            )
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let run_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_runs (
                    agent_id, command, status, input_tokens, output_tokens, cost_micros
                )
                values ($1, 'codex app-server', 'complete', 100, 20, 500)
                returning id
                "#,
            )
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            sqlx::query(
                r#"
                insert into agent_work_items (agent_id, channel_id, title, context, status, source_kind)
                values ($1, $2, 'Review implementation', 'context', 'done', 'collaboration')
                "#,
            )
            .bind(agent_id)
            .bind(channel_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
            record_agent_activity(
                &pool,
                Some(agent_id),
                Some(run_id),
                "thinking",
                "Reading recent context",
                "{}".to_owned(),
            )
            .await?;

            let output = agent_context_agent_inspect(
                &pool,
                &[
                    "agent-inspect".to_owned(),
                    "--target".to_owned(),
                    "@inspectable".to_owned(),
                ],
            )
            .await?;
            assert!(output.contains("Agent @inspectable"));
            assert!(output.contains("display_name=Inspectable Agent"));
            assert!(output.contains("role=review specialist"));
            assert!(output.contains("recent_runs:"));
            assert!(output.contains("recent_requests:"));
            assert!(output.contains("recent_activity:"));
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn streaming_agent_messages_append_and_finish() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "streamer").await?;
            let channel_id = insert_test_channel(&pool, "streaming").await?;
            let stream_key = "run-1:item-1";

            let message_id = append_streaming_agent_message(
                &pool, agent_id, channel_id, None, stream_key, "Hel",
            )
            .await?;
            let same_message_id =
                append_streaming_agent_message(&pool, agent_id, channel_id, None, stream_key, "lo")
                    .await?;
            assert_eq!(message_id, same_message_id);
            finish_streaming_agent_message(&pool, stream_key, "complete").await?;

            let messages = load_messages(&pool).await?;
            let message = messages
                .iter()
                .find(|message| message.id == message_id)
                .expect("streaming message should be visible in bootstrap payload");
            assert_eq!(message.body, "Hello");
            assert_eq!(message.delivery_state, "complete");
            assert_eq!(message.stream_key, stream_key);

            upsert_runtime_thread_id(&pool, agent_id, "codex", "thread-1", "idle").await?;
            assert_eq!(
                load_runtime_thread_id(&pool, agent_id, "codex").await?,
                Some("thread-1".to_owned())
            );
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn streaming_placeholder_is_reused_for_visible_reply() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "placeholder-agent").await?;
            let channel_id = insert_test_channel(&pool, "placeholder-channel").await?;
            let run_id = Uuid::new_v4();
            let pending_stream_key = format!("{run_id}:pending");
            let final_stream_key = format!("{run_id}:item-1");

            let placeholder_id = ensure_streaming_agent_message(
                &pool,
                agent_id,
                channel_id,
                None,
                &pending_stream_key,
            )
            .await?;
            let messages = load_messages(&pool).await?;
            let placeholder = messages
                .iter()
                .find(|message| message.id == placeholder_id)
                .expect("placeholder should be visible in bootstrap payload");
            assert_eq!(placeholder.body, "");
            assert_eq!(placeholder.delivery_state, "streaming");
            assert_eq!(placeholder.stream_key, pending_stream_key);

            let adopted_id =
                adopt_streaming_agent_message_key(&pool, &pending_stream_key, &final_stream_key)
                    .await?;
            assert_eq!(adopted_id, Some(placeholder_id));
            assert!(streaming_message_body_is_empty(&pool, &final_stream_key).await?);

            let message_id = append_streaming_agent_message(
                &pool,
                agent_id,
                channel_id,
                None,
                &final_stream_key,
                "Done",
            )
            .await?;
            assert_eq!(message_id, placeholder_id);
            finish_streaming_agent_message(&pool, &final_stream_key, "complete").await?;

            let messages = load_messages(&pool).await?;
            let message = messages
                .iter()
                .find(|message| message.id == placeholder_id)
                .expect("final reply should reuse placeholder message");
            assert_eq!(message.body, "Done");
            assert_eq!(message.delivery_state, "complete");
            assert_eq!(message.stream_key, final_stream_key);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn activity_only_streaming_reply_keeps_status_message() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "activity-only-agent").await?;
            let channel_id = insert_test_channel(&pool, "activity-only-channel").await?;
            let run_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_runs (agent_id, command, status)
                values ($1, 'codex app-server', 'running')
                returning id
                "#,
            )
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let stream_key = format!("{run_id}:item-activity");
            let event = json!({
                "type": "activity",
                "kind": "thinking",
                "title": "Checking source",
                "detail": "Tracing the code path"
            });

            let message_id = append_streaming_agent_message(
                &pool,
                agent_id,
                channel_id,
                None,
                &stream_key,
                &format!("LANTOR_EVENT {event}\n"),
            )
            .await?;
            finish_streaming_agent_message(&pool, &stream_key, "complete").await?;

            let row = sqlx::query("select body, delivery_state from messages where id = $1")
                .bind(message_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
            assert_eq!(row.get::<String, _>("body"), "");
            assert_eq!(row.get::<String, _>("delivery_state"), "complete");

            let activity_count: i64 = sqlx::query_scalar(
                "select count(*) from agent_activities where run_id = $1 and title = 'Checking source'",
            )
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(activity_count, 1);

            let leaked_messages: i64 = sqlx::query_scalar(
                "select count(*) from messages where body like '%LANTOR_EVENT%'",
            )
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(leaked_messages, 0);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn visible_reply_replaces_prior_activity_only_status_message() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "visible-after-progress-agent").await?;
            let channel_id = insert_test_channel(&pool, "visible-after-progress-channel").await?;
            let run_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_runs (agent_id, command, status)
                values ($1, 'codex app-server', 'running')
                returning id
                "#,
            )
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let progress_stream_key = format!("{run_id}:item-progress");
            let final_stream_key = format!("{run_id}:item-final");
            let event = json!({
                "type": "activity",
                "kind": "thinking",
                "title": "Checking source",
                "detail": "Tracing the code path"
            });

            let progress_message_id = append_streaming_agent_message(
                &pool,
                agent_id,
                channel_id,
                None,
                &progress_stream_key,
                &format!("LANTOR_EVENT {event}\n"),
            )
            .await?;
            finish_streaming_agent_message(&pool, &progress_stream_key, "complete").await?;

            let final_message_id = append_streaming_agent_message(
                &pool,
                agent_id,
                channel_id,
                None,
                &final_stream_key,
                "Done",
            )
            .await?;
            finish_streaming_agent_message(&pool, &final_stream_key, "complete").await?;

            assert_ne!(progress_message_id, final_message_id);
            let progress_message_count: i64 =
                sqlx::query_scalar("select count(*) from messages where id = $1")
                    .bind(progress_message_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(progress_message_count, 0);

            let messages = load_messages(&pool).await?;
            let final_message = messages
                .iter()
                .find(|message| message.id == final_message_id)
                .expect("final reply should remain visible");
            assert_eq!(final_message.body, "Done");
            assert_eq!(final_message.delivery_state, "complete");
            assert_eq!(final_message.stream_key, final_stream_key);
            assert_eq!(
                messages
                    .iter()
                    .filter(|message| message.stream_key.starts_with(&run_id.to_string()))
                    .count(),
                1
            );
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn silent_streaming_reply_hides_message_and_marks_work_item() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "silent-agent").await?;
            let channel_id = insert_test_channel(&pool, "silent-channel").await?;
            let run_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_runs (agent_id, command, status)
                values ($1, 'codex app-server', 'running')
                returning id
                "#,
            )
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let work_item_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_work_items (agent_id, channel_id, title, context, status, run_id)
                values ($1, $2, 'hello', 'hi', 'running', $3)
                returning id
                "#,
            )
            .bind(agent_id)
            .bind(channel_id)
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let inbox_item_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_inbox_items (
                    agent_id, channel_id, kind, state, title, body_preview, work_item_id
                )
                values ($1, $2, 'dm', 'processing', 'hello', 'hi', $3)
                returning id
                "#,
            )
            .bind(agent_id)
            .bind(channel_id)
            .bind(work_item_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let stream_key = "silent-run:item-1";
            let message_id = append_streaming_agent_message(
                &pool,
                agent_id,
                channel_id,
                None,
                stream_key,
                "LANTOR_SILENT_REPLY: greeting only",
            )
            .await?;
            assert!(
                maybe_hide_silent_streaming_reply(
                    &pool,
                    agent_id,
                    run_id,
                    Some(work_item_id),
                    stream_key,
                )
                .await?
            );

            let remaining: i64 = sqlx::query_scalar("select count(*) from messages where id = $1")
                .bind(message_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
            assert_eq!(remaining, 0);
            let status: String =
                sqlx::query_scalar("select status from agent_work_items where id = $1")
                    .bind(work_item_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(status, "silent");
            let inbox_state: String =
                sqlx::query_scalar("select state from agent_inbox_items where id = $1")
                    .bind(inbox_item_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(inbox_state, "archived");
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn streaming_reminder_control_line_is_consumed() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "reminder-agent").await?;
            let channel_id = insert_test_channel(&pool, "reminder-control").await?;
            let root_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'remind me later', false)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let work_item_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_work_items (
                    agent_id, channel_id, thread_root_id, source_message_id, title, context, status
                )
                values ($1, $2, $3, $3, 'set reminder', 'remind me later', 'running')
                returning id
                "#,
            )
            .bind(agent_id)
            .bind(channel_id)
            .bind(root_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let run_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_runs (agent_id, work_item_id, command, status)
                values ($1, $2, 'codex app-server', 'running')
                returning id
                "#,
            )
            .bind(agent_id)
            .bind(work_item_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let due_at = (Utc::now() + ChronoDuration::minutes(30)).to_rfc3339();
            let event = json!({
                "type": "reminder_create",
                "when": due_at,
                "title": "Check PR",
                "note": "Look at CI"
            });
            let stream_key = "reminder-run:item-1";
            let body = format!("I'll remind you.\nLANTOR_EVENT {event}");
            let message_id = append_streaming_agent_message(
                &pool,
                agent_id,
                channel_id,
                Some(root_id),
                stream_key,
                &body,
            )
            .await?;

            let hidden = consume_streaming_agent_control_lines(
                &pool,
                agent_id,
                run_id,
                Some(work_item_id),
                stream_key,
            )
            .await?;
            assert!(!hidden);

            let visible_body: String =
                sqlx::query_scalar("select body from messages where id = $1")
                    .bind(message_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(visible_body, "I'll remind you.");

            let reminders = load_reminders(&pool).await?;
            assert_eq!(reminders.len(), 1);
            assert_eq!(reminders[0].title, "Check PR");
            assert_eq!(reminders[0].note, "Look at CI");
            assert_eq!(reminders[0].creator_agent_id, Some(agent_id));
            assert_eq!(reminders[0].channel_id, Some(channel_id));
            assert_eq!(reminders[0].thread_root_id, Some(root_id));
            assert_eq!(reminders[0].message_id, Some(root_id));
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn streaming_artifact_control_line_is_consumed_and_hidden() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "artifact-stream-agent").await?;
            let channel_id = insert_test_channel(&pool, "artifact-stream-control").await?;
            let root_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'make an architecture artifact', false)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let work_item_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_work_items (
                    agent_id, channel_id, thread_root_id, source_message_id, title, context, status
                )
                values ($1, $2, $3, $3, 'create artifact', 'make an architecture artifact', 'running')
                returning id
                "#,
            )
            .bind(agent_id)
            .bind(channel_id)
            .bind(root_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let run_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_runs (agent_id, work_item_id, command, status)
                values ($1, $2, 'codex app-server', 'running')
                returning id
                "#,
            )
            .bind(agent_id)
            .bind(work_item_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let event = json!({
                "type": "artifact_create",
                "channel_id": channel_id,
                "thread_root_id": root_id,
                "kind": "markdown",
                "title": "Architecture report",
                "summary": "Markdown architecture summary.",
                "content": "# Architecture\n\n- UI\n- Backend\n- Postgres"
            });
            let stream_key = "artifact-run:item-1";
            let raw_control_message_id = append_streaming_agent_message(
                &pool,
                agent_id,
                channel_id,
                Some(root_id),
                stream_key,
                &format!("LANTOR_EVENT {event}"),
            )
            .await?;

            let hidden = consume_streaming_agent_control_lines(
                &pool,
                agent_id,
                run_id,
                Some(work_item_id),
                stream_key,
            )
            .await?;
            assert!(hidden);

            let raw_remaining: i64 =
                sqlx::query_scalar("select count(*) from messages where id = $1")
                    .bind(raw_control_message_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(raw_remaining, 0);

            let artifact = sqlx::query(
                r#"
                select kind, title, content
                from artifacts
                where channel_id = $1 and thread_root_id = $2
                "#,
            )
            .bind(channel_id)
            .bind(root_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(artifact.get::<String, _>("kind"), "markdown");
            assert_eq!(artifact.get::<String, _>("title"), "Architecture report");
            assert!(artifact
                .get::<String, _>("content")
                .contains("# Architecture"));

            let visible_messages = load_messages(&pool).await?;
            assert!(!visible_messages
                .iter()
                .any(|message| message.body.contains("LANTOR_EVENT")));
            assert!(visible_messages.iter().any(|message| message
                .body
                .contains("Created artifact: Architecture report")));
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn streaming_finish_consumes_channel_create_control_line() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "creator-agent").await?;
            let reviewer_id = insert_test_agent(&pool, "Hancock").await?;
            let source_channel_id = insert_test_channel(&pool, "source-channel").await?;
            let run_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_runs (agent_id, command, status)
                values ($1, 'claude stream-json', 'running')
                returning id
                "#,
            )
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let event = json!({
                "type": "channel_create",
                "name": "lantor-ui-design",
                "description": "讨论 Lantor UI 设计后续工作",
                "agent_handles": ["hancock"]
            });
            let stream_key = format!("{run_id}:claude-assistant");
            let message_id = append_streaming_agent_message(
                &pool,
                agent_id,
                source_channel_id,
                None,
                &stream_key,
                &format!("好的，我来创建。\n\nLANTOR_EVENT {event}"),
            )
            .await?;

            finish_streaming_agent_message(&pool, &stream_key, "complete").await?;

            let visible_body: String =
                sqlx::query_scalar("select body from messages where id = $1")
                    .bind(message_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(visible_body, "好的，我来创建。");

            let channel_id: Uuid =
                sqlx::query_scalar("select id from channels where name = 'lantor-ui-design'")
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            let member_count: i64 = sqlx::query_scalar(
                r#"
                select count(*)
                from channel_members
                where channel_id = $1 and agent_id in ($2, $3)
                "#,
            )
            .bind(channel_id)
            .bind(agent_id)
            .bind(reviewer_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(member_count, 2);

            let leaked_messages: i64 = sqlx::query_scalar(
                "select count(*) from messages where body like '%LANTOR_EVENT%'",
            )
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(leaked_messages, 0);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn streaming_unsupported_artifact_control_line_keeps_status_message() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "unsupported-artifact-agent").await?;
            let channel_id = insert_test_channel(&pool, "unsupported-artifact-control").await?;
            let run_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_runs (agent_id, command, status)
                values ($1, 'codex app-server', 'running')
                returning id
                "#,
            )
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let event = json!({
                "type": "artifact_create",
                "channel_id": channel_id,
                "kind": "html",
                "title": "Unsupported HTML",
                "content": "<main>not supported</main>"
            });
            let stream_key = "unsupported-artifact-run:item-1";
            let raw_control_message_id = append_streaming_agent_message(
                &pool,
                agent_id,
                channel_id,
                None,
                stream_key,
                &format!("LANTOR_EVENT {event}"),
            )
            .await?;

            let hidden =
                consume_streaming_agent_control_lines(&pool, agent_id, run_id, None, stream_key)
                    .await?;
            assert!(!hidden);

            let raw_remaining: i64 =
                sqlx::query_scalar("select count(*) from messages where id = $1")
                    .bind(raw_control_message_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(raw_remaining, 1);

            finish_streaming_agent_message(&pool, stream_key, "complete").await?;
            let raw_body: String = sqlx::query_scalar("select body from messages where id = $1")
                .bind(raw_control_message_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
            assert_eq!(raw_body, "");

            let artifact_count: i64 =
                sqlx::query_scalar("select count(*) from artifacts where channel_id = $1")
                    .bind(channel_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(artifact_count, 0);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn dm_rejects_tasks_and_auto_dispatches_owner_messages() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "dm-dispatch").await?;
            let dm_channel_id =
                Uuid::parse_str(&open_dm_with_agent_in_pool(&pool, agent_id).await?)
                    .map_err(|err| err.to_string())?;

            let owner_task_err =
                send_owner_message_in_pool(&pool, dm_channel_id, None, "task body", true, vec![])
                    .await
                    .unwrap_err();
            assert!(owner_task_err.contains("direct messages do not support tasks"));

            let agent_task_err =
                insert_agent_message(&pool, agent_id, dm_channel_id, None, "task body", true)
                    .await
                    .unwrap_err();
            assert!(agent_task_err.contains("direct messages do not support tasks"));

            send_owner_message_in_pool(
                &pool,
                dm_channel_id,
                None,
                "please inspect this",
                false,
                vec![],
            )
            .await?;
            let work_items: i64 = sqlx::query_scalar(
                r#"
                select count(*)
                from agent_work_items
                where channel_id = $1 and agent_id = $2 and status = 'queued'
                "#,
            )
            .bind(dm_channel_id)
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(work_items, 1);
            let source_kind: String = sqlx::query_scalar(
                "select source_kind from agent_work_items where channel_id = $1 and agent_id = $2",
            )
            .bind(dm_channel_id)
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(source_kind, "inbox_wake");
            let inbox = sqlx::query(
                r#"
                select i.id as inbox_item_id, i.kind, i.state, i.work_item_id, w.id as linked_work_item_id, w.inbox_item_id as linked_inbox_item_id
                from agent_inbox_items i
                join agent_work_items w on w.id = i.work_item_id
                where i.channel_id = $1 and i.agent_id = $2
                "#,
            )
            .bind(dm_channel_id)
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(inbox.get::<String, _>("kind"), "dm");
            assert_eq!(inbox.get::<String, _>("state"), "processing");
            assert_eq!(
                inbox.get::<Option<Uuid>, _>("work_item_id"),
                Some(inbox.get::<Uuid, _>("linked_work_item_id"))
            );
            assert_eq!(
                inbox.get::<Option<Uuid>, _>("linked_inbox_item_id"),
                Some(inbox.get::<Uuid, _>("inbox_item_id"))
            );
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn dm_codex_surface_requires_same_thread_root() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "dm-surface").await?;
            let dm_channel_id =
                Uuid::parse_str(&open_dm_with_agent_in_pool(&pool, agent_id).await?)
                    .map_err(|err| err.to_string())?;
            let root_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'thread root', false)
                returning id
                "#,
            )
            .bind(dm_channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            assert!(
                same_codex_surface(&pool, Some(dm_channel_id), None, Some(dm_channel_id), None)
                    .await?
            );
            assert!(
                same_codex_surface(
                    &pool,
                    Some(dm_channel_id),
                    Some(root_id),
                    Some(dm_channel_id),
                    Some(root_id),
                )
                .await?
            );
            assert!(
                !same_codex_surface(
                    &pool,
                    Some(dm_channel_id),
                    Some(root_id),
                    Some(dm_channel_id),
                    None,
                )
                .await?
            );
            assert!(
                !same_codex_surface(
                    &pool,
                    Some(dm_channel_id),
                    None,
                    Some(dm_channel_id),
                    Some(root_id),
                )
                .await?
            );
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn inbox_wake_creates_work_items_without_serializing_unread_items() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "inbox-reschedule").await?;
            let dm_channel_id =
                Uuid::parse_str(&open_dm_with_agent_in_pool(&pool, agent_id).await?)
                    .map_err(|err| err.to_string())?;

            send_owner_message_in_pool(
                &pool,
                dm_channel_id,
                None,
                "first inbox item",
                false,
                vec![],
            )
            .await?;
            send_owner_message_in_pool(
                &pool,
                dm_channel_id,
                None,
                "second inbox item",
                false,
                vec![],
            )
            .await?;

            let initial_work_items: Vec<Uuid> = sqlx::query_scalar(
                "select id from agent_work_items where agent_id = $1 order by created_at asc",
            )
            .bind(agent_id)
            .fetch_all(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(initial_work_items.len(), 2);

            let state_counts = sqlx::query(
                r#"
                select
                    count(*) filter (where state = 'processing') as processing,
                    count(*) filter (where state = 'unread') as unread
                from agent_inbox_items
                where agent_id = $1
                "#,
            )
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(state_counts.get::<Option<i64>, _>("processing"), Some(2));
            assert_eq!(state_counts.get::<Option<i64>, _>("unread"), Some(0));

            sqlx::query("update agent_work_items set status = 'done' where id = $1")
                .bind(initial_work_items[0])
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            notify_ui_work_item_changed(&pool, initial_work_items[0], "test_done").await;

            let final_work_items: Vec<Uuid> = sqlx::query_scalar(
                "select id from agent_work_items where agent_id = $1 order by created_at asc",
            )
            .bind(agent_id)
            .fetch_all(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(final_work_items.len(), 2);
            let state_counts = sqlx::query(
                r#"
                select
                    count(*) filter (where state = 'archived') as archived,
                    count(*) filter (where state = 'processing') as processing,
                    count(*) filter (where state = 'unread') as unread
                from agent_inbox_items
                where agent_id = $1
                "#,
            )
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(state_counts.get::<Option<i64>, _>("archived"), Some(1));
            assert_eq!(state_counts.get::<Option<i64>, _>("processing"), Some(1));
            assert_eq!(state_counts.get::<Option<i64>, _>("unread"), Some(0));
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn inbox_wake_batches_unread_items_for_same_thread() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "inbox-batch").await?;
            let channel_id = insert_test_channel(&pool, "batch-thread").await?;
            sqlx::query(
                r#"
                insert into channel_members (channel_id, agent_id)
                values ($1, $2)
                "#,
            )
            .bind(channel_id)
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let root_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', '@inbox-batch please investigate', false)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            sqlx::query(
                r#"
                insert into agent_work_items (
                    agent_id, channel_id, thread_root_id, source_message_id, title, context, status
                )
                values ($1, $2, $3, $3, 'Initial dispatch', 'Initial dispatch', 'done')
                "#,
            )
            .bind(agent_id)
            .bind(channel_id)
            .bind(root_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            send_owner_message_in_pool(
                &pool,
                channel_id,
                Some(root_id),
                "first pending follow-up",
                false,
                vec![],
            )
            .await?;
            send_owner_message_in_pool(
                &pool,
                channel_id,
                Some(root_id),
                "second pending follow-up",
                false,
                vec![],
            )
            .await?;

            let rows = sqlx::query(
                r#"
                select
                    w.id,
                    w.title,
                    w.context,
                    count(i.id) as inbox_count,
                    count(*) filter (where i.state = 'processing') as processing_count
                from agent_work_items w
                join agent_inbox_items i on i.work_item_id = w.id
                where w.agent_id = $1
                  and w.channel_id = $2
                  and w.thread_root_id = $3
                  and w.source_kind = 'inbox_wake'
                group by w.id, w.title, w.context
                "#,
            )
            .bind(agent_id)
            .bind(channel_id)
            .bind(root_id)
            .fetch_all(&pool)
            .await
            .map_err(|err| err.to_string())?;

            assert_eq!(rows.len(), 1);
            let work_item_id: Uuid = rows[0].get("id");
            assert_eq!(rows[0].get::<i64, _>("inbox_count"), 2);
            assert_eq!(rows[0].get::<i64, _>("processing_count"), 2);
            assert_eq!(
                rows[0].get::<String, _>("title"),
                "Process inbox: first pending follow-up (+1 more)"
            );
            let context: String = rows[0].get("context");
            assert!(context.contains("batches 2 inbox items"));
            assert!(context.contains("first pending follow-up"));
            assert!(context.contains("second pending follow-up"));

            let start_commands: i64 = sqlx::query_scalar(
                r#"
                select count(*)
                from supervisor_commands
                where agent_id = $1 and command_type = 'start_agent'
                "#,
            )
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(start_commands, 1);

            sqlx::query("update agent_work_items set status = 'done' where id = $1")
                .bind(work_item_id)
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            notify_ui_work_item_changed(&pool, work_item_id, "test_done").await;
            let archived: i64 = sqlx::query_scalar(
                r#"
                select count(*)
                from agent_inbox_items
                where agent_id = $1 and work_item_id = $2 and state = 'archived'
                "#,
            )
            .bind(agent_id)
            .bind(work_item_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(archived, 2);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn agent_context_inbox_tools_list_read_and_archive_items() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "inbox-tool").await?;
            let dm_channel_id =
                Uuid::parse_str(&open_dm_with_agent_in_pool(&pool, agent_id).await?)
                    .map_err(|err| err.to_string())?;
            send_owner_message_in_pool(
                &pool,
                dm_channel_id,
                None,
                "please inspect inbox tools",
                false,
                vec![],
            )
            .await?;
            let inbox_id: Uuid = sqlx::query_scalar(
                "select id from agent_inbox_items where agent_id = $1 and kind = 'dm'",
            )
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let list = agent_context_inbox_list(
                &pool,
                &[
                    "inbox-list".to_owned(),
                    "--target".to_owned(),
                    "@inbox-tool".to_owned(),
                    "--state".to_owned(),
                    "active".to_owned(),
                ],
            )
            .await?;
            assert!(list.contains("Lantor inbox for @inbox-tool"));
            assert!(list.contains("kind=dm"));
            assert!(list.contains("please inspect inbox tools"));

            let read = agent_context_inbox_read(
                &pool,
                &[
                    "inbox-read".to_owned(),
                    "--target".to_owned(),
                    "@inbox-tool".to_owned(),
                    "--inbox-id".to_owned(),
                    short_id(inbox_id),
                ],
            )
            .await?;
            assert!(read.contains("source_message:"));
            assert!(read.contains("please inspect inbox tools"));
            assert!(read.contains(&format!("--target \"{dm_channel_id}")));
            assert!(!read.contains("--target \"dm:"));
            assert!(read
                .contains("archive_note=linked work-item inbox items are archived automatically"));
            assert!(!read.contains("archive_when_done="));

            let archived = agent_context_inbox_archive(
                &pool,
                &[
                    "inbox-archive".to_owned(),
                    "--target".to_owned(),
                    "@inbox-tool".to_owned(),
                    "--inbox-id".to_owned(),
                    short_id(inbox_id),
                ],
            )
            .await?;
            assert!(archived.contains("Archived Lantor inbox item"));
            let state: String =
                sqlx::query_scalar("select state from agent_inbox_items where id = $1")
                    .bind(inbox_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(state, "archived");
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn owner_channel_root_message_without_mentions_delivers_to_member_agent_inbox() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "channel-listener").await?;
            let channel_id = insert_test_channel(&pool, "channel-root-delivery").await?;
            sqlx::query(
                r#"
                insert into channel_members (channel_id, agent_id)
                values ($1, $2)
                "#,
            )
            .bind(channel_id)
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            send_owner_message_in_pool(
                &pool,
                channel_id,
                None,
                "Lantor README needs a quick review",
                false,
                vec![],
            )
            .await?;
            let message_id: Uuid =
                sqlx::query_scalar("select id from messages where channel_id = $1 and body = $2")
                    .bind(channel_id)
                    .bind("Lantor README needs a quick review")
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            let row = sqlx::query(
                r#"
                select
                    w.source_kind,
                    w.thread_root_id,
                    w.source_message_id,
                    i.kind as inbox_kind,
                    i.priority,
                    i.body_preview
                from agent_work_items w
                join agent_inbox_items i on i.work_item_id = w.id
                where w.agent_id = $1 and w.source_message_id = $2
                "#,
            )
            .bind(agent_id)
            .bind(message_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(row.get::<String, _>("source_kind"), "inbox_wake");
            assert_eq!(row.get::<String, _>("inbox_kind"), "channel_message");
            assert_eq!(row.get::<i32, _>("priority"), 35);
            assert_eq!(
                row.get::<Option<Uuid>, _>("thread_root_id"),
                Some(message_id)
            );
            assert_eq!(
                row.get::<Option<Uuid>, _>("source_message_id"),
                Some(message_id)
            );
            assert!(row
                .get::<String, _>("body_preview")
                .contains("README needs"));
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn owner_thread_followup_dispatches_to_thread_agents_without_mentions() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "thread-agent").await?;
            let channel_id = insert_test_channel(&pool, "thread-followup").await?;
            sqlx::query(
                r#"
                insert into channel_members (channel_id, agent_id)
                values ($1, $2)
                "#,
            )
            .bind(channel_id)
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let root_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', '@thread-agent please investigate', false)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            sqlx::query(
                r#"
                insert into agent_work_items (
                    agent_id, channel_id, thread_root_id, source_message_id, title, context, status
                )
                values ($1, $2, $3, $3, 'Initial dispatch', 'Initial dispatch', 'done')
                "#,
            )
            .bind(agent_id)
            .bind(channel_id)
            .bind(root_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            send_owner_message_in_pool(
                &pool,
                channel_id,
                Some(root_id),
                "我补充一下：这个复现只在 thread 里出现",
                false,
                vec![],
            )
            .await?;

            let work_items = sqlx::query(
                r#"
                select
                    w.source_message_id,
                    w.source_kind,
                    w.title,
                    w.context,
                    i.kind as inbox_kind,
                    i.body_preview
                from agent_work_items w
                join agent_inbox_items i on i.work_item_id = w.id
                where w.channel_id = $1 and w.agent_id = $2 and w.source_message_id <> $3
                "#,
            )
            .bind(channel_id)
            .bind(agent_id)
            .bind(root_id)
            .fetch_all(&pool)
            .await
            .map_err(|err| err.to_string())?;

            assert_eq!(work_items.len(), 1);
            assert_eq!(work_items[0].get::<String, _>("source_kind"), "inbox_wake");
            assert_eq!(
                work_items[0].get::<String, _>("inbox_kind"),
                "thread_followup"
            );
            assert_eq!(
                work_items[0]
                    .get::<Option<String>, _>("title")
                    .unwrap_or_default(),
                "Process inbox: 我补充一下：这个复现只在 thread 里出现"
            );
            let context: String = work_items[0].get("context");
            assert!(context.contains("Lantor agent inbox wake"));
            assert!(context.contains("inbox-read"));
            assert!(work_items[0]
                .get::<String, _>("body_preview")
                .contains("这个复现"));
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn owner_thread_followup_with_explicit_mention_does_not_fan_out_to_thread_agents() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let mentioned_agent_id = insert_test_agent(&pool, "mentioned-agent").await?;
            let bystander_agent_id = insert_test_agent(&pool, "bystander-agent").await?;
            let channel_id = insert_test_channel(&pool, "thread-explicit-owner").await?;
            for agent_id in [mentioned_agent_id, bystander_agent_id] {
                sqlx::query(
                    r#"
                    insert into channel_members (channel_id, agent_id)
                    values ($1, $2)
                    "#,
                )
                .bind(channel_id)
                .bind(agent_id)
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            }

            let root_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'root request', false)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            upsert_agent_thread_subscription(
                &pool,
                mentioned_agent_id,
                channel_id,
                root_id,
                "mention",
                Some(root_id),
            )
            .await?;
            upsert_agent_thread_subscription(
                &pool,
                bystander_agent_id,
                channel_id,
                root_id,
                "mention",
                Some(root_id),
            )
            .await?;

            send_owner_message_in_pool(
                &pool,
                channel_id,
                Some(root_id),
                "@mentioned-agent 这个后续只给被点名的 agent",
                false,
                vec![],
            )
            .await?;

            let mentioned_inboxes: i64 = sqlx::query_scalar(
                r#"
                select count(*)
                from agent_inbox_items
                where agent_id = $1
                  and source_message_id <> $2
                  and kind = 'mention'
                "#,
            )
            .bind(mentioned_agent_id)
            .bind(root_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let bystander_inboxes: i64 = sqlx::query_scalar(
                r#"
                select count(*)
                from agent_inbox_items
                where agent_id = $1
                  and source_message_id <> $2
                "#,
            )
            .bind(bystander_agent_id)
            .bind(root_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            assert_eq!(mentioned_inboxes, 1);
            assert_eq!(bystander_inboxes, 0);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn owner_thread_followup_with_unknown_mention_does_not_fall_back_to_thread_agents() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "known-thread-agent").await?;
            let channel_id = insert_test_channel(&pool, "thread-unknown-mention").await?;
            sqlx::query(
                r#"
                insert into channel_members (channel_id, agent_id)
                values ($1, $2)
                "#,
            )
            .bind(channel_id)
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let root_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'root request', false)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            upsert_agent_thread_subscription(
                &pool,
                agent_id,
                channel_id,
                root_id,
                "mention",
                Some(root_id),
            )
            .await?;

            send_owner_message_in_pool(
                &pool,
                channel_id,
                Some(root_id),
                "@missing-agent 这个后续不应该 fallback 给 thread 参与者",
                false,
                vec![],
            )
            .await?;

            let inboxes: i64 = sqlx::query_scalar(
                r#"
                select count(*)
                from agent_inbox_items
                where agent_id = $1
                  and source_message_id <> $2
                "#,
            )
            .bind(agent_id)
            .bind(root_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            assert_eq!(inboxes, 0);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn owner_thread_followup_uses_agent_thread_subscription_after_work_done() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "sub-agent").await?;
            let channel_id = insert_test_channel(&pool, "thread-subscription").await?;
            sqlx::query(
                r#"
                insert into channel_members (channel_id, agent_id)
                values ($1, $2)
                "#,
            )
            .bind(channel_id)
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let root_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'root request', false)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            upsert_agent_thread_subscription(
                &pool,
                agent_id,
                channel_id,
                root_id,
                "mention",
                Some(root_id),
            )
            .await?;

            send_owner_message_in_pool(
                &pool,
                channel_id,
                Some(root_id),
                "继续补充，不需要重新 @ agent",
                false,
                vec![],
            )
            .await?;

            let row = sqlx::query(
                r#"
                select w.source_kind, w.thread_root_id, w.task_id, i.kind as inbox_kind
                from agent_work_items w
                join agent_inbox_items i on i.work_item_id = w.id
                where w.agent_id = $1 and w.source_message_id <> $2
                order by w.created_at desc
                limit 1
                "#,
            )
            .bind(agent_id)
            .bind(root_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(row.get::<String, _>("source_kind"), "inbox_wake");
            assert_eq!(row.get::<String, _>("inbox_kind"), "thread_followup");
            assert_eq!(row.get::<Option<Uuid>, _>("thread_root_id"), Some(root_id));
            assert_eq!(row.get::<Option<Uuid>, _>("task_id"), None);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn mentions_create_agent_requests_but_only_task_mode_creates_global_tasks() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "task-agent").await?;
            let channel_id = insert_test_channel(&pool, "task-semantics").await?;

            send_owner_message_in_pool(
                &pool,
                channel_id,
                None,
                "@task-agent please look at this",
                false,
                vec![],
            )
            .await?;
            let task_count: i64 = sqlx::query_scalar("select count(*) from tasks")
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
            assert_eq!(task_count, 0);
            let mention_inbox_kind: String = sqlx::query_scalar(
                "select kind from agent_inbox_items where agent_id = $1 order by created_at desc limit 1",
            )
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(mention_inbox_kind, "mention");

            send_owner_message_in_pool(
                &pool,
                channel_id,
                None,
                "@task-agent implement the tracked feature",
                true,
                vec![],
            )
            .await?;
            let task_count: i64 = sqlx::query_scalar("select count(*) from tasks")
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
            assert_eq!(task_count, 1);
            let task = sqlx::query("select status, assignee_agent_id from tasks limit 1")
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
            assert_eq!(task.get::<String, _>("status"), "in_progress");
            assert_eq!(
                task.get::<Option<Uuid>, _>("assignee_agent_id"),
                Some(agent_id)
            );
            let task_inbox_kind: String = sqlx::query_scalar(
                "select kind from agent_inbox_items where agent_id = $1 order by created_at desc limit 1",
            )
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(task_inbox_kind, "task_assigned");
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn owner_task_without_mentions_auto_assigns_single_channel_agent() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "solo-task-agent").await?;
            let channel_id = insert_test_channel(&pool, "solo-task").await?;
            sqlx::query(
                r#"
                insert into channel_members (channel_id, agent_id)
                values ($1, $2)
                "#,
            )
            .bind(channel_id)
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            send_owner_message_in_pool(
                &pool,
                channel_id,
                None,
                "Implement the compact task flow",
                true,
                vec![],
            )
            .await?;

            let task = sqlx::query("select status, assignee_agent_id from tasks limit 1")
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
            assert_eq!(task.get::<String, _>("status"), "in_progress");
            assert_eq!(
                task.get::<Option<Uuid>, _>("assignee_agent_id"),
                Some(agent_id)
            );
            let inbox_kind: String = sqlx::query_scalar(
                "select kind from agent_inbox_items where agent_id = $1 order by created_at desc limit 1",
            )
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(inbox_kind, "task_assigned");
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn owner_task_without_mentions_stays_unassigned_with_multiple_channel_agents() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let first_agent_id = insert_test_agent(&pool, "multi-task-a").await?;
            let second_agent_id = insert_test_agent(&pool, "multi-task-b").await?;
            let channel_id = insert_test_channel(&pool, "multi-task").await?;
            for agent_id in [first_agent_id, second_agent_id] {
                sqlx::query(
                    r#"
                    insert into channel_members (channel_id, agent_id)
                    values ($1, $2)
                    "#,
                )
                .bind(channel_id)
                .bind(agent_id)
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            }

            send_owner_message_in_pool(
                &pool,
                channel_id,
                None,
                "Implement the unassigned queue",
                true,
                vec![],
            )
            .await?;

            let task = sqlx::query("select status, assignee_agent_id from tasks limit 1")
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
            assert_eq!(task.get::<String, _>("status"), "todo");
            assert_eq!(task.get::<Option<Uuid>, _>("assignee_agent_id"), None);
            let inbox_count: i64 = sqlx::query_scalar(
                r#"
                select count(*)
                from agent_inbox_items
                where task_id = (select id from tasks limit 1)
                  and kind = 'task_available'
                "#,
            )
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(inbox_count, 2);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn unassigned_task_claim_is_atomic_across_agents() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let first_agent_id = insert_test_agent(&pool, "claim-race-a").await?;
            let second_agent_id = insert_test_agent(&pool, "claim-race-b").await?;
            let channel_id = insert_test_channel(&pool, "claim-race").await?;
            for agent_id in [first_agent_id, second_agent_id] {
                sqlx::query(
                    r#"
                    insert into channel_members (channel_id, agent_id)
                    values ($1, $2)
                    "#,
                )
                .bind(channel_id)
                .bind(agent_id)
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            }
            let message_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'Race to claim', true)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let task_id: Uuid = sqlx::query_scalar(
                r#"
                insert into tasks (message_id, channel_id, title, status)
                values ($1, $2, 'Race to claim', 'todo')
                returning id
                "#,
            )
            .bind(message_id)
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let first = try_claim_unassigned_task(&pool, task_id, first_agent_id, Some(0), "test");
            let second =
                try_claim_unassigned_task(&pool, task_id, second_agent_id, Some(0), "test");
            let (first, second) = tokio::join!(first, second);
            let wins = [first?, second?]
                .into_iter()
                .filter(|claim| claim.is_some())
                .count();
            assert_eq!(wins, 1);

            let task = sqlx::query("select status, assignee_agent_id, version from tasks where id = $1")
                .bind(task_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
            assert_eq!(task.get::<String, _>("status"), "in_progress");
            assert!(task.get::<Option<Uuid>, _>("assignee_agent_id").is_some());
            assert_eq!(task.get::<i64, _>("version"), 1);

            let assigned_inboxes: i64 = sqlx::query_scalar(
                "select count(*) from agent_inbox_items where task_id = $1 and kind = 'task_assigned'",
            )
            .bind(task_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(assigned_inboxes, 1);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn stale_task_claim_event_is_ignored_without_dispatch_noise() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let winner_agent_id = insert_test_agent(&pool, "claim-winner").await?;
            let stale_agent_id = insert_test_agent(&pool, "claim-stale").await?;
            let channel_id = insert_test_channel(&pool, "claim-stale").await?;
            for agent_id in [winner_agent_id, stale_agent_id] {
                sqlx::query(
                    r#"
                    insert into channel_members (channel_id, agent_id)
                    values ($1, $2)
                    "#,
                )
                .bind(channel_id)
                .bind(agent_id)
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            }
            let message_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'Claim once', true)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let task_row = sqlx::query(
                r#"
                insert into tasks (message_id, channel_id, title, status)
                values ($1, $2, 'Claim once', 'todo')
                returning id, number
                "#,
            )
            .bind(message_id)
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let task_id: Uuid = task_row.get("id");
            let task_number: i64 = task_row.get("number");
            assert!(
                try_claim_unassigned_task(&pool, task_id, winner_agent_id, None, "test")
                    .await?
                    .is_some()
            );

            let run_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_runs (agent_id, command, status)
                values ($1, 'codex app-server', 'running')
                returning id
                "#,
            )
            .bind(stale_agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let result = handle_agent_event(
                &pool,
                stale_agent_id,
                run_id,
                AgentEvent::TaskClaim {
                    task_number,
                    assignee_handle: None,
                },
            )
            .await?;
            assert_eq!(result, format!("task #{task_number} claim ignored"));

            let task = sqlx::query("select assignee_agent_id from tasks where id = $1")
                .bind(task_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
            assert_eq!(
                task.get::<Option<Uuid>, _>("assignee_agent_id"),
                Some(winner_agent_id)
            );
            let stale_inboxes: i64 = sqlx::query_scalar(
                "select count(*) from agent_inbox_items where task_id = $1 and agent_id = $2 and kind = 'task_assigned'",
            )
            .bind(task_id)
            .bind(stale_agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(stale_inboxes, 0);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn task_status_and_claim_events_do_not_insert_system_messages() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "task-noise-agent").await?;
            let channel_id = insert_test_channel(&pool, "task-noise").await?;
            sqlx::query(
                r#"
                insert into channel_members (channel_id, agent_id)
                values ($1, $2)
                "#,
            )
            .bind(channel_id)
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let message_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'Track this task', true)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let task_number: i64 = sqlx::query_scalar(
                r#"
                insert into tasks (message_id, channel_id, title, status)
                values ($1, $2, 'Track this task', 'todo')
                returning number
                "#,
            )
            .bind(message_id)
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let run_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_runs (agent_id, command, status)
                values ($1, 'codex app-server', 'running')
                returning id
                "#,
            )
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            handle_agent_event(
                &pool,
                agent_id,
                run_id,
                AgentEvent::TaskClaim {
                    task_number,
                    assignee_handle: None,
                },
            )
            .await?;
            handle_agent_event(
                &pool,
                agent_id,
                run_id,
                AgentEvent::TaskStatus {
                    task_number,
                    status: "done".to_owned(),
                },
            )
            .await?;

            let task = sqlx::query("select status, assignee_agent_id from tasks where number = $1")
                .bind(task_number)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
            assert_eq!(task.get::<String, _>("status"), "done");
            assert_eq!(
                task.get::<Option<Uuid>, _>("assignee_agent_id"),
                Some(agent_id)
            );

            let system_messages: i64 = sqlx::query_scalar(
                r#"
                select count(*)
                from messages
                where channel_id = $1
                  and sender_role = 'system'
                  and (body like '%claimed task%' or body like 'Task #%moved%')
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(system_messages, 0);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn task_work_item_queue_and_start_do_not_insert_system_messages() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "task-lifecycle-agent").await?;
            let channel_id = insert_test_channel(&pool, "task-lifecycle").await?;
            let message_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'Handle this task', true)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let task_id: Uuid = sqlx::query_scalar(
                r#"
                insert into tasks (message_id, channel_id, title, status, assignee_agent_id)
                values ($1, $2, 'Handle this task', 'in_progress', $3)
                returning id
                "#,
            )
            .bind(message_id)
            .bind(channel_id)
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let work_item_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_work_items (
                    agent_id, channel_id, thread_root_id, source_message_id,
                    task_id, source_kind, title, context, status
                )
                values ($1, $2, $3, $3, $4, 'task_assigned', 'Handle this task', 'context', 'queued')
                returning id
                "#,
            )
            .bind(agent_id)
            .bind(channel_id)
            .bind(message_id)
            .bind(task_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            notify_ui_work_item_changed(&pool, work_item_id, "work_item_created").await;
            notify_ui_work_item_changed(&pool, work_item_id, "work_item_queued").await;
            sqlx::query("update agent_work_items set status = 'running' where id = $1")
                .bind(work_item_id)
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            notify_ui_work_item_changed(&pool, work_item_id, "work_item_running").await;

            let system_messages: i64 = sqlx::query_scalar(
                r#"
                select count(*)
                from messages
                where channel_id = $1
                  and thread_root_id = $2
                  and sender_role = 'system'
                  and (body like '%queued task run%' or body like '%started task run%')
                "#,
            )
            .bind(channel_id)
            .bind(message_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(system_messages, 0);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn task_claim_opportunity_finish_does_not_insert_system_message() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let loser_agent_id = insert_test_agent(&pool, "claim-loser").await?;
            let winner_agent_id = insert_test_agent(&pool, "claim-winner-finish").await?;
            let channel_id = insert_test_channel(&pool, "claim-finish").await?;
            let available_message_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'Race this task', true)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let available_task_id: Uuid = sqlx::query_scalar(
                r#"
                insert into tasks (message_id, channel_id, title, status)
                values ($1, $2, 'Race this task', 'todo')
                returning id
                "#,
            )
            .bind(available_message_id)
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let available_inbox_id = create_agent_inbox_item(
                &pool,
                AgentInboxItemInput {
                    agent_id: loser_agent_id,
                    channel_id: Some(channel_id),
                    thread_root_id: Some(available_message_id),
                    source_message_id: Some(available_message_id),
                    task_id: Some(available_task_id),
                    kind: "task_available",
                    priority: 70,
                    title: "Race this task",
                    body_preview: "Race this task",
                    payload: json!({}),
                },
            )
            .await?;
            let available_work_item_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_work_items (
                    agent_id, channel_id, thread_root_id, source_message_id, inbox_item_id,
                    task_id, source_kind, title, context, status, completed_at
                )
                values ($1, $2, $3, $3, $4, $5, 'inbox_wake', 'Race this task', 'context', 'done', strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
                returning id
                "#,
            )
            .bind(loser_agent_id)
            .bind(channel_id)
            .bind(available_message_id)
            .bind(available_inbox_id)
            .bind(available_task_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            sqlx::query("update agent_inbox_items set work_item_id = $2 where id = $1")
                .bind(available_inbox_id)
                .bind(available_work_item_id)
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            notify_ui_work_item_changed(&pool, available_work_item_id, "work_item_finished").await;

            let assigned_message_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'Run this task', true)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let assigned_task_id: Uuid = sqlx::query_scalar(
                r#"
                insert into tasks (message_id, channel_id, title, status, assignee_agent_id)
                values ($1, $2, 'Run this task', 'in_progress', $3)
                returning id
                "#,
            )
            .bind(assigned_message_id)
            .bind(channel_id)
            .bind(winner_agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let assigned_inbox_id = create_agent_inbox_item(
                &pool,
                AgentInboxItemInput {
                    agent_id: winner_agent_id,
                    channel_id: Some(channel_id),
                    thread_root_id: Some(assigned_message_id),
                    source_message_id: Some(assigned_message_id),
                    task_id: Some(assigned_task_id),
                    kind: "task_assigned",
                    priority: 95,
                    title: "Run this task",
                    body_preview: "Run this task",
                    payload: json!({}),
                },
            )
            .await?;
            let assigned_work_item_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_work_items (
                    agent_id, channel_id, thread_root_id, source_message_id, inbox_item_id,
                    task_id, source_kind, title, context, status, completed_at
                )
                values ($1, $2, $3, $3, $4, $5, 'inbox_wake', 'Run this task', 'context', 'done', strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
                returning id
                "#,
            )
            .bind(winner_agent_id)
            .bind(channel_id)
            .bind(assigned_message_id)
            .bind(assigned_inbox_id)
            .bind(assigned_task_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            sqlx::query("update agent_inbox_items set work_item_id = $2 where id = $1")
                .bind(assigned_inbox_id)
                .bind(assigned_work_item_id)
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            notify_ui_work_item_changed(&pool, assigned_work_item_id, "work_item_finished").await;

            let claim_opportunity_messages: i64 = sqlx::query_scalar(
                r#"
                select count(*)
                from messages
                where channel_id = $1
                  and thread_root_id = $2
                  and sender_role = 'system'
                  and body like '@claim-loser completed task run%'
                "#,
            )
            .bind(channel_id)
            .bind(available_message_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(claim_opportunity_messages, 0);

            let assigned_messages: i64 = sqlx::query_scalar(
                r#"
                select count(*)
                from messages
                where channel_id = $1
                  and thread_root_id = $2
                  and sender_role = 'system'
                  and body like '@claim-winner-finish completed task run%'
                "#,
            )
            .bind(channel_id)
            .bind(assigned_message_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(assigned_messages, 0);

            sqlx::query("update tasks set status = 'in_review' where id = $1")
                .bind(assigned_task_id)
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            notify_ui_work_item_changed(&pool, assigned_work_item_id, "work_item_finished").await;

            let assigned_messages: i64 = sqlx::query_scalar(
                r#"
                select count(*)
                from messages
                where channel_id = $1
                  and thread_root_id = $2
                  and sender_role = 'system'
                  and body like '@claim-winner-finish completed task run%'
                "#,
            )
            .bind(channel_id)
            .bind(assigned_message_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(assigned_messages, 1);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn conversational_work_item_finish_does_not_insert_system_message() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "conversation-agent").await?;
            let channel_id = insert_test_channel(&pool, "conversation-finish").await?;
            let source_message_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'Please answer in thread', false)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let work_item_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_work_items (
                    agent_id, channel_id, thread_root_id, source_message_id,
                    source_kind, title, context, status, completed_at
                )
                values ($1, $2, $3, $3, 'mention', 'Please answer in thread', 'context', 'done', strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
                returning id
                "#,
            )
            .bind(agent_id)
            .bind(channel_id)
            .bind(source_message_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            notify_ui_work_item_changed(&pool, work_item_id, "work_item_finished").await;

            let system_messages: i64 = sqlx::query_scalar(
                r#"
                select count(*)
                from messages
                where channel_id = $1
                  and thread_root_id = $2
                  and sender_role = 'system'
                  and body like '@conversation-agent completed agent request%'
                "#,
            )
            .bind(channel_id)
            .bind(source_message_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(system_messages, 0);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn task_create_event_creates_root_task_and_execution_thread() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "task-thread-agent").await?;
            let channel_id = insert_test_channel(&pool, "task-thread-api").await?;
            let run_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_runs (agent_id, command, working_directory, status)
                values ($1, 'test', '', 'running')
                returning id
                "#,
            )
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            handle_agent_event(
                &pool,
                agent_id,
                run_id,
                AgentEvent::TaskCreate {
                    channel: None,
                    channel_id: Some(channel_id),
                    title: "Investigate task thread API".to_owned(),
                    body: Some("Track this as a durable task".to_owned()),
                    thread_body: Some("Starting execution in the task thread".to_owned()),
                    assign_self: Some(true),
                    status: Some("in_progress".to_owned()),
                },
            )
            .await?;

            let task_row = sqlx::query(
                r#"
                select t.number, t.status, t.assignee_agent_id, m.id as message_id, m.body, m.thread_root_id
                from tasks t
                join messages m on m.id = t.message_id
                where t.channel_id = $1
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let root_message_id: Uuid = task_row.get("message_id");
            assert_eq!(task_row.get::<String, _>("status"), "in_progress");
            assert_eq!(
                task_row.get::<Option<Uuid>, _>("assignee_agent_id"),
                Some(agent_id)
            );
            assert_eq!(task_row.get::<String, _>("body"), "Track this as a durable task");
            assert_eq!(task_row.get::<Option<Uuid>, _>("thread_root_id"), None);

            let reply_body: String = sqlx::query_scalar(
                "select body from messages where channel_id = $1 and thread_root_id = $2",
            )
            .bind(channel_id)
            .bind(root_message_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(reply_body, "Starting execution in the task thread");

            let subscribed: bool = sqlx::query_scalar(
                r#"
                select exists (
                    select 1
                    from agent_thread_subscriptions
                    where agent_id = $1 and channel_id = $2 and thread_root_id = $3
                )
                "#,
            )
            .bind(agent_id)
            .bind(channel_id)
            .bind(root_message_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert!(subscribed);
            assert!(task_row.get::<i64, _>("number") > 0);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn activity_event_records_progress_without_chat_message() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "activity-agent").await?;
            let run_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_runs (agent_id, command, working_directory, status)
                values ($1, 'test', '', 'running')
                returning id
                "#,
            )
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            handle_agent_event(
                &pool,
                agent_id,
                run_id,
                AgentEvent::Activity {
                    kind: Some("running_command".to_owned()),
                    title: "Running tests".to_owned(),
                    detail: Some("cargo test".to_owned()),
                },
            )
            .await?;

            let row = sqlx::query(
                r#"
                select kind, phase, status, title, detail
                from agent_activities
                where agent_id = $1 and run_id = $2
                "#,
            )
            .bind(agent_id)
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(row.get::<String, _>("kind"), "command");
            assert_eq!(row.get::<String, _>("phase"), "command");
            assert_eq!(row.get::<String, _>("status"), "active");
            assert_eq!(row.get::<String, _>("title"), "Running tests");
            assert_eq!(row.get::<String, _>("detail"), "cargo test");
            let messages: i64 = sqlx::query_scalar("select count(*) from messages")
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
            assert_eq!(messages, 0);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn agent_mentions_dispatch_other_agents_once_in_same_thread() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let author_id = insert_test_agent(&pool, "author").await?;
            let reviewer_id = insert_test_agent(&pool, "reviewer").await?;
            let outsider_id = insert_test_agent(&pool, "outsider").await?;
            let channel_id = insert_test_channel(&pool, "collab").await?;
            sqlx::query(
                r#"
                insert into channel_members (channel_id, agent_id)
                values ($1, $2), ($1, $3)
                "#,
            )
            .bind(channel_id)
            .bind(author_id)
            .bind(reviewer_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let message_id = insert_agent_message(
                &pool,
                author_id,
                channel_id,
                None,
                "@author ignore self, @reviewer please review this, @outsider ignore non-member",
                false,
            )
            .await?;
            queue_mentions_as_work_items(
                &pool,
                channel_id,
                None,
                message_id,
                None,
                "@reviewer please review this again",
                MentionDispatchOrigin::Agent {
                    sender_agent_id: author_id,
                    allow_channel_member_invite: false,
                },
            )
            .await?;

            let work_items = sqlx::query(
                r#"
                select agent_id, thread_root_id, source_message_id
                from agent_work_items
                where source_message_id = $1
                "#,
            )
            .bind(message_id)
            .fetch_all(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(work_items.len(), 1);
            assert_eq!(work_items[0].get::<Uuid, _>("agent_id"), reviewer_id);
            assert_ne!(work_items[0].get::<Uuid, _>("agent_id"), outsider_id);
            assert_eq!(
                work_items[0].get::<Option<Uuid>, _>("thread_root_id"),
                Some(message_id)
            );
            assert_eq!(
                work_items[0].get::<Option<Uuid>, _>("source_message_id"),
                Some(message_id)
            );
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn agent_mentions_do_not_cross_dispatch_from_dm() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let dm_agent_id = insert_test_agent(&pool, "dm-agent").await?;
            let reviewer_id = insert_test_agent(&pool, "dm-reviewer").await?;
            let dm_channel_id =
                Uuid::parse_str(&open_dm_with_agent_in_pool(&pool, dm_agent_id).await?)
                    .map_err(|err| err.to_string())?;

            insert_agent_message(
                &pool,
                dm_agent_id,
                dm_channel_id,
                None,
                "@dm-reviewer please join this DM",
                false,
            )
            .await?;
            let work_items: i64 = sqlx::query_scalar(
                r#"
                select count(*)
                from agent_work_items
                where channel_id = $1 and agent_id = $2
                "#,
            )
            .bind(dm_channel_id)
            .bind(reviewer_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(work_items, 0);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn channel_agent_roster_excludes_current_agent() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let current_id = insert_test_agent(&pool, "current").await?;
            let peer_id = insert_test_agent(&pool, "peer").await?;
            let channel_id = insert_test_channel(&pool, "roster").await?;
            sqlx::query(
                r#"
                insert into channel_members (channel_id, agent_id)
                values ($1, $2), ($1, $3)
                "#,
            )
            .bind(channel_id)
            .bind(current_id)
            .bind(peer_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let roster = load_channel_agent_roster(&pool, Some(channel_id), current_id).await?;
            assert_eq!(roster.len(), 1);
            assert!(roster[0].contains("@peer"));
            assert!(!roster[0].contains("@current"));
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[tokio::test]
    async fn agent_collaboration_pauses_after_thread_limit() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let author_id = insert_test_agent(&pool, "loop-author").await?;
            let reviewer_id = insert_test_agent(&pool, "loop-reviewer").await?;
            let channel_id = insert_test_channel(&pool, "loop-guard").await?;
            sqlx::query(
                r#"
                insert into channel_members (channel_id, agent_id)
                values ($1, $2), ($1, $3)
                "#,
            )
            .bind(channel_id)
            .bind(author_id)
            .bind(reviewer_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let root_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'start collaboration', false)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            for index in 0..10 {
                insert_agent_message(
                    &pool,
                    author_id,
                    channel_id,
                    Some(root_id),
                    &format!("agent loop message {index}"),
                    false,
                )
                .await?;
            }
            insert_agent_message(
                &pool,
                author_id,
                channel_id,
                Some(root_id),
                "@loop-reviewer continue the loop",
                false,
            )
            .await?;

            let work_items: i64 = sqlx::query_scalar(
                r#"
                select count(*)
                from agent_work_items
                where channel_id = $1 and agent_id = $2
                "#,
            )
            .bind(channel_id)
            .bind(reviewer_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(work_items, 0);
            let system_messages: i64 = sqlx::query_scalar(
                r#"
                select count(*)
                from messages
                where channel_id = $1
                  and thread_root_id = $2
                  and sender_role = 'system'
                  and body like 'Inter-agent collaboration paused:%'
                "#,
            )
            .bind(channel_id)
            .bind(root_id)
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
}
