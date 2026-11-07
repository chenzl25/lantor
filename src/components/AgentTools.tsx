import { ChevronDown, ChevronRight, RefreshCw, X } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { apiInvoke } from "../apiClient";
import type { Agent, AgentApp, AgentTools as Tools, AgentToolSummary, AgentToolInventory } from "../types";
import { formatRelativeTime } from "../ui-utils";

const failedStatuses = new Set(["failed", "error", "disconnected", "authenticationRequired", "needs-auth"]);
const needsAttention = (server: Tools["mcp"]["items"][number]) =>
  Boolean(server.tools_error) || server.auth_status === "notLoggedIn" || failedStatuses.has(server.status);
const countLabel = (count: number, noun: string) => `${count} ${noun}${count === 1 ? "" : "s"}`;

export const DEFAULT_WATCHED_APPS = ["Notion", "Slack", "Gmail"];
const MAX_WATCHED_APPS = 30;
const NO_APPS: AgentToolInventory<AgentApp> = { state: "unavailable", message: "", observed_at: null, items: [] };
// "Google Drive", "google-drive" and "claude.ai Google Drive" name the same app.
const appKey = (name: string) => name.replace(/^claude\.ai /i, "").toLowerCase().replace(/[^a-z0-9]/g, "");
const appStatusLabels: Record<string, string> = { connected: "Connected", login_required: "Login required",
  failed: "Failed", pending: "Pending", disabled: "Disabled", needs_setup: "Needs setup", not_connected: "Not connected" };
const appSourceLabels: Record<string, string> = { chatgpt_app: "ChatGPT app", claude_connector: "claude.ai connector",
  mcp_server: "MCP server" };
const appNeedsAttention = (status: string) => ["login_required", "failed", "needs_setup"].includes(status);

// A watched app can also be a configured MCP server that is not an app, such as a Slack server added to Codex.
function findApp(name: string, data: Tools, apps: AgentApp[]): AgentApp | null {
  const key = appKey(name);
  const app = apps.find(item => appKey(item.name) === key);
  if (app) return app;
  const server = data.mcp.items.find(item => appKey(item.name) === key);
  return server ? { name: server.name, status: server.tools_error ? "failed" : "unknown", source: "mcp_server",
    tool_count: server.tools.length, plugin: null, detail: server.tools_error } : null;
}

function AppRow({ name, app, runtime, appsReady }: { name: string; app: AgentApp | null; runtime: string; appsReady: boolean }) {
  const status = app?.status ?? (appsReady ? "not_connected" : "unknown");
  const source = app && [appSourceLabels[app.source] ?? app.source, app.plugin && `${app.plugin} plugin`].filter(Boolean).join(" · ");
  return <li className={`agent-tool-app${appNeedsAttention(status) ? " needs-attention" : ""}${status === "not_connected" ? " missing" : ""}`}>
    <div className="agent-tool-app-row">
      <strong>{app?.name ?? name}</strong>
      <span className={`agent-tools-status${status === "connected" ? " connected" : ""}`}>
        {appStatusLabels[status] ?? "Unknown"}{app && app.tool_count > 0 ? ` · ${countLabel(app.tool_count, "tool")}` : ""}
      </span>
    </div>
    {source && <p>{source}</p>}
    {app?.detail && <p className="agent-tools-error">{app.detail}</p>}
    {status === "not_connected" && <p>{runtime === "claude"
      ? "Add it as a claude.ai connector or an MCP server."
      : "Connect it as a ChatGPT app or add an MCP server."}</p>}
  </li>;
}

function WatchEditor({ initial, onSave, onClose }: {
  initial: string[];
  onSave: (apps: string[]) => Promise<void>;
  onClose: () => void;
}) {
  const [draft, setDraft] = useState(initial);
  const [input, setInput] = useState("");
  const [saving, setSaving] = useState(false);
  const [failed, setFailed] = useState(false);
  const name = input.trim().replace(/\s+/g, " ");
  function add() {
    if (!name) return;
    if (!draft.some(item => appKey(item) === appKey(name))) setDraft([...draft, name]);
    setInput("");
  }
  async function save() {
    setSaving(true); setFailed(false);
    try {
      await onSave(draft);
      onClose();
    } catch {
      setFailed(true);
    } finally {
      setSaving(false);
    }
  }
  return <div className="agent-tools-watch-editor">
    <ul className="agent-tools-chips">{draft.map(item => <li key={item}>
      <span>{item}</span>
      <button type="button" aria-label={`Stop watching ${item}`} onClick={() => setDraft(draft.filter(other => other !== item))}>
        <X size={12} />
      </button>
    </li>)}</ul>
    <form onSubmit={event => { event.preventDefault(); add(); }}>
      <input aria-label="App to watch" placeholder="Add an app, such as Linear" value={input} maxLength={60}
        onChange={event => setInput(event.target.value)} />
      <button type="submit" className="agent-tools-refresh" disabled={!name || draft.length >= MAX_WATCHED_APPS}>Add</button>
    </form>
    <p className="agent-tools-note">Watched apps are listed for every agent, even when they are not connected.</p>
    {failed && <p className="agent-tools-error" role="alert">Could not save. Try again.</p>}
    <div className="agent-tools-editor-actions">
      <button type="button" className="agent-tools-refresh" onClick={onClose}>Cancel</button>
      <button type="button" className="agent-tools-refresh" disabled={saving} onClick={() => void save()}>Save</button>
    </div>
  </div>;
}

function stateLabel(state: string) {
  return ({ ready: "", unavailable: "Could not read status", unconfirmed: "Not confirmed yet",
    not_run: "No workspace run snapshot yet", unsupported: "Unavailable for this runtime" })[state] ?? "Unknown";
}

function statusLabel(status: string) {
  return ({ connected: "Connected", pending: "Pending", starting: "Starting", connecting: "Connecting",
    failed: "Failed", error: "Error", disconnected: "Disconnected", authenticationRequired: "Login required",
    "needs-auth": "Login required", notStarted: "Not started", disabled: "Disabled", cancelled: "Cancelled" })[status] ?? "Unknown";
}

function authLabel(status: string) {
  return ({ oAuth: "OAuth", bearerToken: "Bearer token", notLoggedIn: "Login required",
    unsupported: "Not supported", unknown: "Unknown" })[status] ?? status;
}

function ToolList({ items }: { items: AgentToolSummary[] }) {
  const [query, setQuery] = useState("");
  const filtered = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return items.filter(item => `${item.name} ${item.description}`.toLowerCase().includes(needle));
  }, [items, query]);
  return <div className="agent-tools-list">
    {items.length > 10 && <input type="search" aria-label="Search tools" placeholder="Search tools"
      value={query} onChange={event => setQuery(event.target.value)} />}
    {filtered.length === 0 && <p className="agent-tools-note">{items.length ? "No tools match this search." : "No tools listed."}</p>}
    <ul>{filtered.map((tool, index) => <li key={`${tool.name}-${index}`}>
      <code>{tool.name}</code>{tool.description && <p>{tool.description}</p>}
    </li>)}</ul>
  </div>;
}

function Server({ server, runtime }: { server: Tools["mcp"]["items"][number]; runtime: string }) {
  const [open, setOpen] = useState(false);
  const attention = needsAttention(server);
  return <article className={`agent-tool-server${attention ? " needs-attention" : ""}`}>
    <button type="button" className="agent-tools-disclosure" aria-expanded={open} onClick={() => setOpen(!open)}>
      {open ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
      <strong>{server.name}</strong>
      <span className={`agent-tools-status${runtime === "claude" && server.status === "connected" ? " connected" : ""}`}>
        {statusLabel(server.status)} · {countLabel(server.tools.length, "tool")}
      </span>
    </button>
    {server.auth_status && <p className="agent-tools-note">Authentication: {authLabel(server.auth_status)}</p>}
    {server.tools_error && <p className="agent-tools-error">{server.tools_error}</p>}
    {open && <ToolList items={server.tools} />}
  </article>;
}

function SnapshotNote({ inventory }: { inventory: AgentToolInventory<unknown> }) {
  return <>
    {inventory.state !== "ready" && <p className="agent-tools-note">{inventory.message || stateLabel(inventory.state)}</p>}
    {inventory.observed_at && <p className="agent-tools-note" title={inventory.observed_at}>
      From the last workspace run · {formatRelativeTime(inventory.observed_at)}
    </p>}
  </>;
}

export function AgentTools({ agent, onRestart, restartDisabledReason, watchedApps, onSaveWatchedApps }: {
  agent: Agent;
  onRestart: (agent: Agent) => void;
  restartDisabledReason: string;
  watchedApps: string[];
  onSaveWatchedApps: (apps: string[]) => Promise<void>;
}) {
  const [expanded, setExpanded] = useState(false);
  const [data, setData] = useState<Tools | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState(false);
  const [pluginsOpen, setPluginsOpen] = useState(false);
  const [skillsOpen, setSkillsOpen] = useState(false);
  const [otherAppsOpen, setOtherAppsOpen] = useState(false);
  const [editingWatch, setEditingWatch] = useState(false);
  const generation = useRef(0);
  useEffect(() => {
    generation.current++;
    setExpanded(false); setData(null); setLoading(false); setError(false);
    setPluginsOpen(false); setSkillsOpen(false); setOtherAppsOpen(false); setEditingWatch(false);
    return () => { generation.current++; };
  }, [agent.id, agent.runtime, agent.working_directory, agent.environment_variables]);

  async function load(refresh: boolean) {
    const request = ++generation.current;
    setLoading(true); setError(false);
    try {
      const result = await apiInvoke("load_agent_tools", { agentId: agent.id, refresh });
      if (generation.current === request) setData(result);
    } catch {
      if (generation.current === request) setError(true);
    } finally {
      if (generation.current === request) setLoading(false);
    }
  }
  const servers = [...(data?.mcp.items ?? [])].sort((a, b) => Number(needsAttention(b)) - Number(needsAttention(a)) || a.name.localeCompare(b.name));
  const plugins = [...(data?.plugins.items ?? [])].sort((a, b) => Number(a.builtin) - Number(b.builtin) || a.name.localeCompare(b.name));
  const userPlugins = plugins.filter(p => !p.builtin);
  const builtinPlugins = plugins.filter(p => p.builtin);
  const appPlugins = userPlugins.filter(p => p.apps?.length);
  const otherPlugins = userPlugins.filter(p => !p.apps?.length);
  const appsInventory = data?.apps ?? NO_APPS;
  const appsReady = appsInventory.state === "ready";
  const watched = data ? watchedApps.map(name => ({ name, app: findApp(name, data, appsInventory.items) })) : [];
  const watchedKeys = new Set(watched.flatMap(({ name, app }) => [appKey(name), ...(app ? [appKey(app.name)] : [])]));
  const otherApps = appsInventory.items.filter(app => !watchedKeys.has(appKey(app.name)))
    .sort((a, b) => Number(appNeedsAttention(b.status)) - Number(appNeedsAttention(a.status)) || a.name.localeCompare(b.name));
  const issues = data ? [["Apps", appsInventory], ["MCP", data.mcp], ["Plugins", data.plugins], ["Skills", data.skills]] as const : [];
  return <section className="detail-section agent-tools-section" aria-label="Agent tools">
    <div className="detail-section-head agent-tools-head">
      <button type="button" className="agent-tools-disclosure" aria-expanded={expanded} onClick={() => {
        setExpanded(!expanded);
        if (!expanded && !loading) void load(false);
      }}>{expanded ? <ChevronDown size={16} /> : <ChevronRight size={16} />}<h4>Tools</h4></button>
      {expanded && <button type="button" className="agent-tools-refresh" disabled={loading} onClick={() => void load(true)}>
        <RefreshCw size={13} />{loading ? "Checking…" : "Refresh"}
      </button>}
    </div>
    {expanded && <div className="agent-tools-content" aria-busy={loading}>
      {loading && <p className="agent-tools-note" role="status">Checking tools… This can take about 30 seconds.</p>}
      {error && <p className="agent-tools-error" role="alert">Could not read status. Try Refresh.</p>}
      {data && <>
        <p className="agent-tools-note" title={data.checked_at}>Checked {formatRelativeTime(data.checked_at)}</p>
        {data.mcp.state === "ready" && data.plugins.state === "ready" && data.skills.state === "ready" &&
          <p className="agent-tools-note">{countLabel(servers.length, "MCP server")} · {countLabel(userPlugins.length, "plugin")}{builtinPlugins.length ? ` + ${builtinPlugins.length} built-in` : ""} · {countLabel(data.skills.items.length, "skill")}</p>}
        {issues.filter(([, section]) => section.state !== "ready").map(([label, section]) =>
          <p className={`agent-tools-${section.state === "unavailable" ? "error" : "note"}`} key={label}>
            {label}: {section.message || stateLabel(section.state)}
          </p>)}
        <div className="agent-tools-apps">
          <div className="agent-tools-apps-head">
            <h5>Watched apps</h5>
            {!editingWatch && <button type="button" className="agent-tools-link" onClick={() => setEditingWatch(true)}>Edit</button>}
          </div>
          {editingWatch && <WatchEditor initial={watchedApps} onSave={onSaveWatchedApps} onClose={() => setEditingWatch(false)} />}
          <ul>{watched.map(({ name, app }) => <AppRow key={name} name={name} app={app} runtime={agent.runtime} appsReady={appsReady} />)}</ul>
          {!watched.length && <p className="agent-tools-note">No watched apps. Use Edit to add the ones you need.</p>}
          {otherApps.length > 0 && <div className="agent-tools-other-apps">
            <button type="button" className="agent-tools-disclosure" aria-expanded={otherAppsOpen} onClick={() => setOtherAppsOpen(!otherAppsOpen)}>
              {otherAppsOpen ? <ChevronDown size={14} /> : <ChevronRight size={14} />}<strong>Other apps</strong>
              <span>{otherApps.filter(app => app.status === "connected").length} connected</span>
            </button>
            {otherAppsOpen && <ul>{otherApps.map(app => <AppRow key={app.name} name={app.name} app={app} runtime={agent.runtime} appsReady />)}</ul>}
          </div>}
        </div>
        <div className="agent-tools-mcp">
          <h5>MCP servers{data.mcp.state === "ready" ? ` · ${servers.length}` : ""}</h5>
          {servers.map(server => <Server key={server.name} server={server} runtime={agent.runtime} />)}
          {data.mcp.state === "ready" && !servers.length && <p className="agent-tools-note">No MCP servers listed.</p>}
        </div>
        <div className="agent-tools-group">
          <button type="button" className="agent-tools-disclosure" aria-expanded={pluginsOpen} onClick={() => setPluginsOpen(!pluginsOpen)}>
            {pluginsOpen ? <ChevronDown size={14} /> : <ChevronRight size={14} />}<strong>Plugins</strong>
            <span>{data.plugins.state === "ready" ? `${userPlugins.length} installed${builtinPlugins.length ? ` · ${builtinPlugins.length} built-in` : ""}` : stateLabel(data.plugins.state)}</span>
          </button>
          {pluginsOpen && <>
            <SnapshotNote inventory={data.plugins} />
            {appPlugins.length > 0 && <h5>Provide apps</h5>}
            <ul>{appPlugins.map((p, i) => <li key={`${p.name}-${i}`}><strong>{p.name}</strong> → {p.apps.join(", ")}{!p.enabled && <small> · Disabled</small>}</li>)}</ul>
            {otherPlugins.length > 0 && <h5>{appPlugins.length ? "Other plugins" : "Installed"}</h5>}
            <ul>{otherPlugins.map((p, i) => <li key={`${p.name}-${i}`}><strong>{p.name}</strong>{!p.enabled && <small> · Disabled</small>}{p.description && <p>{p.description}</p>}</li>)}</ul>
            {builtinPlugins.length > 0 && <h5>Built-in</h5>}
            <ul>{builtinPlugins.map((p, i) => <li key={`${p.name}-${i}`}><strong>{p.name}</strong></li>)}</ul>
            {data.plugins.state === "ready" && !plugins.length && <p className="agent-tools-note">No installed plugins.</p>}
          </>}
        </div>
        <div className="agent-tools-group">
          <button type="button" className="agent-tools-disclosure" aria-expanded={skillsOpen} onClick={() => setSkillsOpen(!skillsOpen)}>
            {skillsOpen ? <ChevronDown size={14} /> : <ChevronRight size={14} />}<strong>Skills</strong>
            <span>{data.skills.state === "ready" ? data.skills.items.length : stateLabel(data.skills.state)}</span>
          </button>
          {skillsOpen && <><SnapshotNote inventory={data.skills} /><ToolList items={data.skills.items} /></>}
        </div>
      </>}
      <div className="agent-tools-footer"><p>Changed a login or tools? Restart to apply them to the running agent.</p>
        <button type="button" className="agent-tools-refresh" disabled={Boolean(restartDisabledReason)}
          title={restartDisabledReason || undefined} onClick={() => onRestart(agent)}>Restart</button>
      </div>
    </div>}
  </section>;
}
