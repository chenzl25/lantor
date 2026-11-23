import { useEffect, useState } from "react";
import { apiInvoke } from "../apiClient";
import {
  RUNTIME_PRESETS,
  type Agent,
  type AgentSpend,
  type AgentSubscriptionStatus,
  type AgentSubscriptionWindow,
  type SpendTotals,
} from "../types";
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
  if (status === "limited") return "Limit reached";
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
  if (resetsAt === null || !Number.isFinite(resetsAt)) return "Reset time unknown";
  const date = new Date(resetsAt * 1000);
  if (Number.isNaN(date.getTime())) return "Reset time unknown";
  const remainingMs = date.getTime() - Date.now();
  if (remainingMs <= 0) return "Resetting soon";
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

export function formatSpend(micros: number) {
  if (micros > 0 && micros < 10_000) return "<$0.01";
  const dollars = micros / 1_000_000;
  return `$${dollars.toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;
}

/** What the account has instead of usage windows: credits, or billing per use. */
export function creditsSummary(subscription: Pick<AgentSubscriptionStatus, "status" | "credits">) {
  const credits = subscription.credits;
  const note = subscription.status === "limited" ? "Spend limit reached" : "No usage windows";
  if (credits?.unlimited) return { label: "Credits", value: "Unlimited", note };
  if (credits?.balance != null) {
    const balance = Number(credits.balance);
    const value = Number.isFinite(balance)
      ? balance.toLocaleString("en-US", { maximumFractionDigits: 2 })
      : credits.balance;
    return { label: "Credits", value: `${value} left`, note };
  }
  return { label: "Billing", value: "Pay as you go", note };
}

/** The agent's spend, read when the card shows and again after each new usage snapshot. */
function useAgentSpend(agentId: string | undefined, observedAt: string) {
  const [spend, setSpend] = useState<AgentSpend | null>(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    if (!agentId) return;
    let current = true;
    apiInvoke("load_agent_spend", { agentId }).then(
      (next) => {
        if (!current) return;
        setSpend(next);
        setFailed(false);
      },
      () => {
        if (current) setFailed(true);
      },
    );
    return () => { current = false; };
  }, [agentId, observedAt]);
  return { spend, failed };
}

function SpendRow({ label, totals }: { label: string; totals: SpendTotals }) {
  return (
    <div className="subscription-spend-row">
      <span>{label}</span>
      <dl>
        <div><dt>24h</dt><dd>{formatSpend(totals.last_day_micros)}</dd></div>
        <div><dt>7d</dt><dd>{formatSpend(totals.last_week_micros)}</dd></div>
        <div><dt>30d</dt><dd>{formatSpend(totals.last_month_micros)}</dd></div>
      </dl>
    </div>
  );
}

/**
 * For accounts without usage windows (API keys, enterprise and credit plans):
 * the credits state and what was spent lately, since there is no "percent
 * left" to show.
 */
function WindowlessUsage({ agentId, subscription }: { agentId: string | undefined; subscription: AgentSubscriptionStatus }) {
  const { spend, failed } = useAgentSpend(agentId, subscription.observed_at);
  const credits = creditsSummary(subscription);
  const account = spend?.account && spend.account.agent_count > 1 ? spend.account : null;
  return (
    <div className="subscription-grid windowless">
      <div className={`subscription-card tone-${subscription.status === "limited" ? "danger" : "available"}`}>
        <div className="subscription-card-head">
          <span>{credits.label}</span>
          <strong>{credits.value}</strong>
        </div>
        <div className="subscription-card-meta">
          <span>{credits.note}</span>
        </div>
      </div>
      <div className="subscription-card subscription-spend">
        <div className="subscription-card-head">
          <span>Est. spend</span>
        </div>
        {spend ? (
          <>
            <SpendRow label="This agent" totals={spend.agent} />
            {account && <SpendRow label={`All ${account.agent_count} agents on this account`} totals={account.totals} />}
          </>
        ) : (
          <div className="subscription-card-meta">
            <span>{failed ? "Couldn’t load spend" : "Loading…"}</span>
          </div>
        )}
        <div className="subscription-card-meta">
          <span title="Lantor prices each run’s tokens at the model’s list price. It doesn’t know discounts or contract rates.">
            List-price estimate, not the bill
          </span>
        </div>
      </div>
    </div>
  );
}

export function AgentSubscriptionUsage({ agent, compact = false }: {
  agent: Pick<Agent, "runtime"> & Partial<Pick<Agent, "id" | "subscription_status">>;
  compact?: boolean;
}) {
  const subscription = agent.subscription_status;
  // Filter at display time so persisted snapshots and live updates behave alike.
  const windows = subscription?.windows.filter((window) =>
    subscription.provider !== "codex" || !window.id.startsWith("codex_bengalfox:")
  ) ?? [];
  return (
    <section className={compact ? "detail-section agent-avatar-subscription" : "detail-section subscription-section"}>
      <div className="detail-section-head">
        <h4>Plan usage</h4>
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
          {windows.length > 0 ? (
            <div className="subscription-grid">
              {windows.map((window) => {
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
            <WindowlessUsage agentId={agent.id} subscription={subscription} />
          )}
        </>
      ) : (
        <div className="subscription-empty">
          <strong>No usage yet</strong>
          <span>Shows after the agent’s next reply.</span>
        </div>
      )}
    </section>
  );
}
