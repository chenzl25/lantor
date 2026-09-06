//! One committed SQLite event reader per database/process. Web connections and
//! the desktop listener share payloads, including writes from the supervisor.
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, OnceLock, Weak},
    time::Duration,
};

use sqlx::{Connection, SqliteConnection, SqlitePool};
use tokio::{
    sync::{broadcast, Mutex},
    task::AbortHandle,
};

use crate::{
    app::{to_string, CommandResult},
    ui_notifications::{load_ui_event_replay_from_cursor, read_ui_event_replay, UiEventReplay},
};

// data_version reads connection-local metadata, not ui_events. Keeping one
// dedicated read connection is essential: values cannot be compared across
// pooled connections. This also observes unmodified/older supervisor writers.
const OBSERVE_INTERVAL: Duration = Duration::from_millis(40);
const LIVE_BATCH_CAPACITY: usize = 64;

struct Hub {
    sender: broadcast::Sender<Arc<UiEventReplay>>,
    observer: AbortHandle,
    #[cfg(test)]
    stats: Arc<Stats>,
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.observer.abort();
    }
}

type Registry = Mutex<HashMap<PathBuf, Weak<Hub>>>;
static HUBS: OnceLock<Registry> = OnceLock::new();

async fn shared_hub(pool: &SqlitePool) -> CommandResult<Arc<Hub>> {
    let options = pool.connect_options();
    let path = options.get_filename();
    let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut hubs = HUBS.get_or_init(Registry::default).lock().await;
    hubs.retain(|_, hub| hub.strong_count() > 0);
    if let Some(hub) = hubs.get(&key).and_then(Weak::upgrade) {
        return Ok(hub);
    }
    let options = if path.exists() {
        options
            .as_ref()
            .clone()
            .read_only(true)
            .create_if_missing(false)
    } else {
        options.as_ref().clone()
    };
    let mut connection = SqliteConnection::connect_with(&options)
        .await
        .map_err(to_string)?;
    let mut version: i64 = sqlx::query_scalar("pragma data_version")
        .fetch_one(&mut connection)
        .await
        .map_err(to_string)?;
    let mut cursor: i64 = sqlx::query_scalar("select coalesce(max(id), 0) from ui_events")
        .fetch_one(&mut connection)
        .await
        .map_err(to_string)?;
    let (sender, _) = broadcast::channel(LIVE_BATCH_CAPACITY);
    let deliveries = sender.clone();
    #[cfg(test)]
    let stats = Arc::new(Stats::default());
    #[cfg(test)]
    let observer_stats = stats.clone();
    let task = tokio::spawn(async move {
        let mut timer = tokio::time::interval(OBSERVE_INTERVAL);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            #[cfg(test)]
            observer_stats
                .pragma_reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let current = match sqlx::query_scalar::<_, i64>("pragma data_version")
                .fetch_one(&mut connection)
                .await
            {
                Ok(current) => current,
                Err(err) => {
                    eprintln!("Lantor event observer failed: {err}");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    if let Ok(reconnected) = SqliteConnection::connect_with(&options).await {
                        connection = reconnected;
                        version = -1; // data_version belongs to the new connection.
                    }
                    continue;
                }
            };
            if current == version {
                continue;
            }
            #[cfg(test)]
            observer_stats
                .event_reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            match read_ui_event_replay(&mut connection, cursor).await {
                Ok(batch) => {
                    // Advance only after a successful read. A commit racing the
                    // read changes data_version again and is drained next tick.
                    version = current;
                    cursor = batch.cursor;
                    if batch.replay_gap || !batch.events.is_empty() {
                        let _ = deliveries.send(Arc::new(batch));
                    }
                }
                Err(err) => {
                    eprintln!("Lantor event reader failed: {err}");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    if let Ok(reconnected) = SqliteConnection::connect_with(&options).await {
                        connection = reconnected;
                        version = -1;
                    }
                }
            }
        }
    });
    let hub = Arc::new(Hub {
        sender,
        observer: task.abort_handle(),
        #[cfg(test)]
        stats,
    });
    hubs.insert(key, Arc::downgrade(&hub));
    Ok(hub)
}

pub(crate) struct UiEventSubscription {
    _hub: Arc<Hub>,
    receiver: broadcast::Receiver<Arc<UiEventReplay>>,
    pool: SqlitePool,
    cursor: i64,
    initial: Option<UiEventReplay>,
}

impl UiEventSubscription {
    #[cfg(test)]
    pub(crate) fn observer_counts(&self) -> (u64, u64) {
        use std::sync::atomic::Ordering;
        (
            self._hub.stats.pragma_reads.load(Ordering::Relaxed),
            self._hub.stats.event_reads.load(Ordering::Relaxed),
        )
    }

    pub(crate) async fn connect(
        pool: &SqlitePool,
        requested_cursor: Option<i64>,
    ) -> CommandResult<Self> {
        let hub = shared_hub(pool).await?;
        // Subscribe before capturing the replay boundary; then discard overlap.
        // There is no lost event between the DB snapshot and live delivery.
        let receiver = hub.sender.subscribe();
        let initial = if let Some(cursor) = requested_cursor {
            load_ui_event_replay_from_cursor(pool, cursor).await?
        } else {
            UiEventReplay {
                cursor: sqlx::query_scalar("select coalesce(max(id), 0) from ui_events")
                    .fetch_one(pool)
                    .await
                    .map_err(to_string)?,
                replay_gap: false,
                events: Vec::new(),
            }
        };
        Ok(Self {
            _hub: hub,
            receiver,
            pool: pool.clone(),
            cursor: initial.cursor,
            initial: Some(initial),
        })
    }

    pub(crate) async fn recv(&mut self) -> CommandResult<UiEventReplay> {
        if let Some(initial) = self.initial.take() {
            return Ok(initial);
        }
        loop {
            let batch = match self.receiver.recv().await {
                Ok(batch) if !batch.replay_gap => {
                    if batch.cursor <= self.cursor {
                        continue;
                    }
                    UiEventReplay {
                        cursor: batch.cursor,
                        replay_gap: false,
                        events: batch
                            .events
                            .iter()
                            .filter(|event| event.cursor > self.cursor)
                            .cloned()
                            .collect(),
                    }
                }
                // Only a lagged receiver or a changed replay window goes back
                // to SQLite. Normal fan-out never reads once per subscriber.
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {
                    load_ui_event_replay_from_cursor(&self.pool, self.cursor).await?
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return Err("UI event hub closed".into())
                }
            };
            self.cursor = batch.cursor;
            if batch.replay_gap || !batch.events.is_empty() {
                return Ok(batch);
            }
        }
    }
}

#[cfg(test)]
#[derive(Default)]
struct Stats {
    pragma_reads: std::sync::atomic::AtomicU64,
    event_reads: std::sync::atomic::AtomicU64,
}

#[cfg(test)]
#[path = "tests/ui_event_hub.rs"]
mod tests;
