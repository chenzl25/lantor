import { useEffect, useRef, useState } from "react";
import { apiInvoke } from "../apiClient";
import type { Agent, CliVersionStatus } from "../types";
import { formatRelativeTime } from "../ui-utils";

const CLI_LABELS: Record<string, string> = { claude: "Claude Code", codex: "Codex" };
const POLL_MS = 2000;
const OUTPUT_PREVIEW_LINES = 12;

export function cliLabel(runtime: string) {
  return CLI_LABELS[runtime] ?? runtime;
}

/** The owner message that hands a failed update to the agent. */
export function cliFixRequest(status: CliVersionStatus) {
  const label = cliLabel(status.runtime);
  const outcome = status.last_update;
  const command = outcome?.command ?? `${status.command} update`;
  const output = outcome?.output.trim() || "(no output)";
  const fence = output.includes("```") ? "````" : "```";
  return [
    `\`${command}\` failed on this Mac. ${label} is ${status.installed ?? "not found"}; the latest release is ${status.latest ?? "unknown"}.`,
    "",
    `Please find out why, fix it, and update ${label} to the latest release. Every ${label} agent here uses this install. Tell me what you changed.`,
    "",
    "Output:",
    `${fence}text`,
    output,
    fence,
  ].join("\n");
}

function lastLines(text: string, count: number) {
  const lines = text.trimEnd().split("\n");
  return lines.length > count ? ["…", ...lines.slice(-count)].join("\n") : lines.join("\n");
}

export function AgentCliVersion({ agent, onAskAgentToFix }: {
  agent: Agent;
  onAskAgentToFix: (agent: Agent, body: string) => Promise<void>;
}) {
  const runtime = agent.runtime;
  const label = cliLabel(runtime);
  const [status, setStatus] = useState<CliVersionStatus | null>(null);
  const [loadFailed, setLoadFailed] = useState(false);
  const [starting, setStarting] = useState(false);
  const [asking, setAsking] = useState(false);
  const generation = useRef(0);

  async function load() {
    const request = ++generation.current;
    try {
      const next = await apiInvoke("load_cli_version", { runtime });
      if (generation.current === request) { setStatus(next); setLoadFailed(false); }
    } catch {
      if (generation.current === request) setLoadFailed(true);
    }
  }

  useEffect(() => {
    setStatus(null); setLoadFailed(false); setStarting(false); setAsking(false);
    void load();
    return () => { generation.current++; };
  }, [runtime]);

  useEffect(() => {
    if (!status?.updating) return;
    const timer = window.setTimeout(() => void load(), POLL_MS);
    return () => window.clearTimeout(timer);
  }, [status]);

  async function update() {
    const request = ++generation.current;
    setStarting(true);
    try {
      const next = await apiInvoke("update_cli", { runtime });
      if (generation.current === request) { setStatus(next); setLoadFailed(false); }
    } catch {
      if (generation.current === request) setLoadFailed(true);
    } finally {
      if (generation.current === request) setStarting(false);
    }
  }

  async function askToFix(current: CliVersionStatus) {
    setAsking(true);
    try {
      await onAskAgentToFix(agent, cliFixRequest(current));
    } finally {
      setAsking(false);
    }
  }

  const outcome = status?.last_update ?? null;
  const updating = Boolean(status?.updating) || starting;
  const showFailure = Boolean(status && outcome && !outcome.ok && !updating
    && (status.update_available || !status.installed));
  const showSuccess = Boolean(outcome?.ok && !updating);
  return <div className="cli-version" aria-label={`${label} version`} role="group">
    <div className="cli-version-row">
      <span className="cli-version-name">{label}</span>
      <code>{status?.installed ?? (status || loadFailed ? "—" : "…")}</code>
      {status && !updating && !showFailure && status.update_available && status.latest &&
        <button type="button" className="cli-version-action" onClick={() => void update()}>Update to {status.latest}</button>}
      {updating && <span className="cli-version-state" role="status">Updating…</span>}
      {status && !updating && !status.update_available && status.installed && status.latest &&
        <span className="cli-version-state">Latest</span>}
    </div>
    {loadFailed && <p className="agent-tools-error" role="alert">Couldn’t check the {label} version.</p>}
    {status?.problem && !showFailure && !updating && <p className="agent-tools-note">{status.problem}</p>}
    {status && status.update_available && !updating && !showFailure &&
      <p className="agent-tools-note">Updates the one {label} install every {label} agent on this Mac uses. Each agent switches when it next starts.</p>}
    {updating && <p className="agent-tools-note">Running <code>{status?.command ?? runtime} update</code>. This can take a minute.</p>}
    {showSuccess && outcome &&
      <p className="agent-tools-note" title={outcome.finished_at}>
        Updated {outcome.from ? `from ${outcome.from} ` : ""}to {outcome.to} {formatRelativeTime(outcome.finished_at)}. Each agent switches when it next starts.
      </p>}
    {showFailure && status && outcome && <div className="cli-version-failure" role="alert">
      <p><code>{outcome.command}</code> failed {formatRelativeTime(outcome.finished_at)}.</p>
      <pre className="cli-version-output">{lastLines(outcome.output || "(no output)", OUTPUT_PREVIEW_LINES)}</pre>
      <div className="cli-version-failure-actions">
        <button type="button" className="cli-version-action" disabled={asking} onClick={() => void askToFix(status)}>
          {asking ? "Sending…" : `Ask @${agent.handle} to fix it`}
        </button>
        <button type="button" className="cli-version-link" onClick={() => void update()}>Try again</button>
      </div>
    </div>}
  </div>;
}
