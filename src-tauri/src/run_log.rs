use std::{fs, io::Write};

use sqlx::SqlitePool;
use uuid::Uuid;

use crate::app::{to_string, CommandResult};

pub(crate) async fn append_run_log(
    pool: &SqlitePool,
    run_id: Uuid,
    line: String,
) -> CommandResult<()> {
    // Keep the small database tail for the UI, and the complete provider output
    // beside the database for diagnosis. Archive failure must not drop an event
    // from the runtime reader: keep its text and the failure in the DB fallback.
    let line = match append_run_log_file(pool, run_id, &line) {
        Ok(()) => line,
        Err(error) => format!("{line}\n[run log archive unavailable: {error}]\n"),
    };
    sqlx::query("update agent_runs set log = substr(log || $2, -20000) where id = $1")
        .bind(run_id)
        .bind(line)
        .execute(pool)
        .await
        .map_err(to_string)?;

    Ok(())
}

fn append_run_log_file(pool: &SqlitePool, run_id: Uuid, line: &str) -> CommandResult<()> {
    let options = pool.connect_options();
    let database = options.get_filename();
    if database.as_os_str().is_empty() || database == std::path::Path::new(":memory:") {
        return Ok(());
    }
    let directory = database.with_extension("run-logs");
    fs::create_dir_all(&directory).map_err(to_string)?;
    let mut options = fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(directory.join(format!("{run_id}.log")))
        .and_then(|mut file| file.write_all(line.as_bytes()))
        .map_err(to_string)
}

#[cfg(test)]
#[path = "tests/run_log.rs"]
mod relocated_tests;
