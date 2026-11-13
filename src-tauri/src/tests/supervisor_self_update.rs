use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use chrono::{DateTime, Utc};
use sqlx::{Row, SqlitePool};
use tokio::{sync::Semaphore, time::sleep};
use uuid::Uuid;

use super::{
    run_supervisor_iteration, supervisor_is_idle, SupervisorIteration,
    SUPERVISOR_COMMAND_CONCURRENCY,
};
use crate::runtime::{
    claude::WarmClaudeRegistry,
    codex::WarmCodexRegistry,
    self_update::{ExecutableFile, SelfUpdate},
};
use crate::test_support::{drop_test_schema, insert_test_agent, test_pool};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lantor-{name}-{}", Uuid::new_v4().simple()));
    fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// A self update whose executable at `path` has been replaced by a newer
/// build that waits to take over.
fn pending_update(path: &Path) -> SelfUpdate {
    fs::write(path, "running build").expect("write executable");
    let running = ExecutableFile::read(path).expect("read executable");
    let mut update = SelfUpdate::new(path.to_path_buf(), running);
    let replacement = path.with_extension("next");
    fs::write(&replacement, "a newer, longer build").expect("write newer build");
    fs::rename(&replacement, path).expect("replace executable");
    update.check();
    update.check();
    assert!(update.pending().is_some());
    update
}

async fn set_status(pool: &SqlitePool, table: &str, id: Uuid, status: &str) -> Result<(), String> {
    sqlx::query(&format!("update {table} set status = $1 where id = $2"))
        .bind(status)
        .bind(id)
        .execute(pool)
        .await
        .map(|_| ())
        .map_err(|err| err.to_string())
}

async fn insert_work_item(pool: &SqlitePool, agent_id: Uuid, status: &str) -> Result<Uuid, String> {
    sqlx::query_scalar(
        "insert into agent_work_items (agent_id, title, status) values ($1, 'work', $2) returning id",
    )
    .bind(agent_id)
    .bind(status)
    .fetch_one(pool)
    .await
    .map_err(|err| err.to_string())
}

async fn start_commands(pool: &SqlitePool) -> Result<i64, String> {
    sqlx::query_scalar(
        "select count(*) from supervisor_commands where command_type = 'start_agent'",
    )
    .fetch_one(pool)
    .await
    .map_err(|err| err.to_string())
}

async fn is_idle(pool: &SqlitePool, commands: &Semaphore) -> Result<bool, String> {
    supervisor_is_idle(
        pool,
        &WarmCodexRegistry::default(),
        &WarmClaudeRegistry::default(),
        commands,
    )
    .await
}

/// One pass of the supervisor loop, with no maintenance due.
async fn pass(
    pool: &SqlitePool,
    commands: &Arc<Semaphore>,
    update: &mut SelfUpdate,
) -> Result<SupervisorIteration, String> {
    run_supervisor_iteration(
        pool,
        &WarmCodexRegistry::default(),
        &WarmClaudeRegistry::default(),
        commands,
        Some(update),
        &mut Instant::now(),
        &mut Instant::now(),
    )
    .await
}

#[tokio::test]
async fn idle_means_no_request_run_or_command_in_progress() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let commands = Semaphore::new(SUPERVISOR_COMMAND_CONCURRENCY);
        assert!(is_idle(&pool, &commands).await?);

        // Queued requests wait for the next supervisor.
        let agent_id = insert_test_agent(&pool, "self-update-idle").await?;
        insert_work_item(&pool, agent_id, "queued").await?;
        assert!(is_idle(&pool, &commands).await?);

        for status in ["running", "cancelling"] {
            let work_item_id = insert_work_item(&pool, agent_id, status).await?;
            assert!(!is_idle(&pool, &commands).await?, "a {status} request");
            set_status(&pool, "agent_work_items", work_item_id, "done").await?;
        }

        let command_id: Uuid = sqlx::query_scalar(
            "insert into supervisor_commands (command_type, agent_id, status) values ('start_agent', $1, 'running') returning id",
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert!(!is_idle(&pool, &commands).await?, "a command being carried out");
        set_status(&pool, "supervisor_commands", command_id, "done").await?;

        let run_id: Uuid = sqlx::query_scalar(
            "insert into agent_runs (agent_id, command, status) values ($1, 'codex', 'stopping') returning id",
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert!(!is_idle(&pool, &commands).await?, "a run that has not stopped");
        sqlx::query("update agent_runs set stopped_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') where id = $1")
            .bind(run_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert!(is_idle(&pool, &commands).await?);

        // A command claimed in this process and not yet finished.
        let _permit = commands.try_acquire().map_err(|err| err.to_string())?;
        assert!(!is_idle(&pool, &commands).await?, "a command task in flight");
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn a_newer_build_takes_over_only_in_a_pass_that_starts_nothing() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let dir = temp_dir("self-update-pass");
    let result: Result<(), String> = async {
        let commands = Arc::new(Semaphore::new(SUPERVISOR_COMMAND_CONCURRENCY));
        let mut update = pending_update(&dir.join("lantor"));
        let agent_id = insert_test_agent(&pool, "self-update-pass").await?;
        let busy = insert_work_item(&pool, agent_id, "running").await?;
        let other = insert_test_agent(&pool, "self-update-other").await?;
        insert_work_item(&pool, other, "queued").await?;

        // While a request runs, work is scheduled as usual. Every command
        // permit is held, so the start is queued but not carried out.
        {
            let _permits = commands
                .try_acquire_many(SUPERVISOR_COMMAND_CONCURRENCY as u32)
                .map_err(|err| err.to_string())?;
            let outcome = pass(&pool, &commands, &mut update).await?;
            assert!(!matches!(
                outcome,
                SupervisorIteration::NewerBuildCanTakeOver
            ));
        }
        assert_eq!(start_commands(&pool).await?, 1);
        sqlx::query("delete from supervisor_commands")
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

        // Once nothing runs, the pass hands over instead of starting the
        // queued request; the next supervisor starts it.
        set_status(&pool, "agent_work_items", busy, "done").await?;
        let outcome = pass(&pool, &commands, &mut update).await?;
        assert!(matches!(
            outcome,
            SupervisorIteration::NewerBuildCanTakeOver
        ));
        assert_eq!(start_commands(&pool).await?, 0);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    let _ = fs::remove_dir_all(&dir);
    result.unwrap();
}

struct SupervisorRow {
    pid: Option<i64>,
    started_at: Option<DateTime<Utc>>,
    build_modified_at: Option<DateTime<Utc>>,
    update_waiting_since: Option<DateTime<Utc>>,
    update_error: String,
}

async fn supervisor_row(pool: &SqlitePool) -> Option<SupervisorRow> {
    let row = sqlx::query(
        "select pid, started_at, build_modified_at, update_waiting_since, update_error from supervisor_state where id = 1",
    )
    .fetch_optional(pool)
    .await
    .ok()??;
    Some(SupervisorRow {
        pid: row.get("pid"),
        started_at: row.get("started_at"),
        build_modified_at: row.get("build_modified_at"),
        update_waiting_since: row.get("update_waiting_since"),
        update_error: row.get("update_error"),
    })
}

async fn wait_for_row(
    pool: &SqlitePool,
    limit: Duration,
    what: &str,
    done: impl Fn(&SupervisorRow) -> bool,
) -> SupervisorRow {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(row) = supervisor_row(pool).await {
            if done(&row) {
                return row;
            }
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        sleep(Duration::from_millis(250)).await;
    }
}

/// Runs a copy of the built app as a supervisor on a fresh database, puts a
/// newer build at its path while a request runs, and checks that the same
/// process starts the newer build once the request is done.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "runs the built app as a supervisor for about a minute and a half; npm run test:supervisor-update"]
async fn a_newer_build_takes_over_once_nothing_is_working() {
    let built = std::env::current_exe()
        .expect("test executable")
        .parent()
        .and_then(Path::parent)
        .expect("target directory")
        .join("lantor");
    assert!(built.exists(), "build the app first: {}", built.display());
    let (pool, database_path) = test_pool().await.expect("isolated database");
    let dir = temp_dir("self-update-binary");
    let executable = dir.join("lantor");
    fs::copy(&built, &executable).expect("copy the app");
    let mut supervisor = tokio::process::Command::new(&executable)
        .arg("--supervisor")
        .env("LANTOR_DATABASE_URL", format!("sqlite://{database_path}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("start the supervisor");
    let pid = i64::from(supervisor.id().expect("supervisor pid"));

    let first = wait_for_row(
        &pool,
        Duration::from_secs(30),
        "the supervisor to start",
        |row| row.pid == Some(pid) && row.started_at.is_some(),
    )
    .await;
    let agent_id = insert_test_agent(&pool, "self-update-e2e").await.unwrap();
    let busy = insert_work_item(&pool, agent_id, "running").await.unwrap();

    // A copy keeps the original's modification time; a rebuild would not.
    let next = dir.join("lantor.next");
    fs::copy(&built, &next).expect("copy the newer build");
    fs::File::options()
        .write(true)
        .open(&next)
        .and_then(|file| file.set_modified(SystemTime::now()))
        .expect("date the newer build");
    fs::rename(&next, &executable).expect("replace the app");
    let newer_modified =
        DateTime::<Utc>::from(fs::metadata(&executable).unwrap().modified().unwrap());
    assert_ne!(first.build_modified_at, Some(newer_modified));

    wait_for_row(
        &pool,
        Duration::from_secs(90),
        "the newer build to wait",
        |row| row.update_waiting_since.is_some(),
    )
    .await;
    sleep(Duration::from_secs(5)).await;
    let waiting = supervisor_row(&pool).await.unwrap();
    assert_eq!(
        waiting.started_at, first.started_at,
        "no handover while a request runs"
    );
    assert!(supervisor.try_wait().unwrap().is_none());

    set_status(&pool, "agent_work_items", busy, "done")
        .await
        .unwrap();
    let after = wait_for_row(&pool, Duration::from_secs(30), "the handover", |row| {
        row.started_at != first.started_at
    })
    .await;
    assert_eq!(
        after.pid,
        Some(pid),
        "the same process runs the newer build"
    );
    assert_eq!(after.build_modified_at, Some(newer_modified));
    assert_eq!(after.update_waiting_since, None);
    assert_eq!(after.update_error, "");
    assert!(supervisor.try_wait().unwrap().is_none());

    let _ = supervisor.kill().await;
    drop_test_schema(pool, database_path).await;
    let _ = fs::remove_dir_all(&dir);
}
