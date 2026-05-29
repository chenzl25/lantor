use std::{env, fs, path::PathBuf, time::Duration};

#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::str::FromStr;

use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    SqlitePool,
};

use crate::app::{to_string, CommandResult};
use crate::usage::backfill_agent_run_usage_from_logs;

const DEFAULT_DATABASE_URL: &str = "sqlite://~/Library/Application Support/Lantor/lantor.sqlite";

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

pub(crate) fn db_url() -> String {
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

pub(crate) fn acquire_supervisor_lock(database_url: &str) -> CommandResult<Option<fs::File>> {
    let Some(database_path) = sqlite_database_file_path(database_url)? else {
        return Ok(None);
    };
    let lock_path = PathBuf::from(format!("{}.supervisor.lock", database_path.display()));
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).map_err(to_string)?;
    }
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
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

#[cfg(test)]
mod tests {
    use super::{acquire_supervisor_lock, sqlite_database_file_path};

    #[test]
    fn sqlite_database_file_path_skips_memory_database() {
        let path = sqlite_database_file_path("sqlite::memory:").expect("parse memory sqlite URL");
        assert!(path.is_none());
    }

    #[test]
    fn sqlite_database_file_path_resolves_file_database() {
        let path = sqlite_database_file_path("sqlite:///tmp/lantor-db-path-test.sqlite")
            .expect("parse sqlite URL");
        assert_eq!(
            path.expect("file database path").to_string_lossy(),
            "/tmp/lantor-db-path-test.sqlite"
        );
    }

    #[test]
    fn supervisor_lock_skips_memory_database() {
        let lock = acquire_supervisor_lock("sqlite::memory:").expect("memory DB lock check");
        assert!(lock.is_none());
    }
}
