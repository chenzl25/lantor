import { useState } from "react";
import { useCliVersion, type CliVersionStore } from "../cli-versions";
import type { Agent, CliVersionStatus } from "../types";

const CLI_LABELS: Record<string, string> = { claude: "Claude Code", codex: "Codex" };
/** A hover card reuses a status read this recently. */
const HOVER_MAX_AGE_MS = 60_000;

export function cliLabel(runtime: string) {
  return CLI_LABELS[runtime] ?? runtime;
}

/** Whether a later release exists for the CLI on this Mac. */
export function cliCanUpdate(status: CliVersionStatus | null): status is CliVersionStatus & { installed: string; latest: string } {
  return Boolean(status?.update_available && status.installed && status.latest);
}

/**
 * The owner message that asks an agent to update its CLI. The agent runs the
 * update from its shell, so it can deal with an error instead of Lantor
 * showing one.
 */
export function cliUpdateRequest(status: CliVersionStatus & { installed: string; latest: string }) {
  const label = cliLabel(status.runtime);
  const command = status.command;
  return [
    `${label} ${status.latest} is out; this Mac has ${status.installed}.`,
    "",
    `Please update ${label} to the latest release from your shell. \`${command} update\` usually works, since it knows how ${label} was installed here. If it fails, find out why and fix it.`,
    "",
    `Every ${label} agent on this Mac uses this install. Your own process keeps its current version until it next starts. When you're done, run \`${command} --version\` and tell me the version and anything you changed.`,
  ].join("\n");
}

/** Sends the update request to the agent, then makes the next view read the version again. */
function useAskToUpdate(agentId: string | undefined, status: CliVersionStatus | null, store: CliVersionStore | null) {
  const [asking, setAsking] = useState(false);
  const ask = store?.askAgent;
  const run = agentId && store && ask && cliCanUpdate(status)
    ? async () => {
      setAsking(true);
      try {
        await ask(agentId, cliUpdateRequest(status));
      } finally {
        store.invalidate(status.runtime);
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
  const status = cli?.entry.status ?? null;
  const askToUpdate = useAskToUpdate(agent.id, status, cli?.store ?? null);
  if (!cli) return null;
  const loadFailed = cli.entry.loadFailed;
  return <div className="cli-version" aria-label={`${label} version`} role="group">
    <div className="cli-version-row">
      <span className="cli-version-name">{label}</span>
      <code>{status?.installed ?? (status || loadFailed ? "—" : "…")}</code>
      {askToUpdate.run && status?.latest &&
        <button type="button" className="cli-version-action" disabled={askToUpdate.asking} onClick={() => void askToUpdate.run?.()}>
          {askToUpdate.asking ? "Sending…" : `Update to ${status.latest}`}
        </button>}
      {status && !status.update_available && status.installed && status.latest &&
        <span className="cli-version-state">Latest</span>}
    </div>
    {loadFailed && <p className="agent-tools-error" role="alert">Couldn’t check the {label} version.</p>}
    {status?.problem && <p className="agent-tools-note">{status.problem}</p>}
    {askToUpdate.run &&
      <p className="agent-tools-note">Update asks @{agent.handle} to install it from its shell. Every {label} agent on this Mac uses this install and switches when it next starts.</p>}
  </div>;
}

/**
 * The avatar hover card's line about the agent's CLI. It shows only when a
 * newer release exists; otherwise it renders nothing.
 */
export function AgentCliUpdateHint({ agent }: {
  agent: Pick<Agent, "handle"> & Partial<Pick<Agent, "id" | "runtime">>;
}) {
  const runtime = agent.runtime ?? "";
  const label = cliLabel(runtime);
  const cli = useCliVersion(runtime, HOVER_MAX_AGE_MS);
  const status = cli?.entry.status ?? null;
  const askToUpdate = useAskToUpdate(agent.id, status, cli?.store ?? null);
  if (!askToUpdate.run || !cliCanUpdate(status)) return null;
  return <div className="cli-update-hint" role="group" aria-label={`${label} update`}>
    <span className="cli-update-hint-copy">
      <span>{label} {status.latest} is available</span>
      <span className="cli-update-hint-detail">This Mac has {status.installed}. Update asks @{agent.handle} to install it.</span>
    </span>
    <button type="button" className="cli-update-hint-action" disabled={askToUpdate.asking} onClick={() => void askToUpdate.run?.()}>
      {askToUpdate.asking ? "Sending…" : "Update"}
    </button>
  </div>;
}
