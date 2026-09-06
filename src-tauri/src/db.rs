use std::{env, fs, path::PathBuf, time::Duration};

#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::str::FromStr;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    Row, SqlitePool,
};
use uuid::Uuid;

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

async fn ensure_column(
    pool: &SqlitePool,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), sqlx::Error> {
    let columns = sqlx::query(&format!("pragma table_info('{table}')"))
        .fetch_all(pool)
        .await?;
    if columns
        .iter()
        .any(|row| row.get::<String, _>("name") == column)
    {
        return Ok(());
    }
    sqlx::query(&format!(
        "alter table {table} add column {column} {definition}"
    ))
    .execute(pool)
    .await?;
    Ok(())
}

async fn ensure_text_column(
    pool: &SqlitePool,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), sqlx::Error> {
    ensure_column(pool, table, column, definition).await
}

async fn ensure_integer_column(
    pool: &SqlitePool,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), sqlx::Error> {
    ensure_column(pool, table, column, definition).await
}

fn next_recurring_due_after(
    anchor_at: DateTime<Utc>,
    now: DateTime<Utc>,
    recurrence: &str,
) -> Option<DateTime<Utc>> {
    if anchor_at > now {
        return Some(anchor_at);
    }
    let interval_days = match recurrence {
        "daily" => 1,
        "weekly" => 7,
        _ => return None,
    };
    let elapsed_days = now.signed_duration_since(anchor_at).num_days();
    let elapsed_intervals = elapsed_days / interval_days;
    anchor_at.checked_add_signed(ChronoDuration::days(
        (elapsed_intervals + 1) * interval_days,
    ))
}

async fn backfill_reminder_recurrence_anchors(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let rows = sqlx::query(
        r#"
        select
            r.id,
            r.status,
            r.recurrence,
            r.due_at,
            coalesce(
                (
                    select e.detail
                    from reminder_events e
                    where e.reminder_id = r.id
                      and e.event_type = 'created'
                      and julianday(e.detail) is not null
                    order by e.created_at asc, e.rowid asc
                    limit 1
                ),
                r.due_at
            ) as anchor_at,
            (
                select e.event_type
                from reminder_events e
                where e.reminder_id = r.id
                order by e.created_at desc, e.rowid desc
                limit 1
            ) as latest_event_type
        from reminders r
        where r.recurrence_anchor_at is null
        "#,
    )
    .fetch_all(&mut *transaction)
    .await?;
    let now = Utc::now();
    for row in rows {
        let reminder_id: Uuid = row.get("id");
        let status: String = row.get("status");
        let recurrence: String = row.get("recurrence");
        let due_at: String = row.get("due_at");
        let anchor_at: String = row.get("anchor_at");
        let latest_event_type: Option<String> = row.get("latest_event_type");
        let repaired_due_at = if status == "scheduled"
            && matches!(recurrence.as_str(), "daily" | "weekly")
            && latest_event_type.as_deref() != Some("snoozed")
        {
            let parsed_due_at = DateTime::parse_from_rfc3339(&due_at)
                .ok()
                .map(|value| value.with_timezone(&Utc));
            let parsed_anchor_at = DateTime::parse_from_rfc3339(&anchor_at)
                .ok()
                .map(|value| value.with_timezone(&Utc));
            match (parsed_due_at, parsed_anchor_at) {
                (Some(parsed_due_at), Some(parsed_anchor_at)) if parsed_due_at > now => {
                    next_recurring_due_after(parsed_anchor_at, now, &recurrence)
                        .map(|value| value.to_rfc3339())
                        .unwrap_or_else(|| due_at.clone())
                }
                _ => due_at.clone(),
            }
        } else {
            due_at.clone()
        };
        sqlx::query(
            r#"
            update reminders
            set recurrence_anchor_at = $2,
                due_at = $3
            where id = $1
              and recurrence_anchor_at is null
            "#,
        )
        .bind(reminder_id)
        .bind(anchor_at)
        .bind(repaired_due_at)
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await?;
    Ok(())
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
            model text not null default 'gpt-5.6-sol',
            avatar text not null default '',
            description text not null default '',
            launch_command text not null default '',
            environment_variables text not null default '',
            working_directory text not null default '',
            daily_budget_micros integer not null default 0,
            reasoning_effort text not null default 'medium',
            service_tier text not null default '',
            created_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
        )
        "#,
        r#"
        create table if not exists agent_subscription_status (
            agent_id blob primary key not null references agents(id) on delete cascade,
            provider text not null,
            plan text,
            status text not null,
            windows_json text not null default '[]',
            observed_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
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
        create table if not exists channel_github_repositories (
            channel_id blob primary key not null references channels(id) on delete cascade,
            repository_id text not null,
            name_with_owner text not null,
            url text not null,
            local_path text not null default '',
            account_login text not null,
            review_login text not null,
            review_queue_synced_at text,
            issue_queue_synced_at text,
            review_attention_synced_at text,
            review_attention_initialized boolean not null default 0,
            created_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
            updated_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
        )
        "#,
        r#"
        create table if not exists github_review_request_cache (
            channel_id blob not null references channels(id) on delete cascade,
            repository_id text not null,
            review_login text not null,
            pull_number integer not null,
            title text not null,
            url text not null,
            author_login text not null,
            is_draft boolean not null,
            state text not null,
            github_updated_at text not null,
            is_review_requested boolean not null default 1,
            is_authored boolean not null default 0,
            head_sha text not null default '',
            checks_status text not null default 'none',
            checks_total integer not null default 0,
            checks_pending integer not null default 0,
            checks_failed integer not null default 0,
            failing_checks_json text not null default '[]',
            review_commits_ahead integer,
            attention_unread boolean not null default 0,
            cached_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
            primary key (channel_id, repository_id, review_login, pull_number)
        )
        "#,
        r#"
        create table if not exists github_commit_comparisons (
            repository_id text not null,
            base_sha text not null,
            head_sha text not null,
            ahead_by integer not null check (ahead_by >= 0),
            primary key (repository_id, base_sha, head_sha)
        )
        "#,
        r#"
        create table if not exists github_issue_cache (
            channel_id blob not null references channels(id) on delete cascade,
            repository_id text not null,
            queue_login text not null,
            issue_number integer not null,
            title text not null,
            url text not null,
            author_login text not null,
            state text not null,
            created_at text not null,
            github_updated_at text not null,
            comments_count integer not null,
            labels_json text not null,
            assignees_json text not null,
            is_related boolean not null,
            cached_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
            primary key (channel_id, repository_id, queue_login, issue_number)
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
            seq integer not null default 0,
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
        create table if not exists github_resource_threads (
            id blob primary key not null default (randomblob(16)),
            channel_id blob not null references channels(id) on delete cascade,
            repository_id text not null,
            resource_kind text not null,
            resource_number integer not null,
            resource_url text not null,
            thread_root_id blob not null references messages(id) on delete cascade,
            task_id blob references tasks(id) on delete set null,
            head_sha text not null default '',
            created_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
            updated_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
            unique(channel_id, repository_id, resource_kind, resource_number)
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
            recurrence_anchor_at text,
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
            current_input_tokens integer not null default 0,
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
            context_max_seq integer not null default 0,
            freshness_generation integer not null default 0,
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
        create table if not exists agent_target_watermarks (
            agent_id blob not null references agents(id) on delete cascade,
            channel_id blob not null references channels(id) on delete cascade,
            thread_root_id blob references messages(id) on delete cascade,
            last_seen_seq integer not null default 0,
            updated_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
        )
        "#,
        r#"
        create table if not exists agent_held_outputs (
            id blob primary key not null default (randomblob(16)),
            agent_id blob not null references agents(id) on delete cascade,
            run_id blob references agent_runs(id) on delete set null,
            work_item_id blob references agent_work_items(id) on delete set null,
            retry_work_item_id blob references agent_work_items(id) on delete set null,
            channel_id blob not null references channels(id) on delete cascade,
            thread_root_id blob references messages(id) on delete cascade,
            output_kind text not null,
            body text not null default '',
            seen_seq integer not null default 0,
            latest_seq integer not null default 0,
            latest_message_id blob references messages(id) on delete set null,
            state text not null default 'held',
            reason text not null default '',
            created_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
            expires_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now', '+60 minutes')),
            resolved_at text
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
        r#"
        create table if not exists channel_wiki_revisions (
            id blob primary key not null default (randomblob(16)),
            channel_id blob not null references channels(id) on delete cascade,
            parent_id blob references channel_wiki_revisions(id),
            content text not null,
            author text not null default '',
            note text not null default '',
            created_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
        )
        "#,
        r#"
        create index if not exists idx_channel_wiki_revisions_channel
            on channel_wiki_revisions (channel_id, created_at desc)
        "#,
        r#"
        create table if not exists channel_wiki_heads (
            channel_id blob primary key not null references channels(id) on delete cascade,
            head_id blob not null references channel_wiki_revisions(id),
            updated_at text not null default (strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
        )
        "#,
    ] {
        sqlx::query(statement).execute(pool).await?;
    }

    ensure_text_column(
        pool,
        "agents",
        "environment_variables",
        "text not null default ''",
    )
    .await?;
    ensure_text_column(
        pool,
        "channel_github_repositories",
        "review_queue_synced_at",
        "text",
    )
    .await?;
    ensure_text_column(
        pool,
        "channel_github_repositories",
        "issue_queue_synced_at",
        "text",
    )
    .await?;
    ensure_text_column(
        pool,
        "channel_github_repositories",
        "review_attention_synced_at",
        "text",
    )
    .await?;
    ensure_integer_column(
        pool,
        "channel_github_repositories",
        "review_attention_initialized",
        "boolean not null default 0",
    )
    .await?;
    ensure_integer_column(
        pool,
        "github_review_request_cache",
        "is_review_requested",
        "boolean not null default 1",
    )
    .await?;
    ensure_integer_column(
        pool,
        "github_review_request_cache",
        "is_authored",
        "boolean not null default 0",
    )
    .await?;
    ensure_text_column(
        pool,
        "github_review_request_cache",
        "head_sha",
        "text not null default ''",
    )
    .await?;
    ensure_text_column(
        pool,
        "github_review_request_cache",
        "checks_status",
        "text not null default 'none'",
    )
    .await?;
    ensure_integer_column(
        pool,
        "github_review_request_cache",
        "checks_total",
        "integer not null default 0",
    )
    .await?;
    ensure_integer_column(
        pool,
        "github_review_request_cache",
        "checks_pending",
        "integer not null default 0",
    )
    .await?;
    ensure_integer_column(
        pool,
        "github_review_request_cache",
        "checks_failed",
        "integer not null default 0",
    )
    .await?;
    ensure_text_column(
        pool,
        "github_review_request_cache",
        "failing_checks_json",
        "text not null default '[]'",
    )
    .await?;
    ensure_integer_column(
        pool,
        "github_review_request_cache",
        "review_commits_ahead",
        "integer",
    )
    .await?;
    ensure_integer_column(
        pool,
        "github_review_request_cache",
        "attention_unread",
        "boolean not null default 0",
    )
    .await?;
    ensure_integer_column(pool, "messages", "seq", "integer not null default 0").await?;
    ensure_integer_column(
        pool,
        "agent_work_items",
        "context_max_seq",
        "integer not null default 0",
    )
    .await?;
    ensure_integer_column(
        pool,
        "agent_work_items",
        "freshness_generation",
        "integer not null default 0",
    )
    .await?;
    ensure_integer_column(
        pool,
        "agent_runs",
        "current_input_tokens",
        "integer not null default 0",
    )
    .await?;
    ensure_text_column(pool, "reminders", "recurrence_anchor_at", "text").await?;
    backfill_reminder_recurrence_anchors(pool).await?;

    sqlx::query(
        r#"
        with existing_max as (
            select coalesce(max(seq), 0) as max_seq
            from messages
            where seq > 0
        ),
        ordered as (
            select
                id,
                existing_max.max_seq + row_number() over (order by julianday(created_at) asc, created_at asc, rowid asc) as next_seq
            from messages
            cross join existing_max
            where seq <= 0
        )
        update messages
        set seq = (select next_seq from ordered where ordered.id = messages.id)
        where seq <= 0
        "#,
    )
    .execute(pool)
    .await?;

    for statement in [
        "create unique index if not exists channels_dm_unique on channels(dm_agent_id) where kind = 'dm' and dm_agent_id is not null",
        "create unique index if not exists messages_stream_key_unique on messages(stream_key) where stream_key <> ''",
        "create index if not exists messages_seq_idx on messages(seq)",
        "create index if not exists messages_channel_seq_idx on messages(channel_id, seq desc)",
        "create index if not exists messages_channel_root_seq_idx on messages(channel_id, seq desc) where thread_root_id is null",
        "create index if not exists messages_channel_created_idx on messages(channel_id, created_at desc)",
        "create index if not exists messages_thread_root_idx on messages(thread_root_id) where thread_root_id is not null",
        "create index if not exists message_attachments_message_id_idx on message_attachments(message_id)",
        "create index if not exists saved_messages_created_at_idx on saved_messages(created_at desc)",
        "create index if not exists artifacts_message_id_idx on artifacts(message_id)",
        "create index if not exists artifacts_channel_id_idx on artifacts(channel_id)",
        "create index if not exists github_resource_threads_thread_idx on github_resource_threads(thread_root_id)",
        "create index if not exists github_resource_threads_task_idx on github_resource_threads(task_id) where task_id is not null",
        "create index if not exists agent_runs_started_idx on agent_runs(started_at desc)",
        "create index if not exists agent_work_items_created_idx on agent_work_items(created_at desc)",
        "create index if not exists agent_activities_agent_created_idx on agent_activities(agent_id, agent_handle, created_at desc)",
        r#"
        create index if not exists agent_activities_owner_instant_idx on agent_activities(
            coalesce(
                case when agent_id is null then null else lower(hex(agent_id)) end,
                nullif(agent_handle, ''),
                'unknown'
            ),
            julianday(created_at) desc,
            created_at desc
        )
        "#,
        "create index if not exists reminders_due_idx on reminders(status, due_at)",
        "create index if not exists agent_schedules_due_idx on agent_schedules(status, next_run_at)",
        "create index if not exists agent_inbox_items_agent_state_idx on agent_inbox_items(agent_id, state, priority desc, created_at)",
        "create unique index if not exists agent_inbox_items_source_unique on agent_inbox_items(agent_id, source_message_id, kind) where source_message_id is not null",
        "create unique index if not exists agent_target_watermarks_root_unique on agent_target_watermarks(agent_id, channel_id) where thread_root_id is null",
        "create unique index if not exists agent_target_watermarks_thread_unique on agent_target_watermarks(agent_id, channel_id, thread_root_id) where thread_root_id is not null",
        "create index if not exists agent_held_outputs_state_expires_idx on agent_held_outputs(state, expires_at)",
        "create index if not exists agent_held_outputs_work_item_idx on agent_held_outputs(work_item_id)",
        r#"
        create trigger if not exists messages_seq_after_insert
        after insert on messages
        when new.seq <= 0
        begin
            update messages
            set seq = (
                select coalesce(max(seq), 0) + 1
                from messages
            )
            where id = new.id;
        end
        "#,
        // `ui_events` is an ephemeral refresh queue read only by id (PK); the
        // old created_at index was never used and is pruned by row id, so drop
        // it on existing databases to reclaim its space.
        "drop index if exists ui_events_created_idx",
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
    use super::{
        acquire_supervisor_lock, backfill_reminder_recurrence_anchors, sqlite_database_file_path,
    };
    use crate::test_support::{drop_test_schema, insert_test_channel, test_pool};

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

    #[tokio::test]
    async fn recurring_reminder_anchor_backfill_repairs_drift_without_losing_pending_work() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let channel_id = insert_test_channel(&pool, "reminder-anchor-backfill").await?;
            let drifted_id: uuid::Uuid = sqlx::query_scalar(
                r#"
                insert into reminders (
                    channel_id, title, due_at, recurrence, status
                )
                values (
                    $1,
                    'Drifted daily reminder',
                    strftime('%Y-%m-%dT12:14:40.000+00:00','now','+1 day'),
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
            sqlx::query(
                r#"
                insert into reminder_events (reminder_id, event_type, detail)
                values (
                    $1,
                    'created',
                    strftime('%Y-%m-%dT01:00:00.000+00:00','now','-3 days')
                )
                "#,
            )
            .bind(drifted_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let overdue_id: uuid::Uuid = sqlx::query_scalar(
                r#"
                insert into reminders (
                    channel_id, title, due_at, recurrence, status
                )
                values (
                    $1,
                    'Overdue daily reminder',
                    strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-1 minute'),
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
            sqlx::query(
                r#"
                insert into reminder_events (reminder_id, event_type, detail)
                values (
                    $1,
                    'created',
                    strftime('%Y-%m-%dT03:00:00.000+00:00','now','-3 days')
                )
                "#,
            )
            .bind(overdue_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let overdue_before: String =
                sqlx::query_scalar("select due_at from reminders where id = $1")
                    .bind(overdue_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;

            let snoozed_id: uuid::Uuid = sqlx::query_scalar(
                r#"
                insert into reminders (
                    channel_id, title, due_at, recurrence, status
                )
                values (
                    $1,
                    'Snoozed daily reminder',
                    strftime('%Y-%m-%dT12:30:00.000+00:00','now','+1 day'),
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
            sqlx::query(
                r#"
                insert into reminder_events (reminder_id, event_type, detail)
                values
                    (
                        $1,
                        'created',
                        strftime('%Y-%m-%dT04:00:00.000+00:00','now','-3 days')
                    ),
                    ($1, 'snoozed', '60 minutes')
                "#,
            )
            .bind(snoozed_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let snoozed_before: String =
                sqlx::query_scalar("select due_at from reminders where id = $1")
                    .bind(snoozed_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;

            backfill_reminder_recurrence_anchors(&pool)
                .await
                .map_err(|err| err.to_string())?;
            backfill_reminder_recurrence_anchors(&pool)
                .await
                .map_err(|err| err.to_string())?;

            let (drifted_anchor, drifted_due): (String, String) =
                sqlx::query_as("select recurrence_anchor_at, due_at from reminders where id = $1")
                    .bind(drifted_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert!(drifted_anchor.contains("T01:00:00"));
            assert!(
                drifted_due.contains("T01:00:00"),
                "drifted reminder should return to its original wall-clock time: {drifted_due}"
            );
            let now: String =
                sqlx::query_scalar("select strftime('%Y-%m-%dT%H:%M:%f+00:00','now')")
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert!(drifted_due > now);

            let (overdue_anchor, overdue_after): (String, String) =
                sqlx::query_as("select recurrence_anchor_at, due_at from reminders where id = $1")
                    .bind(overdue_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert!(overdue_anchor.contains("T03:00:00"));
            assert_eq!(overdue_after, overdue_before);

            let (snoozed_anchor, snoozed_after): (String, String) =
                sqlx::query_as("select recurrence_anchor_at, due_at from reminders where id = $1")
                    .bind(snoozed_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert!(snoozed_anchor.contains("T04:00:00"));
            assert_eq!(snoozed_after, snoozed_before);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn messages_seq_is_assigned_on_insert() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let channel_id = insert_test_channel(&pool, "seq-migration").await?;
            let first_id: uuid::Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'first', false)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let second_id: uuid::Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'second', false)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let first_seq: i64 = sqlx::query_scalar("select seq from messages where id = $1")
                .bind(first_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
            let second_seq: i64 = sqlx::query_scalar("select seq from messages where id = $1")
                .bind(second_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
            assert!(first_seq > 0);
            assert!(second_seq > first_seq);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }
}
