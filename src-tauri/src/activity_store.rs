use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::models::{AgentActivity, AgentRun, AgentWorkItem};
use crate::{to_string, CommandResult};

pub(crate) async fn load_agent_runs(pool: &SqlitePool) -> CommandResult<Vec<AgentRun>> {
    let rows = sqlx::query(
        r#"
        select
            r.id,
            r.agent_id,
            a.handle as agent_handle,
            r.work_item_id,
            r.command,
            r.working_directory,
            r.status,
            r.pid,
            r.exit_code,
            r.log,
            r.input_tokens,
            r.output_tokens,
            r.cost_micros,
            r.started_at,
            r.stopped_at
        from agent_runs r
        join agents a on a.id = r.agent_id
        order by r.started_at desc
        limit 30
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    Ok(rows
        .into_iter()
        .map(|row| AgentRun {
            id: row.get("id"),
            agent_id: row.get("agent_id"),
            agent_handle: row.get("agent_handle"),
            work_item_id: row.get("work_item_id"),
            command: row.get("command"),
            working_directory: row.get("working_directory"),
            status: row.get("status"),
            pid: row.get("pid"),
            exit_code: row.get("exit_code"),
            log: row.get("log"),
            input_tokens: row.get("input_tokens"),
            output_tokens: row.get("output_tokens"),
            cost_micros: row.get("cost_micros"),
            started_at: row.get("started_at"),
            stopped_at: row.get("stopped_at"),
        })
        .collect())
}

pub(crate) async fn load_agent_work_items(pool: &SqlitePool) -> CommandResult<Vec<AgentWorkItem>> {
    let rows = sqlx::query(
        r#"
        select
            w.id,
            w.agent_id,
            a.handle as agent_handle,
            w.channel_id,
            c.name as channel_name,
            w.thread_root_id,
            w.source_message_id,
            w.inbox_item_id,
            w.task_id,
            t.number as task_number,
            w.source_kind,
            w.title,
            w.context,
            w.status,
            w.run_id,
            w.created_at,
            w.updated_at,
            w.completed_at
        from agent_work_items w
        join agents a on a.id = w.agent_id
        left join channels c on c.id = w.channel_id
        left join tasks t on t.id = w.task_id
        order by w.created_at desc
        limit 80
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    Ok(rows
        .into_iter()
        .map(|row| AgentWorkItem {
            id: row.get("id"),
            agent_id: row.get("agent_id"),
            agent_handle: row.get("agent_handle"),
            channel_id: row.get("channel_id"),
            channel_name: row.get("channel_name"),
            thread_root_id: row.get("thread_root_id"),
            source_message_id: row.get("source_message_id"),
            inbox_item_id: row.get("inbox_item_id"),
            task_id: row.get("task_id"),
            task_number: row.get("task_number"),
            source_kind: row.get("source_kind"),
            title: row.get("title"),
            context: row.get("context"),
            status: row.get("status"),
            run_id: row.get("run_id"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
            completed_at: row.get("completed_at"),
        })
        .collect())
}

pub(crate) async fn load_agent_activities(pool: &SqlitePool) -> CommandResult<Vec<AgentActivity>> {
    let rows = sqlx::query(
        r#"
        select
            id,
            agent_id,
            agent_handle,
            run_id,
            kind,
            phase,
            status,
            title,
            summary,
            detail,
            metadata as metadata,
            created_at
        from (
            select
                agent_activities.*,
                row_number() over (
                    partition by coalesce(case when agent_id is null then null else lower(hex(agent_id)) end, nullif(agent_handle, ''), 'unknown')
                    order by julianday(created_at) desc, created_at desc
                ) as activity_rank
            from agent_activities
        ) ranked
        where activity_rank <= 80
        order by julianday(created_at) desc, created_at desc
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    Ok(rows
        .into_iter()
        .map(|row| AgentActivity {
            id: row.get("id"),
            agent_id: row.get("agent_id"),
            agent_handle: row.get("agent_handle"),
            run_id: row.get("run_id"),
            kind: row.get("kind"),
            phase: row.get("phase"),
            status: row.get("status"),
            title: row.get("title"),
            summary: row.get("summary"),
            detail: row.get("detail"),
            metadata: parse_json_value(row.get("metadata")),
            created_at: row.get("created_at"),
        })
        .collect())
}

pub(crate) async fn load_agent_activity(
    pool: &SqlitePool,
    activity_id: Uuid,
) -> CommandResult<AgentActivity> {
    let row = sqlx::query(
        r#"
        select
            id,
            agent_id,
            agent_handle,
            run_id,
            kind,
            phase,
            status,
            title,
            summary,
            detail,
            metadata as metadata,
            created_at
        from agent_activities
        where id = $1
        "#,
    )
    .bind(activity_id)
    .fetch_one(pool)
    .await
    .map_err(to_string)?;

    Ok(AgentActivity {
        id: row.get("id"),
        agent_id: row.get("agent_id"),
        agent_handle: row.get("agent_handle"),
        run_id: row.get("run_id"),
        kind: row.get("kind"),
        phase: row.get("phase"),
        status: row.get("status"),
        title: row.get("title"),
        summary: row.get("summary"),
        detail: row.get("detail"),
        metadata: parse_json_value(row.get("metadata")),
        created_at: row.get("created_at"),
    })
}

fn parse_json_value(raw: String) -> Value {
    serde_json::from_str(&raw).unwrap_or_else(|_| json!({}))
}

#[cfg(test)]
mod tests {
    use sqlx::SqlitePool;
    use std::fs as std_fs;
    use uuid::Uuid;

    use crate::{db_connect_with_url, migrate};

    use super::load_agent_activities;

    async fn test_pool() -> Option<(SqlitePool, String)> {
        let database_path =
            std::env::temp_dir().join(format!("lantor-test-{}.sqlite", Uuid::new_v4().simple()));
        let database_path = database_path.to_string_lossy().into_owned();
        let database_url = format!("sqlite://{database_path}");
        let pool = match db_connect_with_url(&database_url, 1).await {
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

    #[tokio::test]
    async fn load_agent_activities_compares_mixed_timezone_timestamps_by_instant() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "activity-clock").await?;
            for index in 0..80 {
                let created_at =
                    format!("2026-05-19T{:02}:{:02}:00+08:00", 15 + index / 60, index % 60);
                sqlx::query(
                    r#"
                    insert into agent_activities (
                        agent_id,
                        agent_handle,
                        kind,
                        phase,
                        status,
                        title,
                        summary,
                        detail,
                        created_at
                    )
                    values ($1, 'activity-clock', 'thinking', 'thinking', 'active', $2, $2, '', $3)
                    "#,
                )
                .bind(agent_id)
                .bind(format!("older-local-{index:02}"))
                .bind(created_at)
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            }
            sqlx::query(
                r#"
                insert into agent_activities (
                    agent_id,
                    agent_handle,
                    kind,
                    phase,
                    status,
                    title,
                    summary,
                    detail,
                    created_at
                )
                values ($1, 'activity-clock', 'thinking', 'thinking', 'active', 'newer-utc', 'newer-utc', '', '2026-05-19T09:14:24+00:00')
                "#,
            )
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let activities = load_agent_activities(&pool).await?;
            let agent_activities = activities
                .into_iter()
                .filter(|activity| activity.agent_id == Some(agent_id))
                .collect::<Vec<_>>();
            assert_eq!(agent_activities.len(), 80);
            assert_eq!(agent_activities[0].title, "newer-utc");
            assert!(agent_activities
                .iter()
                .any(|activity| activity.title == "newer-utc"));
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }
}
