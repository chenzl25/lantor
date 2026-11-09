import { useState } from "react";
import { useCliVersion, type CliVersionEntry } from "../cli-versions";
import type { Agent, CliVersionStatus } from "../types";
import { formatRelativeTime } from "../ui-utils";

const CLI_LABELS: Record<string, string> = { claude: "Claude Code", codex: "Codex" };
const OUTPUT_PREVIEW_LINES = 12;
/** A hover card reuses a status read this recently. */
const HOVER_MAX_AGE_MS = 60_000;
/** A hover card mentions a finished update this long. */
const HOVER_RECENT_UPDATE_MS = 10 * 60_000;

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

/** What the version row shows for a status: one state at a time. */
export function cliVersionView(entry: CliVersionEntry) {
  const { status } = entry;
  const outcome = status?.last_update ?? null;
  const updating = Boolean(status?.updating) || entry.starting;
  // A failure stays shown while the CLI is still behind or missing.
  const failed = Boolean(status && outcome && !outcome.ok && !updating
    && (status.update_available || !status.installed));
  return {
    outcome,
    updating,
    failed,
    succeeded: Boolean(outcome?.ok && !updating),
    canUpdate: Boolean(status && !updating && !failed && status.update_available && status.latest),
  };
}

function useAskToFix(agentId: string | undefined, status: CliVersionStatus | null, ask: ((agentId: string, body: string) => Promise<void>) | null) {
  const [asking, setAsking] = useState(false);
  const run = agentId && status && ask
    ? async () => {
      setAsking(true);
      try {
        await ask(agentId, cliFixRequest(status));
      } finally {
        setAsking(false);
      }
    }
    : null;
  return { asking, run };
}

export function AgentCliVersion({ agent }: { agent: Agent }) {
  const runtime = agent.runtime;
  const label = cliLabel(runtime);
  const cli = useCliVersion(runtime, 0);
  const entry = cli?.entry;
  const status = entry?.status ?? null;
  const loadFailed = Boolean(entry?.loadFailed);
  const askToFix = useAskToFix(agent.id, status, cli?.store.askAgentToFix ?? null);
  if (!cli || !entry) return null;
  const update = () => void cli.store.update(runtime);
  const { outcome, updating, failed: showFailure, succeeded: showSuccess, canUpdate } = cliVersionView(entry);
  return <div className="cli-version" aria-label={`${label} version`} role="group">
    <div className="cli-version-row">
      <span className="cli-version-name">{label}</span>
      <code>{status?.installed ?? (status || loadFailed ? "—" : "…")}</code>
      {canUpdate && status?.latest &&
        <button type="button" className="cli-version-action" onClick={update}>Update to {status.latest}</button>}
      {updating && <span className="cli-version-state" role="status">Updating…</span>}
      {status && !updating && !status.update_available && status.installed && status.latest &&
        <span className="cli-version-state">Latest</span>}
    </div>
    {loadFailed && <p className="agent-tools-error" role="alert">Couldn’t check the {label} version.</p>}
    {status?.problem && !showFailure && !updating && <p className="agent-tools-note">{status.problem}</p>}
    {canUpdate &&
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
        {askToFix.run && <button type="button" className="cli-version-action" disabled={askToFix.asking} onClick={() => void askToFix.run?.()}>
          {askToFix.asking ? "Sending…" : `Ask @${agent.handle} to fix it`}
        </button>}
        <button type="button" className="cli-version-link" onClick={update}>Try again</button>
      </div>
    </div>}
  </div>;
}

/**
 * The avatar hover card's line about the agent's CLI. It shows only when
 * there is something to do or see: a newer release, an update running, one
 * that just finished, or one that failed. Otherwise it renders nothing.
 */
export function AgentCliUpdateHint({ agent }: {
  agent: Pick<Agent, "handle"> & Partial<Pick<Agent, "id" | "runtime">>;
}) {
  const runtime = agent.runtime ?? "";
  const label = cliLabel(runtime);
  const cli = useCliVersion(runtime, HOVER_MAX_AGE_MS);
  const status = cli?.entry.status ?? null;
  const askToFix = useAskToFix(agent.id, status, cli?.store.askAgentToFix ?? null);
  if (!cli || !status) return null;
  const update = () => void cli.store.update(runtime);
  const { outcome, updating, failed, succeeded, canUpdate } = cliVersionView(cli.entry);
  const recentSuccess = succeeded && outcome?.to
    && Date.now() - Date.parse(outcome.finished_at) < HOVER_RECENT_UPDATE_MS;
  if (!canUpdate && !updating && !failed && !recentSuccess) return null;
  return <div className="cli-update-hint" role="group" aria-label={`${label} update`}>
    {canUpdate && <>
      <span className="cli-update-hint-copy">
        <span>{label} {status.latest} is available</span>
        <span className="cli-update-hint-detail">This Mac has {status.installed ?? "an older version"}</span>
      </span>
      <button type="button" className="cli-update-hint-action" onClick={update}>Update</button>
    </>}
    {updating && <span className="cli-update-hint-copy" role="status">Updating {label}{status.latest ? ` to ${status.latest}` : ""}…</span>}
    {recentSuccess && outcome && <span className="cli-update-hint-copy" role="status">
      <span>{label} updated to {outcome.to}</span>
      <span className="cli-update-hint-detail">Each agent switches when it next starts</span>
    </span>}
    {failed && outcome && <>
      <span className="cli-update-hint-copy" role="alert">{label} update failed</span>
      <span className="cli-update-hint-actions">
        {askToFix.run && <button type="button" className="cli-update-hint-action" disabled={askToFix.asking} onClick={() => void askToFix.run?.()}>
          {askToFix.asking ? "Sending…" : `Ask @${agent.handle} to fix it`}
        </button>}
        <button type="button" className="cli-update-hint-link" onClick={update}>Try again</button>
      </span>
    </>}
  </div>;
}
