import { RUNTIME_PRESETS, type Agent, type AgentSubscriptionWindow } from "../types";
import { formatRelativeTime } from "../ui-utils";

function titleCaseIdentifier(value: string) {
  return value
    .split(/[_-]+/)
    .filter(Boolean)
    .map((part) => `${part.charAt(0).toUpperCase()}${part.slice(1)}`)
    .join(" ");
}

function subscriptionStatusLabel(status: string) {
  if (status === "available") return "Available";
  if (status === "warning") return "Near limit";
  if (status === "limited") return "Rate limited";
  if (status === "unavailable") return "Unavailable";
  return titleCaseIdentifier(status || "unknown");
}

function subscriptionTone(window: AgentSubscriptionWindow) {
  const remaining = 100 - window.used_percent;
  if (remaining <= 10) return "danger";
  if (remaining <= 25) return "warning";
  return "available";
}

function formatSubscriptionPercent(value: number) {
  const clamped = Math.max(0, Math.min(100, value));
  if (clamped > 0 && clamped < 1) return "<1";
  return String(Math.round(clamped));
}

function formatSubscriptionReset(resetsAt: number | null) {
  if (resetsAt === null || !Number.isFinite(resetsAt)) return "Reset time unavailable";
  const date = new Date(resetsAt * 1000);
  if (Number.isNaN(date.getTime())) return "Reset time unavailable";
  const remainingMs = date.getTime() - Date.now();
  if (remainingMs <= 0) return "Reset pending";
  const totalMinutes = Math.max(1, Math.ceil(remainingMs / 60_000));
  if (totalMinutes < 60) return `Resets in ${totalMinutes}m`;
  const totalHours = Math.floor(totalMinutes / 60);
  const minutes = totalMinutes % 60;
  if (totalHours < 24) return `Resets in ${totalHours}h${minutes > 0 ? ` ${minutes}m` : ""}`;
  const days = Math.floor(totalHours / 24);
  const hours = totalHours % 24;
  return `Resets in ${days}d${hours > 0 ? ` ${hours}h` : ""}`;
}

function subscriptionResetTitle(resetsAt: number | null) {
  if (resetsAt === null || !Number.isFinite(resetsAt)) return undefined;
  const date = new Date(resetsAt * 1000);
  return Number.isNaN(date.getTime()) ? undefined : date.toLocaleString();
}

function subscriptionStatusIsStale(observedAt: string) {
  const observed = new Date(observedAt).getTime();
  return Number.isNaN(observed) || Date.now() - observed > 30 * 60_000;
}

function runtimeLabel(runtime: string) {
  return RUNTIME_PRESETS[runtime]?.label ?? runtime;
}

export function AgentSubscriptionUsage({ agent, compact = false }: {
  agent: Pick<Agent, "runtime"> & Partial<Pick<Agent, "subscription_status">>;
  compact?: boolean;
}) {
  const subscription = agent.subscription_status;
  return (
    <section className={compact ? "detail-section agent-avatar-subscription" : "detail-section subscription-section"}>
      <div className="detail-section-head">
        <h4>Subscription</h4>
        <span>
          {subscription?.plan
            ? `${runtimeLabel(subscription.provider)} · ${titleCaseIdentifier(subscription.plan)}`
            : runtimeLabel(agent.runtime)}
        </span>
      </div>
      {subscription ? (
        <>
          <div className="subscription-summary">
            <span className={`subscription-status status-${subscription.status}`}>
              {subscriptionStatusLabel(subscription.status)}
            </span>
            <small title={subscription.observed_at}>
              Updated {formatRelativeTime(subscription.observed_at)}
              {subscriptionStatusIsStale(subscription.observed_at) ? " · stale" : ""}
            </small>
          </div>
          {subscription.windows.length > 0 ? (
            <div className="subscription-grid">
              {subscription.windows.map((window) => {
                const remaining = Math.max(0, Math.min(100, 100 - window.used_percent));
                const tone = subscriptionTone(window);
                return (
                  <div className={`subscription-card tone-${tone}`} key={window.id}>
                    <div className="subscription-card-head">
                      <span>{window.label}</span>
                      <strong>{formatSubscriptionPercent(remaining)}% left</strong>
                    </div>
                    <div
                      className="subscription-meter"
                      role="progressbar"
                      aria-label={`${window.label} subscription remaining`}
                      aria-valuemin={0}
                      aria-valuemax={100}
                      aria-valuenow={Math.round(remaining)}
                    >
                      <span style={{ width: `${remaining}%` }} />
                    </div>
                    <div className="subscription-card-meta">
                      <span>{formatSubscriptionPercent(window.used_percent)}% used</span>
                      <time title={subscriptionResetTitle(window.resets_at)}>
                        {formatSubscriptionReset(window.resets_at)}
                      </time>
                    </div>
                  </div>
                );
              })}
            </div>
          ) : (
            <div className="subscription-empty">
              <strong>No quota windows reported</strong>
              <span>This account or provider does not expose subscription usage.</span>
            </div>
          )}
        </>
      ) : (
        <div className="subscription-empty">
          <strong>Waiting for usage data</strong>
          <span>Usage appears after this agent's next provider turn.</span>
        </div>
      )}
    </section>
  );
}
