import { createContext, useContext, useRef, useState, type ReactNode } from "react";
import { RotateCcw, Square } from "lucide-react";
import { apiInvoke } from "../apiClient";
import type { AgentWorkItem } from "../types";

type Action = "stop" | "retry";
type ActionState = { action: Action; pending: boolean; requested?: boolean; error?: string };
const WorkItemActionsContext = createContext<{
  states: Record<string, ActionState>;
  request: (item: AgentWorkItem, action: Action) => void;
} | null>(null);

export function WorkItemActionsProvider({ children, onChanged }: { children: ReactNode; onChanged: () => void }) {
  const [states, setStates] = useState<Record<string, ActionState>>({});
  const inFlight = useRef(new Set<string>());
  async function request(item: AgentWorkItem, action: Action) {
    if (inFlight.current.has(item.id)) return;
    inFlight.current.add(item.id);
    setStates((current) => ({ ...current, [item.id]: { action, pending: true } }));
    try {
      if (action === "stop") await apiInvoke("cancel_agent_work", { workItemId: item.id });
      else await apiInvoke("retry_agent_work", { workItemId: item.id });
      setStates((current) => ({ ...current, [item.id]: { action, pending: false, requested: true } }));
    } catch (error) {
      setStates((current) => ({ ...current, [item.id]: {
        action, pending: false, error: error instanceof Error ? error.message : String(error),
      } }));
    } finally {
      inFlight.current.delete(item.id);
      onChanged();
    }
  }
  return <WorkItemActionsContext.Provider value={{ states, request }}>{children}</WorkItemActionsContext.Provider>;
}

export function WorkItemActions({ item }: { item: AgentWorkItem }) {
  const controls = useContext(WorkItemActionsContext);
  const state = controls?.states[item.id];
  const stoppable = ["queued", "running", "cancelling"].includes(item.status);
  const retryable = ["failed", "cancelled"].includes(item.status);
  if (!stoppable && !retryable) return null;
  const stopping = item.status === "cancelling" || (stoppable && state?.action === "stop" && (state.pending || state.requested));
  const retried = Boolean(item.retry_work_item_id) || (retryable && state?.action === "retry" && state.requested);
  const label = stopping ? "Stopping…" : retried ? "Retried"
    : state?.pending ? state.action === "stop" ? "Stopping…" : "Retrying…"
    : stoppable ? "Stop" : "Retry";
  const error = state?.error && (state.action === "retry" ? !item.retry_work_item_id : ["queued", "running"].includes(item.status))
    ? state.error : null;
  const Icon = stoppable ? Square : RotateCcw;
  return <div className="work-item-actions">
    <button type="button" disabled={!controls || state?.pending || stopping || retried}
      aria-label={`${label} request: ${item.title}`}
      onClick={() => controls?.request(item, stoppable ? "stop" : "retry")}>
      <Icon size={13} aria-hidden="true" />{label}
    </button>
    {error && <span role="alert">{error}</span>}
  </div>;
}
