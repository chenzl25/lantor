import { createContext, useContext, useSyncExternalStore } from "react";
import type { Decision, Task, ThreadActivity } from "./types";

/** Tasks untouched this long (no status change, no thread reply) count as stalled. */
export const STALLED_TASK_AFTER_MS = 3 * 24 * 60 * 60 * 1000;

export type DecisionActions = {
  answer: (decision: Decision, optionId: string | null, note: string) => Promise<void>;
  dismiss: (decision: Decision) => Promise<void>;
};

type Listener = () => void;

/**
 * Keyed decision lookup for message rows. Rows subscribe by message id, so a
 * decision update re-renders only the card that changed, not every row.
 */
export class DecisionStore {
  private byMessageId = new Map<string, Decision>();
  private listeners = new Set<Listener>();
  actions: DecisionActions | null = null;

  set(decisions: readonly Decision[] | undefined) {
    const next = new Map<string, Decision>();
    for (const decision of decisions ?? []) next.set(decision.message_id, decision);
    const changed = next.size !== this.byMessageId.size
      || [...next].some(([id, decision]) => this.byMessageId.get(id) !== decision);
    this.byMessageId = next;
    if (changed) for (const listener of this.listeners) listener();
  }

  get = (messageId: string) => this.byMessageId.get(messageId);

  subscribe = (listener: Listener) => {
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); };
  };
}

export const DecisionStoreContext = createContext<DecisionStore | null>(null);

const noopSubscribe = () => () => {};
const noDecision = () => undefined;

export function useMessageDecision(messageId: string): Decision | undefined {
  const store = useContext(DecisionStoreContext);
  return useSyncExternalStore(
    store?.subscribe ?? noopSubscribe,
    store ? () => store.get(messageId) : noDecision,
  );
}

export function useDecisionActions(): DecisionActions | null {
  return useContext(DecisionStoreContext)?.actions ?? null;
}

export type NeedsYouTask = {
  task: Task;
  lastActivityAt: string;
  idleMs: number;
};

export type NeedsYou = {
  decisions: Decision[];
  reviews: NeedsYouTask[];
  stalled: NeedsYouTask[];
  /** Badge count: things only the owner can unblock right now. */
  count: number;
};

function latestTimestamp(...values: (string | null | undefined)[]) {
  let latest: string | null = null;
  let latestTime = -Infinity;
  for (const value of values) {
    if (!value) continue;
    const time = new Date(value).getTime();
    if (Number.isFinite(time) && time > latestTime) {
      latest = value;
      latestTime = time;
    }
  }
  return latest;
}

/**
 * Everything waiting on the owner: open decisions, tasks in review, and active
 * tasks with no movement for {@link STALLED_TASK_AFTER_MS}.
 */
export function deriveNeedsYou(
  decisions: readonly Decision[] | undefined,
  tasks: readonly Task[],
  threadActivities: readonly ThreadActivity[],
  now: number,
  stalledAfterMs = STALLED_TASK_AFTER_MS,
): NeedsYou {
  const open = (decisions ?? [])
    .filter((decision) => decision.status === "open")
    .sort((left, right) => right.created_at.localeCompare(left.created_at));
  const threadActivityByRoot = new Map(threadActivities.map((activity) => [activity.thread_root_id, activity]));
  const withActivity = (task: Task): NeedsYouTask => {
    const lastActivityAt = latestTimestamp(
      task.updated_at,
      task.created_at,
      threadActivityByRoot.get(task.message_id)?.latest_activity_at,
    ) ?? task.updated_at;
    return { task, lastActivityAt, idleMs: Math.max(0, now - new Date(lastActivityAt).getTime()) };
  };
  const reviews = tasks
    .filter((task) => task.status === "in_review")
    .map(withActivity)
    .sort((left, right) => right.idleMs - left.idleMs);
  const stalled = tasks
    .filter((task) => task.status === "todo" || task.status === "in_progress")
    .map(withActivity)
    .filter((item) => item.idleMs >= stalledAfterMs)
    .sort((left, right) => right.idleMs - left.idleMs);
  return { decisions: open, reviews, stalled, count: open.length + reviews.length };
}

export function formatIdle(ms: number) {
  const hours = Math.floor(ms / 3_600_000);
  if (hours < 1) return "just now";
  if (hours < 48) return `${hours}h idle`;
  return `${Math.floor(hours / 24)}d idle`;
}

export function decisionStatusLabel(decision: Decision) {
  if (decision.status === "answered") return "Decided";
  if (decision.status === "dismissed") return "Dismissed";
  if (decision.status === "withdrawn") return "Withdrawn";
  return "Decision needed";
}
