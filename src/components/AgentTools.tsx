import { ChevronDown, ChevronRight, RefreshCw } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { apiInvoke } from "../apiClient";
import type { Agent, AgentTools as Tools, AgentToolSummary, AgentToolInventory } from "../types";
import { formatRelativeTime } from "../ui-utils";

const failedStatuses = new Set(["failed", "error", "disconnected", "authenticationRequired"]);
const needsAttention = (server: Tools["mcp"]["items"][number]) =>
  Boolean(server.tools_error) || server.auth_status === "notLoggedIn" || failedStatuses.has(server.status);

function stateLabel(state: string) {
  return ({ ready: "", unavailable: "Could not read status", unconfirmed: "Not confirmed yet",
    not_run: "No workspace run snapshot yet", unsupported: "Unavailable for this runtime" })[state] ?? "Unknown";
}

function statusLabel(status: string) {
  return ({ connected: "Connected", pending: "Pending", starting: "Starting", connecting: "Connecting",
    failed: "Failed", error: "Error", disconnected: "Disconnected", authenticationRequired: "Login required",
    notStarted: "Not started", disabled: "Disabled", cancelled: "Cancelled" })[status] ?? "Unknown";
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
        {statusLabel(server.status)} · {server.tools.length} tools
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

export function AgentTools({ agent, onRestart }: { agent: Agent; onRestart: (agent: Agent) => void }) {
  const [expanded, setExpanded] = useState(false);
  const [data, setData] = useState<Tools | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState(false);
  const [pluginsOpen, setPluginsOpen] = useState(false);
  const [skillsOpen, setSkillsOpen] = useState(false);
  const generation = useRef(0);
  useEffect(() => {
    generation.current++;
    setExpanded(false); setData(null); setLoading(false); setError(false);
    setPluginsOpen(false); setSkillsOpen(false);
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
  const issues = data ? [["MCP", data.mcp], ["Plugins", data.plugins], ["Skills", data.skills]] as const : [];
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
          <p className="agent-tools-note">{servers.length} MCP servers · {userPlugins.length} plugins{builtinPlugins.length ? ` + ${builtinPlugins.length} built-in` : ""} · {data.skills.items.length} skills</p>}
        {issues.filter(([, section]) => section.state !== "ready").map(([label, section]) =>
          <p className={`agent-tools-${section.state === "unavailable" ? "error" : "note"}`} key={label}>
            {label}: {section.message || stateLabel(section.state)}
          </p>)}
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
            {userPlugins.length > 0 && <h5>Installed</h5>}
            <ul>{userPlugins.map((p, i) => <li key={`${p.name}-${i}`}><strong>{p.name}</strong>{!p.enabled && <small> · Disabled</small>}{p.description && <p>{p.description}</p>}</li>)}</ul>
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
        <button type="button" className="agent-tools-refresh" onClick={() => onRestart(agent)}>Restart</button>
      </div>
    </div>}
  </section>;
}
