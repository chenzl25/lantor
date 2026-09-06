use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::ui_notifications::{enqueue_ui_event_in_tx, UiEvent};
use crate::{
    agent_environment::normalize_agent_environment_variables,
    agent_workspace::load_agent_workspace_summary,
    app::{to_string, CommandResult},
    attachments::remove_attachment_files,
    db::expand_home_path,
    events::activity::record_agent_activity,
    models::{Agent, AgentSubscriptionStatus, AgentSubscriptionWindow, OwnerProfile},
    prompts::ensure_agent_workspace,
};

pub(crate) const DEFAULT_OWNER_DISPLAY_NAME: &str = "Me";
const DEFAULT_OWNER_AVATAR: &str = "dicebear:dylan:owner";
const DEFAULT_OWNER_DESCRIPTION: &str = "local owner";

pub(crate) async fn update_owner_profile_in_pool(
    pool: &SqlitePool,
    display_name: String,
    avatar: String,
    description: String,
) -> CommandResult<()> {
    let display_name = display_name.trim();
    if display_name.is_empty() {
        return Err("display name is empty".to_owned());
    }

    let mut transaction = pool.begin().await.map_err(to_string)?;
    sqlx::query(
        r#"
        insert into owner_profile (id, display_name, avatar, description, updated_at)
        values (1, $1, $2, $3, strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
        on conflict (id) do update set
            display_name = excluded.display_name,
            avatar = excluded.avatar,
            description = excluded.description,
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        "#,
    )
    .bind(display_name)
    .bind(avatar.trim())
    .bind(description.trim())
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;
    enqueue_ui_event_in_tx(
        &mut transaction,
        &UiEvent::Refresh {
            reason: "owner_profile_updated",
        },
    )
    .await?;
    transaction.commit().await.map_err(to_string)?;
    Ok(())
}

fn normalize_reasoning_effort(
    runtime: &str,
    model: &str,
    value: Option<&str>,
) -> CommandResult<String> {
    if runtime.eq_ignore_ascii_case("codex") {
        let effort = value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("medium")
            .to_ascii_lowercase();
        let model = model.trim().to_ascii_lowercase();
        let supports_ultra = matches!(model.as_str(), "gpt-5.6" | "gpt-5.6-sol" | "gpt-5.6-terra");
        let supports_max =
            supports_ultra || matches!(model.as_str(), "gpt-6-astra" | "gpt-5.6-luna");
        return match effort.as_str() {
            "low" | "medium" | "high" | "xhigh" => Ok(effort),
            "max" if supports_max => Ok(effort),
            "ultra" if supports_ultra => Ok(effort),
            "max" | "ultra" => Err(format!(
                "Codex model {model} does not support reasoning effort {effort}"
            )),
            _ => Err(format!("invalid Codex reasoning effort: {effort}")),
        };
    }
    if runtime.eq_ignore_ascii_case("claude") {
        let effort = value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or_default()
            .to_ascii_lowercase();
        return match effort.as_str() {
            "" => Ok(String::new()),
            "low" | "medium" | "high" | "xhigh" | "max" => Ok(effort),
            _ => Err(format!("invalid Claude effort: {effort}")),
        };
    }
    Ok(String::new())
}

fn normalize_service_tier(runtime: &str, value: Option<&str>) -> CommandResult<String> {
    if !runtime.eq_ignore_ascii_case("codex") {
        return Ok(String::new());
    }
    let tier = value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match tier.as_str() {
        "" | "default" => Ok(String::new()),
        "standard" => Ok(String::new()),
        "fast" => Ok(tier),
        _ => Err(format!("invalid Codex speed tier: {tier}")),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn create_agent_in_pool(
    pool: &SqlitePool,
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
    environment_variables: Option<String>,
    working_directory: String,
    daily_budget_micros: Option<i64>,
) -> CommandResult<Uuid> {
    let normalized_handle = handle.trim().trim_start_matches('@');
    if normalized_handle.is_empty() {
        return Err("agent handle is empty".to_owned());
    }
    let display_name = if display_name.trim().is_empty() {
        normalized_handle
    } else {
        display_name.trim()
    };
    let role = role
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("agent");
    let avatar = avatar
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            normalized_handle
                .chars()
                .next()
                .map(|c| c.to_uppercase().to_string())
                .unwrap_or_else(|| "A".to_owned())
        });
    let description = description
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Local agent");
    let daily_budget_micros = daily_budget_micros.unwrap_or_default().max(0);
    let working_directory = expand_home_path(&working_directory);
    ensure_agent_workspace(&working_directory, normalized_handle)?;
    let runtime = runtime.trim();
    let model = model.trim();
    let reasoning_effort = normalize_reasoning_effort(runtime, model, reasoning_effort.as_deref())?;
    let service_tier = normalize_service_tier(runtime, service_tier.as_deref())?;
    let environment_variables =
        normalize_agent_environment_variables(environment_variables.as_deref())?;
    let existing_subscription_scope =
        sqlx::query("select runtime, environment_variables from agents where handle = $1")
            .bind(normalized_handle)
            .fetch_optional(pool)
            .await
            .map_err(to_string)?;
    let subscription_scope_changed = existing_subscription_scope.is_some_and(|row| {
        row.get::<String, _>("runtime") != runtime
            || row.get::<String, _>("environment_variables") != environment_variables
    });

    let mut transaction = pool.begin().await.map_err(to_string)?;
    let agent_id: Uuid = sqlx::query_scalar(
        r#"
        insert into agents (
            handle, display_name, role, status, runtime, model, avatar, description,
            launch_command, environment_variables, working_directory, daily_budget_micros,
            reasoning_effort, service_tier
        )
        values ($1, $2, $3, 'idle', $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
        on conflict (handle) do update set
            display_name = excluded.display_name,
            role = excluded.role,
            runtime = excluded.runtime,
            model = excluded.model,
            avatar = excluded.avatar,
            description = excluded.description,
            launch_command = excluded.launch_command,
            environment_variables = excluded.environment_variables,
            working_directory = excluded.working_directory,
            daily_budget_micros = excluded.daily_budget_micros,
            reasoning_effort = excluded.reasoning_effort,
            service_tier = excluded.service_tier
        returning id
        "#,
    )
    .bind(normalized_handle)
    .bind(display_name)
    .bind(role)
    .bind(runtime)
    .bind(model)
    .bind(avatar)
    .bind(description)
    .bind(launch_command.trim())
    .bind(&environment_variables)
    .bind(&working_directory)
    .bind(daily_budget_micros)
    .bind(&reasoning_effort)
    .bind(&service_tier)
    .fetch_one(&mut *transaction)
    .await
    .map_err(to_string)?;
    if subscription_scope_changed {
        sqlx::query("delete from agent_subscription_status where agent_id = $1")
            .bind(agent_id)
            .execute(&mut *transaction)
            .await
            .map_err(to_string)?;
    }
    enqueue_ui_event_in_tx(
        &mut transaction,
        &UiEvent::Refresh {
            reason: "agent_created",
        },
    )
    .await?;
    transaction.commit().await.map_err(to_string)?;
    record_agent_activity(
        pool,
        Some(agent_id),
        None,
        "profile",
        "Agent profile saved",
        format!(
            "runtime={} model={} reasoning_effort={} service_tier={}",
            runtime, model, reasoning_effort, service_tier
        ),
    )
    .await?;
    Ok(agent_id)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn update_agent_in_pool(
    pool: &SqlitePool,
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
    environment_variables: Option<String>,
    working_directory: String,
    daily_budget_micros: Option<i64>,
) -> CommandResult<()> {
    let normalized_handle = handle.trim().trim_start_matches('@');
    if normalized_handle.is_empty() {
        return Err("agent handle is empty".to_owned());
    }
    let display_name = if display_name.trim().is_empty() {
        normalized_handle
    } else {
        display_name.trim()
    };
    let role = role
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("agent");
    let avatar = avatar
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            normalized_handle
                .chars()
                .next()
                .map(|c| c.to_uppercase().to_string())
                .unwrap_or_else(|| "A".to_owned())
        });
    let daily_budget_micros = daily_budget_micros.unwrap_or_default().max(0);
    let working_directory = expand_home_path(&working_directory);
    ensure_agent_workspace(&working_directory, normalized_handle)?;
    let runtime = runtime.trim();
    let model = model.trim();
    let reasoning_effort = normalize_reasoning_effort(runtime, model, reasoning_effort.as_deref())?;
    let service_tier = normalize_service_tier(runtime, service_tier.as_deref())?;
    let environment_variables =
        normalize_agent_environment_variables(environment_variables.as_deref())?;
    let existing_subscription_scope =
        sqlx::query("select runtime, environment_variables from agents where id = $1")
            .bind(agent_id)
            .fetch_optional(pool)
            .await
            .map_err(to_string)?;
    let subscription_scope_changed = existing_subscription_scope.is_some_and(|row| {
        row.get::<String, _>("runtime") != runtime
            || row.get::<String, _>("environment_variables") != environment_variables
    });

    let mut transaction = pool.begin().await.map_err(to_string)?;
    sqlx::query(
        r#"
        update agents
        set handle = $2,
            display_name = $3,
            role = $4,
            runtime = $5,
            model = $6,
            avatar = $7,
            description = $8,
            launch_command = $9,
            environment_variables = $10,
            working_directory = $11,
            daily_budget_micros = $12,
            reasoning_effort = $13,
            service_tier = $14
        where id = $1
        "#,
    )
    .bind(agent_id)
    .bind(normalized_handle)
    .bind(display_name)
    .bind(role)
    .bind(runtime)
    .bind(model)
    .bind(avatar)
    .bind(description.trim())
    .bind(launch_command.trim())
    .bind(&environment_variables)
    .bind(&working_directory)
    .bind(daily_budget_micros)
    .bind(&reasoning_effort)
    .bind(&service_tier)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;
    if subscription_scope_changed {
        sqlx::query("delete from agent_subscription_status where agent_id = $1")
            .bind(agent_id)
            .execute(&mut *transaction)
            .await
            .map_err(to_string)?;
    }
    enqueue_ui_event_in_tx(
        &mut transaction,
        &UiEvent::Refresh {
            reason: "agent_updated",
        },
    )
    .await?;
    transaction.commit().await.map_err(to_string)?;
    record_agent_activity(
        pool,
        Some(agent_id),
        None,
        "profile",
        "Agent profile updated",
        format!(
            "runtime={} model={} reasoning_effort={} service_tier={}",
            runtime, model, reasoning_effort, service_tier
        ),
    )
    .await?;
    Ok(())
}

pub(crate) async fn delete_agent_in_pool(pool: &SqlitePool, agent_id: Uuid) -> CommandResult<()> {
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
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;

    if active_run.is_some() {
        return Err("stop the agent before deleting it".to_owned());
    }

    let agent_handle: Option<String> =
        sqlx::query_scalar("select handle from agents where id = $1")
            .bind(agent_id)
            .fetch_optional(pool)
            .await
            .map_err(to_string)?;
    let Some(agent_handle) = agent_handle else {
        return Err("agent does not exist".to_owned());
    };

    record_agent_activity(
        pool,
        Some(agent_id),
        None,
        "profile",
        "Agent profile deleted",
        format!("@{agent_handle}"),
    )
    .await?;

    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(to_string)?;
    let attachment_paths = sqlx::query_scalar::<_, String>(
        r#"
        select ma.storage_path
        from message_attachments ma
        join messages m on m.id = ma.message_id
        join channels c on c.id = m.channel_id
        where c.kind = 'dm' and c.dm_agent_id = $1
        "#,
    )
    .bind(agent_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(to_string)?;
    let result = sqlx::query("delete from agents where id = $1")
        .bind(agent_id)
        .execute(&mut *tx)
        .await
        .map_err(to_string)?;
    if result.rows_affected() == 0 {
        return Err("agent does not exist".to_owned());
    }
    enqueue_ui_event_in_tx(
        &mut tx,
        &UiEvent::Refresh {
            reason: "agent_deleted",
        },
    )
    .await?;
    tx.commit().await.map_err(to_string)?;
    remove_attachment_files(&attachment_paths);

    Ok(())
}

pub(crate) async fn load_owner_profile(pool: &SqlitePool) -> CommandResult<OwnerProfile> {
    let row = sqlx::query(
        r#"
        select display_name, avatar, description
        from owner_profile
        where id = 1
        "#,
    )
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;

    Ok(row
        .map(|row| OwnerProfile {
            display_name: row.get("display_name"),
            avatar: row.get("avatar"),
            description: row.get("description"),
        })
        .unwrap_or_else(|| OwnerProfile {
            display_name: DEFAULT_OWNER_DISPLAY_NAME.to_owned(),
            avatar: DEFAULT_OWNER_AVATAR.to_owned(),
            description: DEFAULT_OWNER_DESCRIPTION.to_owned(),
        }))
}

pub(crate) async fn load_agents(pool: &SqlitePool) -> CommandResult<Vec<Agent>> {
    let rows = sqlx::query(
        r#"
        select
            a.id,
            handle,
            display_name,
            role,
            a.status,
            runtime,
            model,
            reasoning_effort,
            service_tier,
            avatar,
            description,
            launch_command,
            environment_variables,
            working_directory,
            daily_budget_micros,
            subscription.provider as subscription_provider,
            subscription.plan as subscription_plan,
            subscription.status as subscription_status,
            subscription.windows_json as subscription_windows_json,
            subscription.observed_at as subscription_observed_at
        from agents a
        left join agent_subscription_status subscription on subscription.agent_id = a.id
        order by case when handle = 'Hancock' then 0 else 1 end, display_name
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(to_string)?;

    Ok(rows
        .into_iter()
        .map(|row| {
            let working_directory: String = row.get("working_directory");
            let workspace = load_agent_workspace_summary(&working_directory);
            let subscription_provider = row.get::<Option<String>, _>("subscription_provider");
            let subscription_status = subscription_provider.and_then(|provider| {
                let runtime = row.get::<String, _>("runtime");
                if !provider.eq_ignore_ascii_case(&runtime) {
                    return None;
                }
                let windows_json = row
                    .get::<Option<String>, _>("subscription_windows_json")
                    .unwrap_or_else(|| "[]".to_owned());
                let windows = serde_json::from_str::<Vec<AgentSubscriptionWindow>>(&windows_json)
                    .unwrap_or_default();
                Some(AgentSubscriptionStatus {
                    provider,
                    plan: row.get("subscription_plan"),
                    status: row
                        .get::<Option<String>, _>("subscription_status")
                        .unwrap_or_else(|| "unknown".to_owned()),
                    windows,
                    observed_at: row
                        .get::<Option<String>, _>("subscription_observed_at")
                        .unwrap_or_default(),
                })
            });
            Agent {
                id: row.get("id"),
                handle: row.get("handle"),
                display_name: row.get("display_name"),
                role: row.get("role"),
                status: row.get("status"),
                runtime: row.get("runtime"),
                model: row.get("model"),
                reasoning_effort: row.get("reasoning_effort"),
                service_tier: row.get("service_tier"),
                avatar: row.get("avatar"),
                description: row.get("description"),
                launch_command: row.get("launch_command"),
                environment_variables: row.get("environment_variables"),
                working_directory,
                workspace_exists: workspace.exists,
                workspace_memory_path: workspace.memory_path,
                workspace_memory_exists: workspace.memory_exists,
                workspace_entries: workspace.entries,
                daily_budget_micros: row.get("daily_budget_micros"),
                subscription_status,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{delete_agent_in_pool, normalize_reasoning_effort};
    use crate::attachments::write_attachment_file;
    use crate::db::{db_connect_with_url, migrate};
    use sqlx::{Row, SqlitePool};
    use std::fs;
    use uuid::Uuid;

    #[test]
    fn codex_reasoning_effort_is_limited_by_model_capability() {
        for (model, effort) in [
            ("gpt-6-astra", "max"),
            ("gpt-5.6", "ultra"),
            ("gpt-5.6-sol", "max"),
            ("gpt-5.6-sol", "ultra"),
            ("gpt-5.6-terra", "ultra"),
            ("gpt-5.6-luna", "max"),
            ("gpt-5.5", "xhigh"),
        ] {
            assert_eq!(
                normalize_reasoning_effort("codex", model, Some(effort)),
                Ok(effort.to_owned()),
                "{model} should support {effort}"
            );
        }

        for (model, effort) in [
            ("gpt-6-astra", "ultra"),
            ("gpt-5.6-luna", "ultra"),
            ("gpt-5.5", "max"),
            ("gpt-5.5", "ultra"),
            ("gpt-5.6-sol-custom", "ultra"),
        ] {
            assert!(
                normalize_reasoning_effort("codex", model, Some(effort)).is_err(),
                "{model} should reject {effort}"
            );
        }

        assert_eq!(
            normalize_reasoning_effort("codex", " GPT-5.6-SOL ", Some(" ULTRA ")),
            Ok("ultra".to_owned())
        );
        assert_eq!(
            normalize_reasoning_effort("codex", "gpt-5.6-sol", None),
            Ok("medium".to_owned())
        );
        assert!(normalize_reasoning_effort("codex", "gpt-5.6-sol", Some("impossible")).is_err());
    }

    async fn test_pool() -> Option<(SqlitePool, String)> {
        let database_path = std::env::temp_dir().join(format!(
            "lantor-profile-test-{}.sqlite",
            Uuid::new_v4().simple()
        ));
        let database_path = database_path.to_string_lossy().into_owned();
        let database_url = format!("sqlite://{database_path}");
        let pool = match db_connect_with_url(&database_url, 1).await {
            Ok(pool) => pool,
            Err(err) => {
                eprintln!("skipping SQLite-backed Lantor profile test: {err}");
                return None;
            }
        };
        if let Err(err) = migrate(&pool).await {
            eprintln!("skipping SQLite-backed Lantor profile test: {err}");
            drop_test_schema(pool, database_path).await;
            return None;
        }
        Some((pool, database_path))
    }

    async fn drop_test_schema(pool: SqlitePool, database_path: String) {
        pool.close().await;
        let _ = fs::remove_file(&database_path);
        let _ = fs::remove_file(format!("{database_path}-wal"));
        let _ = fs::remove_file(format!("{database_path}-shm"));
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

    async fn insert_channel(pool: &SqlitePool, name: &str) -> Result<Uuid, String> {
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

    async fn insert_dm_channel(pool: &SqlitePool, agent_id: Uuid) -> Result<Uuid, String> {
        sqlx::query_scalar(
            r#"
            insert into channels (name, description, kind, dm_agent_id)
            values ($1, 'test dm', 'dm', $2)
            returning id
            "#,
        )
        .bind(format!("dm:{agent_id}"))
        .bind(agent_id)
        .fetch_one(pool)
        .await
        .map_err(|err| err.to_string())
    }

    async fn insert_agent_message(
        pool: &SqlitePool,
        agent_id: Uuid,
        channel_id: Uuid,
        body: &str,
    ) -> Result<Uuid, String> {
        sqlx::query_scalar(
            r#"
            insert into messages (channel_id, sender_agent_id, sender_name, sender_role, body, is_task)
            values ($1, $2, 'delete-me', 'agent', $3, false)
            returning id
            "#,
        )
        .bind(channel_id)
        .bind(agent_id)
        .bind(body)
        .fetch_one(pool)
        .await
        .map_err(|err| err.to_string())
    }

    #[tokio::test]
    async fn delete_agent_cascades_dm_and_preserves_sender_text() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "delete-me").await?;
            let channel_id = insert_channel(&pool, "delete-agent").await?;
            let dm_channel_id = insert_dm_channel(&pool, agent_id).await?;
            let dm_message_id =
                insert_agent_message(&pool, agent_id, dm_channel_id, "before delete").await?;
            let attachment_id = Uuid::new_v4();
            let attachment_path =
                write_attachment_file(dm_message_id, attachment_id, "dm.txt", b"dm")?;
            sqlx::query(
                r#"
                insert into message_attachments (
                    id, message_id, original_name, mime_type, size_bytes, storage_path
                )
                values ($1, $2, 'dm.txt', 'text/plain', 2, $3)
                "#,
            )
            .bind(attachment_id)
            .bind(dm_message_id)
            .bind(&attachment_path)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let channel_message_id =
                insert_agent_message(&pool, agent_id, channel_id, "channel message").await?;
            delete_agent_in_pool(&pool, agent_id).await?;

            let agent_count: i64 =
                sqlx::query_scalar("select count(*) from agents where id = $1")
                    .bind(agent_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(agent_count, 0);

            let dm_count: i64 =
                sqlx::query_scalar("select count(*) from channels where id = $1")
                    .bind(dm_channel_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(dm_count, 0);

            let deleted_activity: i64 = sqlx::query_scalar(
                "select count(*) from agent_activities where agent_handle = 'delete-me' and title = 'Agent profile deleted'",
            )
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(deleted_activity, 1);

            let message = sqlx::query(
                "select sender_agent_id, sender_name, body from messages where id = $1",
            )
            .bind(channel_message_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(message.get::<Option<Uuid>, _>("sender_agent_id"), None);
            assert_eq!(message.get::<String, _>("sender_name"), "delete-me");
            assert_eq!(message.get::<String, _>("body"), "channel message");
            assert!(!std::path::Path::new(&attachment_path).exists());
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }
}
