use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader},
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use serde::Deserialize;
use serde_json::Value;
use sqlx::{Row, SqlitePool};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    app::{to_string, CommandResult},
    runtime::tools::{self, AgentTools, Inventory, PluginSummary, ProbeConfig, ToolSummary},
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentToolsRequest {
    pub(crate) agent_id: Uuid,
    #[serde(default)]
    pub(crate) refresh: bool,
}

struct Cached {
    config: ProbeConfig,
    result: AgentTools,
    completed: Instant,
}

type CacheEntry = Arc<Mutex<Option<Cached>>>;
#[derive(Default)]
struct ToolsCache(Mutex<HashMap<(PathBuf, Uuid), CacheEntry>>);
const CACHE_TTL: Duration = Duration::from_secs(180);

pub(crate) async fn load(
    pool: &SqlitePool,
    request: AgentToolsRequest,
) -> CommandResult<AgentTools> {
    static CACHE: OnceLock<ToolsCache> = OnceLock::new();
    load_cached(pool, request, CACHE.get_or_init(ToolsCache::default)).await
}

async fn load_cached(
    pool: &SqlitePool,
    request: AgentToolsRequest,
    cache: &ToolsCache,
) -> CommandResult<AgentTools> {
    let requested = Instant::now();
    let row = sqlx::query("select handle, runtime, working_directory, environment_variables, model, reasoning_effort from agents where id = $1")
        .bind(request.agent_id).fetch_one(pool).await.map_err(to_string)?;
    let cwd: String = row.get("working_directory");
    let cwd = if cwd.is_empty() {
        std::env::current_dir().map_err(to_string)?
    } else {
        PathBuf::from(cwd)
    };
    let config = ProbeConfig {
        agent_id: request.agent_id,
        handle: row.get("handle"),
        runtime: row.get("runtime"),
        working_directory: tools::workspace_path(&cwd.to_string_lossy())
            .to_string_lossy()
            .into_owned(),
        environment_variables: row.get("environment_variables"),
        model: row.get("model"),
        reasoning_effort: row.get("reasoning_effort"),
    };
    let key = (
        pool.connect_options().get_filename().to_path_buf(),
        request.agent_id,
    );
    let result = cache
        .obtain(key, config.clone(), requested, request.refresh, || async {
            let mut result = AgentTools::unavailable(&config);
            if config.runtime == "claude" {
                load_claude_snapshot(pool, &config, &mut result).await;
            }
            if matches!(config.runtime.as_str(), "claude" | "codex") {
                tools::probe(&config, &mut result).await;
            } else {
                result.mcp = Inventory::empty(
                    "unsupported",
                    "Tool discovery is not available for this runtime",
                );
                result.plugins = Inventory::empty(
                    "unsupported",
                    "Tool discovery is not available for this runtime",
                );
                result.skills = Inventory::empty(
                    "unsupported",
                    "Tool discovery is not available for this runtime",
                );
            }
            result.checked_at = chrono::Utc::now().to_rfc3339();
            result
        })
        .await;
    Ok(result)
}

impl ToolsCache {
    async fn obtain<F: std::future::Future<Output = AgentTools>>(
        &self,
        key: (PathBuf, Uuid),
        config: ProbeConfig,
        requested: Instant,
        refresh: bool,
        discover: impl FnOnce() -> F,
    ) -> AgentTools {
        let entry = self
            .0
            .lock()
            .await
            .entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(None)))
            .clone();
        let mut slot = entry.lock().await;
        if let Some(cached) = &*slot {
            if cached.config == config
                && cached.completed.elapsed() < CACHE_TTL
                && (!refresh || cached.completed >= requested)
            {
                return cached.result.clone();
            }
        }
        let result = discover().await;
        *slot = Some(Cached {
            config,
            result: result.clone(),
            completed: Instant::now(),
        });
        result
    }
}

async fn load_claude_snapshot(pool: &SqlitePool, config: &ProbeConfig, result: &mut AgentTools) {
    let rows = match sqlx::query("select id, started_at from agent_runs where agent_id = $1 order by started_at desc, rowid desc")
        .bind(config.agent_id).fetch_all(pool).await {
        Ok(rows) => rows,
        Err(_) => return,
    };
    let directory = pool
        .connect_options()
        .get_filename()
        .with_extension("run-logs");
    let paths: Vec<_> = rows
        .into_iter()
        .map(|row| {
            let id: Uuid = row.get("id");
            (
                directory.join(format!("{id}.log")),
                row.get::<String, _>("started_at"),
            )
        })
        .collect();
    let cwd = config.working_directory.clone();
    match tokio::task::spawn_blocking(move || latest_workspace_init(paths, &cwd)).await {
        Ok(Ok(Some((init, observed)))) => {
            if let Some(items) = init["plugins"].as_array() {
                result.plugins = Inventory::ready(
                    items
                        .iter()
                        .filter_map(|p| {
                            let name = p.as_str().or_else(|| p["name"].as_str())?;
                            Some(PluginSummary {
                                name: name.into(),
                                description: String::new(),
                                builtin: p["source"]
                                    .as_str()
                                    .is_some_and(|source| source.ends_with("@builtin"))
                                    || p["path"] == "builtin"
                                    || name.ends_with("@builtin"),
                                enabled: true,
                            })
                        })
                        .collect(),
                );
                result.plugins.observed_at = Some(observed.clone());
            }
            if let Some(items) = init["skills"].as_array() {
                result.skills = Inventory::ready(
                    items
                        .iter()
                        .filter_map(|s| {
                            let name = s.as_str().or_else(|| s["name"].as_str())?;
                            Some(ToolSummary {
                                name: name.into(),
                                description: s["description"].as_str().unwrap_or("").into(),
                            })
                        })
                        .collect(),
                );
                result.skills.observed_at = Some(observed);
            }
        }
        Ok(Ok(None)) => {
            result.plugins = Inventory::empty("not_run", "No workspace run snapshot yet");
            result.skills = Inventory::empty("not_run", "No workspace run snapshot yet");
        }
        _ => {}
    }
}

fn latest_workspace_init(
    paths: Vec<(PathBuf, String)>,
    cwd: &str,
) -> CommandResult<Option<(Value, String)>> {
    let cwd = tools::workspace_path(cwd);
    let mut unreadable = false;
    for (path, observed) in paths {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(_) => {
                unreadable = true;
                continue;
            }
        };
        let mut matching = None;
        for line in BufReader::new(file).lines() {
            let Ok(line) = line else {
                unreadable = true;
                break;
            };
            let Some(line) = line.strip_prefix("[claude] ") else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if value["type"] == "system"
                && value["subtype"] == "init"
                && value["cwd"]
                    .as_str()
                    .is_some_and(|path| tools::workspace_path(path) == cwd)
            {
                matching = Some(value);
            }
        }
        if let Some(init) = matching {
            return Ok(Some((init, observed)));
        }
    }
    if unreadable {
        Err("Run logs unavailable".into())
    } else {
        Ok(None)
    }
}

#[cfg(test)]
#[path = "../tests/agent_tools.rs"]
mod tests;
