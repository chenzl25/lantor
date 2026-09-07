import type { AgentActivity, AgentWorkItem } from "./types";

export function errorDetail(value: unknown, depth = 0): string {
  if (depth > 5 || value == null) return "";
  if (typeof value === "string") {
    const detail = value.trim();
    if (detail.startsWith("{") || detail.startsWith("[")) {
      try { return errorDetail(JSON.parse(detail), depth + 1); } catch { return detail; }
    }
    if (/^[0-9a-f]{8}-[0-9a-f-]{27,}$/i.test(detail)) return "";
    return detail.replace(/^(codex|claude) warm turn failed:\s*/i, "");
  }
  if (Array.isArray(value)) return value.map((entry) => errorDetail(entry, depth + 1)).filter(Boolean).join("; ");
  if (typeof value !== "object") return "";
  const record = value as Record<string, unknown>;
  for (const key of ["error", "message", "reason", "detail", "errors", "data"]) {
    const detail = errorDetail(record[key], depth + 1);
    if (detail) return detail;
  }
  return "";
}

export function workItemFailure(item: AgentWorkItem, activities: AgentActivity[] = []) {
  const activity = activities.find((entry) => entry.run_id === item.run_id && entry.status === "error"
    && (errorDetail(entry.metadata) || errorDetail(entry.detail)));
  return errorDetail(item.failure_detail) || (activity && (errorDetail(activity.metadata) || errorDetail(activity.detail)))
    || "The request failed without an error description.";
}

export function latestUnretriedFailures(items: AgentWorkItem[]) {
  const latest = new Map<string, AgentWorkItem>();
  for (const item of items) {
    if (item.status !== "failed" || item.retry_work_item_id) continue;
    const key = JSON.stringify([item.agent_id, item.channel_id, item.thread_root_id]);
    const previous = latest.get(key);
    if (!previous || item.created_at > previous.created_at
      || (item.created_at === previous.created_at && item.id > previous.id)) latest.set(key, item);
  }
  return [...latest.values()];
}

// Match bootstrap retention: recent history plus live work and the latest
// unresolved failure on each surface. Busy agents must not evict a failed card.
export function retainWorkItems(items: AgentWorkItem[]) {
  const retained = new Set(items.slice(0, 80).map((item) => item.id));
  for (const item of latestUnretriedFailures(items)) retained.add(item.id);
  return items.filter((item) => retained.has(item.id) || ["queued", "running", "cancelling"].includes(item.status));
}
