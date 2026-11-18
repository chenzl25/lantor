//! How the supervisor learns there is something to start before its next poll.
//!
//! Work reaches the supervisor from two sides. Its own process queues requests
//! and ends turns, and says so with [`wake_supervisor`]. The app's process
//! writes queued requests and commands to the database. SQLite cannot notify
//! another process, so one dedicated connection watches `PRAGMA data_version`
//! as `ui_event_hub.rs` does and, when another connection has committed,
//! compares what is queued with what it saw last.

use std::time::Duration;

use sqlx::{sqlite::SqliteConnectOptions, Connection, SqliteConnection, SqlitePool};
use tokio::{sync::Notify, time::Instant};

static SIGNAL: Notify = Notify::const_new();

/// How often the database is checked while work has arrived recently, and
/// otherwise. A check that finds no commit reads no table.
const BUSY_CHECK_INTERVAL: Duration = Duration::from_millis(50);
const QUIET_CHECK_INTERVAL: Duration = Duration::from_millis(250);
const BUSY_PERIOD: Duration = Duration::from_secs(30);
const RECONNECT_INTERVAL: Duration = Duration::from_secs(2);

/// Tells the supervisor in this process to look for work now. A call in a
/// process without a supervisor loop does nothing.
pub(crate) fn wake_supervisor() {
    SIGNAL.notify_one();
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Wake {
    /// This process queued a request or ended a turn.
    Signalled,
    /// Another connection queued a request or a command.
    Queued,
    TimedOut,
}

pub(crate) struct SupervisorWake {
    signal: &'static Notify,
    options: SqliteConnectOptions,
    connection: Option<SqliteConnection>,
    reconnect_after: Instant,
    /// `data_version` belongs to one connection, so it is compared only with
    /// values this connection read.
    data_version: i64,
    queued: String,
    busy_until: Instant,
}

impl SupervisorWake {
    pub(crate) async fn connect(pool: &SqlitePool) -> Self {
        Self::connect_with_signal(pool, &SIGNAL).await
    }

    async fn connect_with_signal(pool: &SqlitePool, signal: &'static Notify) -> Self {
        let options = pool
            .connect_options()
            .as_ref()
            .clone()
            .read_only(true)
            .create_if_missing(false);
        let mut wake = Self {
            signal,
            options,
            connection: None,
            reconnect_after: Instant::now(),
            data_version: -1,
            queued: String::new(),
            busy_until: Instant::now(),
        };
        // What is queued now is the next pass's to start, not a reason to wake.
        wake.queued_changed().await;
        wake
    }

    /// Returns when this process signals, when another connection queued a
    /// request or a command, or after `timeout`.
    pub(crate) async fn wait(&mut self, timeout: Duration) -> Wake {
        let deadline = Instant::now() + timeout;
        loop {
            if self.queued_changed().await {
                self.busy_until = Instant::now() + BUSY_PERIOD;
                return Wake::Queued;
            }
            let now = Instant::now();
            if now >= deadline {
                return Wake::TimedOut;
            }
            let interval = if now < self.busy_until {
                BUSY_CHECK_INTERVAL
            } else {
                QUIET_CHECK_INTERVAL
            };
            tokio::select! {
                _ = self.signal.notified() => {
                    self.busy_until = Instant::now() + BUSY_PERIOD;
                    return Wake::Signalled;
                }
                _ = tokio::time::sleep(interval.min(deadline - now)) => {}
            }
        }
    }

    /// Whether another connection committed a change to what is queued. A
    /// failed read drops the connection; until it is back the caller's
    /// timeout is the poll interval.
    async fn queued_changed(&mut self) -> bool {
        if self.connection.is_none() {
            if Instant::now() < self.reconnect_after {
                return false;
            }
            self.reconnect_after = Instant::now() + RECONNECT_INTERVAL;
            self.connection = SqliteConnection::connect_with(&self.options).await.ok();
            self.data_version = -1;
        }
        let Some(connection) = self.connection.as_mut() else {
            return false;
        };
        let read = async {
            let data_version: i64 = sqlx::query_scalar("pragma data_version")
                .fetch_one(&mut *connection)
                .await?;
            if data_version == self.data_version {
                return Ok::<_, sqlx::Error>(None);
            }
            // Queued requests change when one arrives, is merged into, is
            // started or goes back to the queue; commands when one arrives.
            let queued: String = sqlx::query_scalar(
                r#"
                select
                    (select count(*) || ':' || coalesce(max(created_at), '') || ':' || coalesce(max(updated_at), '')
                     from agent_work_items where status = 'queued')
                    || '|' ||
                    (select count(*) || ':' || coalesce(max(created_at), '')
                     from supervisor_commands where status = 'pending')
                "#,
            )
            .fetch_one(&mut *connection)
            .await?;
            Ok(Some((data_version, queued)))
        };
        match read.await {
            Ok(None) => false,
            Ok(Some((data_version, queued))) => {
                let first_read = self.data_version < 0;
                self.data_version = data_version;
                let changed = queued != self.queued;
                self.queued = queued;
                changed && !first_read
            }
            Err(err) => {
                eprintln!("Lantor supervisor wake check failed: {err}");
                self.connection = None;
                false
            }
        }
    }
}

#[cfg(test)]
#[path = "../tests/runtime_wake.rs"]
mod tests;
