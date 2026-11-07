//! Short-lived CLI discovery. Never starts a thread, sends a prompt or creates a run.
use std::{collections::HashMap, path::PathBuf, process::Stdio, time::Duration};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::{timeout, Instant},
};
use uuid::Uuid;

use crate::app::CommandResult;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct ProbeConfig {
    pub(crate) agent_id: Uuid,
    pub(crate) handle: String,
    pub(crate) runtime: String,
    pub(crate) working_directory: String,
    pub(crate) environment_variables: String,
    pub(crate) model: String,
    pub(crate) reasoning_effort: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ToolSummary {
    pub(crate) name: String,
    pub(crate) description: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct PluginSummary {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) builtin: bool,
    pub(crate) enabled: bool,
    /// Apps this plugin provides, by name.
    pub(crate) apps: Vec<String>,
}

/// An app an agent can reach: a Codex ChatGPT app, a claude.ai connector or
/// another MCP server.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct AppSummary {
    pub(crate) name: String,
    /// connected, login_required, failed, pending, disabled, needs_setup or unknown.
    pub(crate) status: &'static str,
    /// chatgpt_app, claude_connector or mcp_server.
    pub(crate) source: &'static str,
    pub(crate) tool_count: usize,
    pub(crate) plugin: Option<String>,
    pub(crate) detail: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct McpServer {
    pub(crate) name: String,
    pub(crate) status: String,
    pub(crate) auth_status: Option<String>,
    pub(crate) tools_error: Option<String>,
    pub(crate) tools: Vec<ToolSummary>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Inventory<T> {
    pub(crate) state: &'static str,
    pub(crate) message: String,
    pub(crate) observed_at: Option<String>,
    pub(crate) items: Vec<T>,
}

impl<T> Inventory<T> {
    pub(crate) fn empty(state: &'static str, message: &str) -> Self {
        Self {
            state,
            message: message.into(),
            observed_at: None,
            items: Vec::new(),
        }
    }

    pub(crate) fn ready(items: Vec<T>) -> Self {
        Self {
            state: "ready",
            message: String::new(),
            observed_at: None,
            items,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AgentTools {
    pub(crate) agent_id: Uuid,
    pub(crate) runtime: String,
    pub(crate) checked_at: String,
    pub(crate) apps: Inventory<AppSummary>,
    pub(crate) mcp: Inventory<McpServer>,
    pub(crate) plugins: Inventory<PluginSummary>,
    pub(crate) skills: Inventory<ToolSummary>,
}

impl AgentTools {
    pub(crate) fn unavailable(config: &ProbeConfig) -> Self {
        Self {
            agent_id: config.agent_id,
            runtime: config.runtime.clone(),
            checked_at: chrono::Utc::now().to_rfc3339(),
            apps: Inventory::empty("unavailable", "Could not read status"),
            mcp: Inventory::empty("unavailable", "Could not read status"),
            plugins: Inventory::empty("unavailable", "Could not read status"),
            skills: Inventory::empty("unavailable", "Could not read status"),
        }
    }
}

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const PROBE_TIMEOUT: Duration = Duration::from_secs(95);

pub(crate) async fn probe(config: &ProbeConfig, result: &mut AgentTools) {
    let command = match config.runtime.as_str() {
        "claude" => {
            let mut command = Command::new("claude");
            if !config.model.is_empty() {
                command.args(["--model", &config.model]);
            }
            if !config.reasoning_effort.is_empty() {
                command.args(["--effort", &config.reasoning_effort]);
            }
            if super::claude::configure_claude_command(
                &mut command,
                config.agent_id,
                &config.handle,
                &config.working_directory,
                &config.environment_variables,
            )
            .is_err()
            {
                return;
            }
            command.arg("--no-session-persistence");
            Ok(command)
        }
        "codex" => super::codex::app_server_command(
            config.agent_id,
            &config.handle,
            &config.working_directory,
            &config.environment_variables,
        ),
        _ => return,
    };
    if let Ok(command) = command {
        probe_command(config, result, command, REQUEST_TIMEOUT, PROBE_TIMEOUT).await;
    }
}

/// A process group guard also covers a dropped HTTP future or a cancelled timeout.
struct ProbeProcess {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    stderr: tokio::task::JoinHandle<()>,
    pid: Option<u32>,
    next_id: u64,
    request_timeout: Duration,
}

impl ProbeProcess {
    fn start(mut command: Command, request_timeout: Duration) -> CommandResult<Self> {
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| "CLI unavailable")?;
        let stdin = child.stdin.take().ok_or("CLI stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("CLI stdout unavailable")?;
        let mut stderr = child.stderr.take().ok_or("CLI stderr unavailable")?;
        let stderr = tokio::spawn(async move {
            let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
        });
        Ok(Self {
            pid: child.id(),
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            stderr,
            next_id: 0,
            request_timeout,
        })
    }

    async fn write(&mut self, value: Value) -> CommandResult<()> {
        let line = format!("{value}\n");
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|_| "CLI closed".to_owned())?;
        self.stdin
            .flush()
            .await
            .map_err(|_| "CLI closed".to_owned())
    }

    async fn request(&mut self, method: &str, params: Value, claude: bool) -> CommandResult<Value> {
        self.next_id += 1;
        let id = self.next_id.to_string();
        let message = if claude {
            json!({"type":"control_request", "request_id":id, "request":params})
        } else {
            json!({"id":id,"method":method,"params":params})
        };
        let deadline = self.request_timeout;
        timeout(deadline, async {
            self.write(message).await?;
            while let Some(line) = self
                .lines
                .next_line()
                .await
                .map_err(|_| "CLI read failed")?
            {
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let response = if claude { &value["response"] } else { &value };
                let response_id = if claude {
                    &response["request_id"]
                } else {
                    &response["id"]
                };
                if response_id.as_str() != Some(&id) {
                    continue;
                }
                if response.get("error").is_some() || response["subtype"] == "error" {
                    return Err("CLI request failed".to_owned());
                }
                return Ok(if claude {
                    response["response"].clone()
                } else {
                    response["result"].clone()
                });
            }
            Err("CLI closed".to_owned())
        })
        .await
        .map_err(|_| "CLI request timed out".to_owned())?
    }

    /// Sends Codex requests together and waits for every reply within one
    /// request timeout. A failed or missing reply is `None`.
    async fn request_many(&mut self, method: &str, params: Vec<Value>) -> Vec<Option<Value>> {
        let first = self.next_id + 1;
        self.next_id += params.len() as u64;
        let mut results: Vec<Option<Value>> = vec![None; params.len()];
        let mut answered = vec![false; params.len()];
        let deadline = self.request_timeout;
        let _ = timeout(deadline, async {
            for (offset, params) in params.into_iter().enumerate() {
                let id = (first + offset as u64).to_string();
                self.write(json!({"id":id,"method":method,"params":params}))
                    .await?;
            }
            while answered.iter().any(|done| !done) {
                let Some(line) = self
                    .lines
                    .next_line()
                    .await
                    .map_err(|_| "CLI read failed".to_owned())?
                else {
                    break;
                };
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let Some(index) = value["id"]
                    .as_str()
                    .and_then(|id| id.parse::<u64>().ok())
                    .and_then(|id| id.checked_sub(first))
                    .and_then(|index| usize::try_from(index).ok())
                    .filter(|index| *index < answered.len())
                else {
                    continue;
                };
                answered[index] = true;
                if value.get("error").is_none() {
                    results[index] = Some(value["result"].clone());
                }
            }
            Ok::<(), String>(())
        })
        .await;
        results
    }

    fn kill_group(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            // Only the new child's group; never the warm agent's process.
            if pid > 1 {
                let _ = std::process::Command::new("/bin/kill")
                    .args(["-KILL", "--", &format!("-{pid}")])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
        let _ = self.child.start_kill();
    }

    async fn close(mut self) {
        self.kill_group();
        let _ = self.child.wait().await;
        self.pid = None;
        self.stderr.abort();
    }
}

impl Drop for ProbeProcess {
    fn drop(&mut self) {
        self.kill_group();
        self.stderr.abort();
    }
}

async fn probe_command(
    config: &ProbeConfig,
    result: &mut AgentTools,
    command: Command,
    request_timeout: Duration,
    total_timeout: Duration,
) {
    let Ok(mut process) = ProbeProcess::start(command, request_timeout) else {
        return;
    };
    let _ = timeout(total_timeout, async {
        if config.runtime == "claude" {
            probe_claude(&mut process, result, request_timeout).await;
        } else {
            probe_codex(&mut process, config, result).await;
        }
    })
    .await;
    process.close().await;
    if config.runtime == "claude" {
        result.apps = apps_from_mcp(&result.mcp);
    }
}

/// Claude reaches apps only through MCP: `claude.ai <App>` servers are
/// claude.ai connectors, the rest are configured MCP servers.
fn apps_from_mcp(mcp: &Inventory<McpServer>) -> Inventory<AppSummary> {
    if !matches!(mcp.state, "ready" | "unconfirmed") {
        return Inventory::empty(mcp.state, &mcp.message);
    }
    let mut apps = Inventory::ready(
        mcp.items
            .iter()
            .map(|server| {
                let (name, source) = match server.name.strip_prefix("claude.ai ") {
                    Some(name) => (name.to_owned(), "claude_connector"),
                    None => (server.name.clone(), "mcp_server"),
                };
                AppSummary {
                    name,
                    status: match server.status.as_str() {
                        "connected" => "connected",
                        "needs-auth" => "login_required",
                        "failed" => "failed",
                        "pending" | "connecting" => "pending",
                        "disabled" => "disabled",
                        _ => "unknown",
                    },
                    source,
                    tool_count: server.tools.len(),
                    plugin: None,
                    detail: server.tools_error.clone(),
                }
            })
            .collect(),
    );
    apps.state = mcp.state;
    apps.message = mcp.message.clone();
    apps
}

async fn probe_claude(process: &mut ProbeProcess, result: &mut AgentTools, wait: Duration) {
    if process
        .request("", json!({"subtype":"initialize"}), true)
        .await
        .is_err()
    {
        return;
    }
    let deadline = Instant::now() + wait;
    loop {
        let response = process
            .request("", json!({"subtype":"mcp_status"}), true)
            .await;
        let Ok(response) = response else {
            result.mcp = Inventory::empty("unavailable", "Could not read status");
            return;
        };
        let Some(servers) = response["mcpServers"].as_array() else {
            return;
        };
        let items: Vec<McpServer> = servers
            .iter()
            .map(|s| McpServer {
                name: string(s, "name"),
                status: string(s, "status"),
                auth_status: None,
                tools_error: s["error"].as_str().map(str::to_owned),
                tools: s["tools"]
                    .as_array()
                    .map(|tools| tools.iter().map(tool_summary).collect())
                    .unwrap_or_default(),
            })
            .collect();
        let pending = items.is_empty()
            || items
                .iter()
                .any(|s| matches!(s.status.as_str(), "pending" | "connecting"));
        result.mcp = Inventory::ready(items);
        if !pending {
            return;
        }
        if Instant::now() >= deadline {
            result.mcp.state = "unconfirmed";
            result.mcp.message = "Not confirmed yet".into();
            return;
        }
        tokio::time::sleep(Duration::from_secs(1).min(wait / 30)).await;
    }
}

async fn probe_codex(process: &mut ProbeProcess, config: &ProbeConfig, result: &mut AgentTools) {
    if process.request("initialize", json!({"clientInfo":{"name":"lantor_tools","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}), false).await.is_err() { return; }
    if process
        .write(json!({"method":"initialized"}))
        .await
        .is_err()
    {
        return;
    }
    let mut cursor = Value::Null;
    let mut servers = Vec::new();
    // ChatGPT apps share the `codex_apps` server; each tool names its connector.
    let mut connectors: HashMap<String, (String, usize)> = HashMap::new();
    loop {
        let response = process
            .request(
                "mcpServerStatus/list",
                json!({"cursor":cursor,"limit":100,"detail":"toolsAndAuthOnly"}),
                false,
            )
            .await;
        let Ok(response) = response else { break };
        let Some(data) = response["data"].as_array() else {
            break;
        };
        for tools in data
            .iter()
            .filter(|s| s["name"] == "codex_apps")
            .filter_map(|s| s["tools"].as_object())
        {
            for meta in tools.values().map(|tool| &tool["_meta"]) {
                if let Some(id) = meta["connector_id"].as_str() {
                    let name = meta["connector_name"].as_str().unwrap_or(id);
                    connectors
                        .entry(id.to_owned())
                        .or_insert_with(|| (name.to_owned(), 0))
                        .1 += 1;
                }
            }
        }
        servers.extend(data.iter().map(|s| {
            McpServer {
                name: string(s, "name"),
                status: s["runtimeStatus"].as_str().unwrap_or("unknown").into(),
                auth_status: s["authStatus"].as_str().map(str::to_owned),
                tools_error: s["toolsError"].as_str().map(str::to_owned),
                tools: s["tools"]
                    .as_object()
                    .map(|t| t.values().map(tool_summary).collect())
                    .unwrap_or_default(),
            }
        }));
        let next = response["nextCursor"].clone();
        if next.is_null() || next == "" {
            result.mcp = Inventory::ready(servers);
            break;
        }
        if next == cursor {
            break;
        }
        cursor = next;
    }
    let installed_apps = process
        .request("app/installed", json!({"forceRefresh":false}), false)
        .await
        .ok()
        .and_then(|response| response["apps"].as_array().cloned());
    // App id -> (app name, plugin name), from each installed plugin's details.
    let mut plugin_apps: HashMap<String, (String, String)> = HashMap::new();
    // Listing plugins materializes installed plugin skills in this CLI version.
    let mut plugins_listed = false;
    if let Ok(response) = process
        .request(
            "plugin/list",
            json!({"cwds":[config.working_directory],"forceRefetch":false}),
            false,
        )
        .await
    {
        if let Some(markets) = response["marketplaces"].as_array() {
            plugins_listed = true;
            let mut plugins = Vec::new();
            let mut reads = Vec::new();
            for marketplace in markets {
                if let Some(items) = marketplace["plugins"].as_array() {
                    for p in items.iter().filter(|p| p["installed"] == true) {
                        let name = string(p, "name");
                        reads.push(match marketplace["path"].as_str() {
                            Some(path) => json!({"pluginName":name,"marketplacePath":path}),
                            None => json!({"pluginName":name,"remoteMarketplaceName":marketplace["name"]}),
                        });
                        plugins.push(PluginSummary {
                            name,
                            description: p
                                .pointer("/interface/shortDescription")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .into(),
                            builtin: false,
                            enabled: p["enabled"].as_bool().unwrap_or(true),
                            apps: Vec::new(),
                        });
                    }
                }
            }
            // Remote plugin details take over a second each; read them together.
            let details = process.request_many("plugin/read", reads).await;
            for (plugin, detail) in plugins.iter_mut().zip(details) {
                let Some(apps) = detail.as_ref().and_then(|d| d["plugin"]["apps"].as_array())
                else {
                    continue;
                };
                for app in apps {
                    let name = string(app, "name");
                    if let Some(id) = app["id"].as_str() {
                        plugin_apps.insert(id.to_owned(), (name.clone(), plugin.name.clone()));
                    }
                    plugin.apps.push(name);
                }
            }
            result.plugins = Inventory::ready(plugins);
            if response["marketplaceLoadErrors"]
                .as_array()
                .is_some_and(|e| !e.is_empty())
            {
                result.plugins.state = "unavailable";
                result.plugins.message = "Some plugin catalogs could not be read".into();
            }
        }
    }
    if let Ok(response) = process
        .request(
            "skills/list",
            json!({"cwds":[config.working_directory],"forceReload":true}),
            false,
        )
        .await
    {
        if let Some(entries) = response["data"].as_array() {
            if let Some(entry) = entries
                .iter()
                .find(|e| e["cwd"].as_str() == Some(&config.working_directory))
            {
                if let Some(skills) = entry["skills"].as_array() {
                    result.skills = Inventory::ready(
                        skills
                            .iter()
                            .filter(|s| s["enabled"] != false)
                            .map(tool_summary)
                            .collect(),
                    );
                    if !plugins_listed {
                        result.skills.state = "unconfirmed";
                        result.skills.message = "Plugin skills may be missing".into();
                    }
                    if entry["errors"].as_array().is_some_and(|e| !e.is_empty()) {
                        result.skills.state = "unavailable";
                        result.skills.message = "Some skills could not be read".into();
                    }
                }
            }
        }
    }
    if let Some(installed) = installed_apps {
        let mut apps: Vec<AppSummary> = installed
            .iter()
            .map(|app| {
                let id = app["id"].as_str().unwrap_or("");
                let from_plugin = plugin_apps.get(id);
                AppSummary {
                    name: app["runtimeName"]
                        .as_str()
                        .map(str::to_owned)
                        .or_else(|| from_plugin.map(|(name, _)| name.clone()))
                        .or_else(|| connectors.get(id).map(|(name, _)| name.clone()))
                        .unwrap_or_else(|| id.to_owned()),
                    status: if app["enabled"] == false {
                        "disabled"
                    } else if app["callable"] == true {
                        "connected"
                    } else {
                        "needs_setup"
                    },
                    source: "chatgpt_app",
                    tool_count: connectors.get(id).map_or(0, |(_, count)| *count),
                    plugin: from_plugin.map(|(_, plugin)| plugin.clone()),
                    detail: None,
                }
            })
            .collect();
        apps.sort_by_key(|app| app.name.to_lowercase());
        result.apps = Inventory::ready(apps);
    }
}

fn string(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or("").to_owned()
}
fn tool_summary(value: &Value) -> ToolSummary {
    ToolSummary {
        name: string(value, "name"),
        description: string(value, "description"),
    }
}

pub(crate) fn workspace_path(cwd: &str) -> PathBuf {
    PathBuf::from(cwd)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(cwd))
}

#[cfg(test)]
#[path = "../tests/runtime_tools.rs"]
mod tests;
