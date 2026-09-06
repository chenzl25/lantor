// Scoped invalidation reads: never load message history, run logs or artifact content.
use crate::app::{to_string, AppState, CommandResult};
use serde::Deserialize;
use serde_json::{Map, Value};
use sqlx::SqlitePool;
use tauri::State;

#[derive(Debug, Deserialize)]
pub(crate) struct UiStateRequest {
    pub(crate) scopes: Vec<String>,
}

pub(crate) async fn load_ui_state_in_pool(
    pool: &SqlitePool,
    scopes: Vec<String>,
) -> CommandResult<Value> {
    let mut patch = Map::new();
    for scope in scopes {
        if patch.contains_key(&scope) {
            continue;
        }
        let value = match scope.as_str() {
            "owner_profile" => {
                serde_json::to_value(crate::agent_profile::load_owner_profile(pool).await?)
            }
            "channels" => serde_json::to_value(crate::channels::load_channels(pool).await?),
            "channel_members" => {
                serde_json::to_value(crate::channels::load_channel_members(pool).await?)
            }
            "thread_activities" => {
                serde_json::to_value(crate::channels::load_thread_activities(pool).await?)
            }
            "agents" => serde_json::to_value(crate::agent_profile::load_agents(pool).await?),
            "saved_messages" => {
                serde_json::to_value(crate::message_store::load_saved_messages(pool).await?)
            }
            "dismissed_inbox_items" => {
                serde_json::to_value(crate::owner_inbox::load_dismissed_inbox_items(pool).await?)
            }
            "read_inbox_items" => {
                serde_json::to_value(crate::owner_inbox::load_read_inbox_items(pool).await?)
            }
            "artifacts" => {
                serde_json::to_value(crate::message_store::load_artifact_summaries(pool).await?)
            }
            "tasks" => serde_json::to_value(crate::task_store::load_tasks(pool).await?),
            "reminders" => {
                serde_json::to_value(crate::domain::reminders::load_reminders(pool).await?)
            }
            "agent_schedules" => {
                serde_json::to_value(crate::domain::schedules::load_agent_schedules(pool).await?)
            }
            "agent_runs" => {
                serde_json::to_value(crate::activity_store::load_agent_run_summaries(pool).await?)
            }
            "agent_work_items" => {
                serde_json::to_value(crate::activity_store::load_agent_work_items(pool).await?)
            }
            "agent_activities" => serde_json::to_value(
                crate::activity_store::load_agent_activity_summaries(pool).await?,
            ),
            "supervisor" => serde_json::to_value(
                crate::runtime::supervisor::load_supervisor_status(pool).await?,
            ),
            "launch_agent" => {
                serde_json::to_value(crate::launch_agent::load_launch_agent_status()?)
            }
            _ => return Err(format!("unsupported UI state scope: {scope}")),
        }
        .map_err(to_string)?;
        patch.insert(scope, value);
    }
    Ok(Value::Object(patch))
}

#[tauri::command]
pub(crate) async fn load_ui_state(
    scopes: Vec<String>,
    state: State<'_, AppState>,
) -> CommandResult<Value> {
    load_ui_state_in_pool(&state.pool, scopes).await
}
