use serde_json::{json, Value};
use sqlx::{sqlite::SqliteRow, Row, Sqlite, SqlitePool, Transaction};
use uuid::Uuid;

use crate::app::{to_string, CommandResult};
use crate::models::{AgentActivity, AgentRun, AgentWorkItem};

const DEFAULT_AGENT_ACTIVITY_LIMIT_PER_AGENT: i64 = 80;
const WEB_AGENT_ACTIVITY_LIMIT_PER_AGENT: i64 = 20;

pub(crate) async fn load_agent_runs(pool: &SqlitePool) -> CommandResult<Vec<AgentRun>> {
    load_agent_runs_with_log_mode(pool, true).await
}

pub(crate) async fn load_agent_run_summaries(pool: &SqlitePool) -> CommandResult<Vec<AgentRun>> {
    load_agent_runs_with_log_mode(pool, false).await
}

async fn load_agent_runs_with_log_mode(
    pool: &SqlitePool,
    include_log: bool,
) -> CommandResult<Vec<AgentRun>> {
    let log_select = if include_log { "r.log" } else { "'' as log" };
    let sql = format!(
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
            {log_select},
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
    );
    let rows = sqlx::query(&sql).fetch_all(pool).await.map_err(to_string)?;

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
    load_agent_work_items_with_context(pool, true, None).await
}

async fn load_agent_work_items_with_context(
    pool: &SqlitePool,
    include_context: bool,
    agent_id: Option<Uuid>,
) -> CommandResult<Vec<AgentWorkItem>> {
    let rows = sqlx::query(
        r#"
        with recent as (
            select id from agent_work_items
            where ($2 is null or agent_id = $2)
            order by created_at desc limit 80
        ), failures as (
            select id, row_number() over (
                partition by agent_id, channel_id, thread_root_id
                order by created_at desc, id desc
            ) as position
            from agent_work_items
            where status = 'failed' and retry_work_item_id is null
              and ($2 is null or agent_id = $2)
        ), retained as (
            select id from recent
            union select id from failures where position = 1
            union select id from agent_work_items
                where status in ('queued', 'running', 'cancelling')
                  and ($2 is null or agent_id = $2)
        )
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
            case when $1 then w.context else '' end as context,
            w.status,
            w.run_id,
            w.retry_work_item_id,
            case when w.status = 'failed' then coalesce((
                select detail from agent_activities
                where run_id = w.run_id and kind = 'run_error'
                order by created_at desc limit 1
            ), '') else '' end as failure_detail,
            w.created_at,
            w.updated_at,
            w.completed_at
        from retained
        join agent_work_items w on w.id = retained.id
        join agents a on a.id = w.agent_id
        left join channels c on c.id = w.channel_id
        left join tasks t on t.id = w.task_id
        order by w.created_at desc
        "#,
    )
    .bind(include_context)
    .bind(agent_id)
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
            retry_work_item_id: row.get("retry_work_item_id"),
            failure_detail: row.get("failure_detail"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
            completed_at: row.get("completed_at"),
        })
        .collect())
}

pub(crate) async fn load_agent_work_item_summaries(
    pool: &SqlitePool,
) -> CommandResult<Vec<AgentWorkItem>> {
    load_agent_work_items_with_context(pool, false, None).await
}

pub(crate) async fn load_bootstrap_activities(
    pool: &SqlitePool,
) -> CommandResult<Vec<AgentActivity>> {
    let mut activities = load_agent_activities_with_limit(pool, 3, None).await?;
    for activity in &mut activities {
        activity.detail = activity.detail.chars().take(240).collect();
        if let Some(metadata) = activity.metadata.as_object_mut() {
            metadata.retain(|_, value| {
                value.is_number()
                    || value.is_boolean()
                    || value.as_str().is_some_and(|text| text.len() <= 256)
            });
        }
    }
    Ok(activities)
}

pub(crate) async fn load_agent_activities(pool: &SqlitePool) -> CommandResult<Vec<AgentActivity>> {
    load_agent_activities_with_limit(pool, DEFAULT_AGENT_ACTIVITY_LIMIT_PER_AGENT, None).await
}

pub(crate) async fn load_agent_activity_summaries(
    pool: &SqlitePool,
) -> CommandResult<Vec<AgentActivity>> {
    load_agent_activities_with_limit(pool, WEB_AGENT_ACTIVITY_LIMIT_PER_AGENT, None).await
}

async fn load_agent_activities_with_limit(
    pool: &SqlitePool,
    limit_per_agent: i64,
    agent_id: Option<Uuid>,
) -> CommandResult<Vec<AgentActivity>> {
    // A loose index scan visits one owner key at a time (including orphaned
    // handles), instead of DISTINCT scanning every historical activity. Each
    // recursive step strictly increases owner_key and stops at the null tail.
    let rows = sqlx::query(
        r#"
        with recursive owners(owner_key) as (
            select case when $2 is not null then lower(hex($2)) else (
                select coalesce(case when agent_id is null then null else lower(hex(agent_id)) end,
                    nullif(agent_handle, ''), 'unknown')
                from agent_activities
                order by coalesce(case when agent_id is null then null else lower(hex(agent_id)) end,
                    nullif(agent_handle, ''), 'unknown') limit 1
            ) end
            union all
            select (
                select coalesce(case when agent_id is null then null else lower(hex(agent_id)) end,
                    nullif(agent_handle, ''), 'unknown')
                from agent_activities
                where coalesce(case when agent_id is null then null else lower(hex(agent_id)) end,
                    nullif(agent_handle, ''), 'unknown') > owners.owner_key
                order by coalesce(case when agent_id is null then null else lower(hex(agent_id)) end,
                    nullif(agent_handle, ''), 'unknown') limit 1
            ) from owners where $2 is null and owner_key is not null
        )
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
        from owners
        join agent_activities activity on activity.id in (
            select recent.id
            from agent_activities recent
            where coalesce(
                case when recent.agent_id is null then null else lower(hex(recent.agent_id)) end,
                nullif(recent.agent_handle, ''),
                'unknown'
            ) = owners.owner_key
            order by julianday(recent.created_at) desc, recent.created_at desc
            limit $1
        )
        order by julianday(activity.created_at) desc, activity.created_at desc
        "#,
    )
    .bind(limit_per_agent)
    .bind(agent_id)
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

pub(crate) async fn load_agent_activity_in_tx(
    transaction: &mut Transaction<'_, Sqlite>,
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
    .fetch_one(&mut **transaction)
    .await
    .map_err(to_string)?;

    Ok(agent_activity_from_row(&row))
}

fn agent_activity_from_row(row: &SqliteRow) -> AgentActivity {
    AgentActivity {
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
    }
}

fn parse_json_value(raw: String) -> Value {
    serde_json::from_str(&raw).unwrap_or_else(|_| json!({}))
}

pub(crate) async fn load_agent_detail_activities(
    pool: &SqlitePool,
    agent_id: Uuid,
) -> CommandResult<Vec<AgentActivity>> {
    load_agent_activities_with_limit(pool, DEFAULT_AGENT_ACTIVITY_LIMIT_PER_AGENT, Some(agent_id))
        .await
}
pub(crate) async fn load_agent_detail_work_items(
    pool: &SqlitePool,
    agent_id: Uuid,
) -> CommandResult<Vec<AgentWorkItem>> {
    load_agent_work_items_with_context(pool, true, Some(agent_id)).await
}

#[cfg(test)]
mod tests {
    use sqlx::SqlitePool;
    use std::fs as std_fs;
    use uuid::Uuid;

    use crate::db::{db_connect_with_url, migrate};

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
    async fn activity_owner_seeks_keep_deleted_anonymous_and_requested_owners() {
        let (pool, schema) = test_pool().await.expect("isolated database");
        assert!(super::load_agent_activities_with_limit(&pool, 1, None)
            .await
            .unwrap()
            .is_empty());
        let agent = insert_test_agent(&pool, "owner-seek").await.unwrap();
        for (agent_id, handle) in [(Some(agent), "owner-seek"), (None, "deleted"), (None, "")] {
            for index in 0..3 {
                sqlx::query("insert into agent_activities(agent_id,agent_handle,kind,title,created_at) values($1,$2,'thinking',$3,$4)")
                    .bind(agent_id).bind(handle).bind(format!("{handle}:{index}"))
                    .bind(format!("2026-09-08T00:00:0{index}+00:00")).execute(&pool).await.unwrap();
            }
        }
        let all = super::load_agent_activities_with_limit(&pool, 1, None)
            .await
            .unwrap();
        assert_eq!(all.len(), 3);
        assert!(all.iter().all(|row| row.title.ends_with(":2")));
        let only = super::load_agent_activities_with_limit(&pool, 1, Some(agent))
            .await
            .unwrap();
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].agent_id, Some(agent));
        assert!(
            super::load_agent_activities_with_limit(&pool, 1, Some(Uuid::new_v4()))
                .await
                .unwrap()
                .is_empty()
        );
        pool.close().await;
        drop_sqlite_test_files(&schema);
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
