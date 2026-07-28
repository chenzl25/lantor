import { useEffect, useState } from "react";
import { AlertTriangle } from "lucide-react";
import type { SupervisorStatus } from "../types";

// The backend marks the supervisor "stale" once its heartbeat (`updated_at`) is
// older than this, and "offline" when there is no supervisor row at all. We
// mirror the same threshold on the client so the banner can appear even when the
// backend has stopped pushing fresh bootstrap snapshots (e.g. the supervisor
// process died mid-window and nothing is emitting `app_state_changed`).
const STALE_AFTER_MS = 10_000;
const TICK_MS = 5_000;

export type SupervisorHealth = {
  /** Scheduling is paused: the supervisor is offline or its heartbeat is stale. */
  stale: boolean;
  /** Normalized status: "offline" | "stale" | the raw backend status. */
  status: string;
  /** Seconds since the last heartbeat, or null when never seen / offline. */
  secondsSinceHeartbeat: number | null;
};

/**
 * Derive supervisor health from the bootstrap `supervisor` field, re-evaluating
 * freshness on a timer so a heartbeat that goes stale surfaces without waiting
 * for the next backend state push.
 */
export function useSupervisorHealth(
  supervisor: SupervisorStatus | null | undefined,
): SupervisorHealth {
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), TICK_MS);
    return () => window.clearInterval(timer);
  }, []);

  if (!supervisor) {
    return { stale: false, status: "unknown", secondsSinceHeartbeat: null };
  }

  if (supervisor.status === "offline" || !supervisor.updated_at) {
    return { stale: true, status: "offline", secondsSinceHeartbeat: null };
  }

  const heartbeatMs = new Date(supervisor.updated_at).getTime();
  const ageMs = Number.isFinite(heartbeatMs) ? Math.max(0, now - heartbeatMs) : null;
  // Trust the backend's own verdict, but also re-derive client-side.
  const stale = supervisor.status === "stale" || (ageMs !== null && ageMs > STALE_AFTER_MS);

  return {
    stale,
    status: stale ? "stale" : supervisor.status,
    secondsSinceHeartbeat: ageMs === null ? null : Math.round(ageMs / 1000),
  };
}

/** Short root-cause hint shown on queued work while the supervisor is down. */
export function supervisorQueuedHint(health: SupervisorHealth): string | null {
  if (!health.stale) return null;
  return health.status === "offline"
    ? "调度进程未在运行：排队的 agent 不会被调度，直到 supervisor 恢复。"
    : "supervisor 心跳停滞：排队的 agent 暂停调度，直到 supervisor 恢复。";
}

export function SupervisorBanner({ health }: { health: SupervisorHealth }) {
  if (!health.stale) return null;

  const offline = health.status === "offline";
  const detail = offline
    ? "调度进程未在运行，排队中的 agent 不会被调度。"
    : health.secondsSinceHeartbeat !== null
      ? `已 ${health.secondsSinceHeartbeat}s 未收到心跳，排队中的 agent 暂停调度。`
      : "心跳停滞，排队中的 agent 暂停调度。";

  return (
    <div className="supervisor-banner" role="alert">
      <AlertTriangle size={16} className="supervisor-banner-icon" aria-hidden="true" />
      <div className="supervisor-banner-copy">
        <strong>backend supervisor unavailable — agent 调度暂停</strong>
        <span>{detail}</span>
      </div>
    </div>
  );
}
