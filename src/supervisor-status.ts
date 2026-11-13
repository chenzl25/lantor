import type { SupervisorStatus } from "./types";
import { formatTime } from "./ui-utils";

/** What Settings says about the supervisor: how it runs, then whether a newer build waits. */
export function supervisorStatusLines(status: SupervisorStatus | undefined): string[] {
  if (!status || status.pid === null || status.status === "offline") return ["Not running."];
  if (status.status === "stale") {
    return [status.updated_at ? `Not responding since ${formatTime(status.updated_at)}.` : "Not responding."];
  }
  const lines = [
    status.status === "restarting"
      ? "Restarting with a newer build."
      : status.started_at
        ? `Running since ${formatTime(status.started_at)}.`
        : "Running.",
  ];
  if (status.status !== "restarting" && status.update_waiting_since) {
    lines.push("A newer build is ready. It takes over when no agent is working.");
  }
  if (status.update_error) lines.push(`The newer build could not start: ${status.update_error}`);
  return lines;
}
