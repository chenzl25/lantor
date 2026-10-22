use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use tokio::time::sleep;

use crate::app::CommandResult;

pub(crate) mod reminder_checks;
pub(crate) mod reminders;

pub(crate) fn spawn_reminder_worker(pool: SqlitePool) {
    tauri::async_runtime::spawn(async move {
        let mut reconciliation_pending = true;
        loop {
            if reconciliation_pending {
                match reminders::complete_successful_agent_reminders(&pool, None).await {
                    Ok(()) => reconciliation_pending = false,
                    Err(err) => eprintln!("Lantor reminder reconciliation failed: {err}"),
                }
            }
            if let Err(err) = reminders::process_due_reminders(&pool).await {
                eprintln!("Lantor reminder worker failed: {err}");
            }
            sleep(Duration::from_secs(15)).await;
        }
    });
}

pub(crate) fn parse_due_at(value: &str) -> CommandResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|err| format!("invalid reminder due_at: {err}"))
}
