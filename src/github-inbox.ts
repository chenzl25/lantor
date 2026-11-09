// The owner's GitHub inbox: participating notifications across all
// repositories, synced by the backend (`github/inbox.rs`). The filters and
// their counts match github.com/notifications: each count is the number of
// unread threads for that reason.
import type { GithubInbox, GithubInboxItem } from "./types";

export type GithubInboxFilter = "participating" | "review_requested" | "assign" | "mention" | "team_mention";

export const GITHUB_INBOX_FILTERS: readonly { id: GithubInboxFilter; label: string }[] = [
  { id: "participating", label: "Participating" },
  { id: "review_requested", label: "Review requested" },
  { id: "assign", label: "Assigned" },
  { id: "mention", label: "Mentioned" },
  { id: "team_mention", label: "Team mentioned" },
];

const REASON_LABELS: Record<string, string> = {
  review_requested: "review requested",
  mention: "mention",
  team_mention: "team mention",
  assign: "assigned",
  author: "author",
  comment: "commented",
  state_change: "state change",
  ci_activity: "CI activity",
  security_alert: "security alert",
  approval_requested: "approval requested",
  invitation: "invitation",
  manual: "subscribed",
  subscribed: "subscribed",
};

export function githubInboxReasonLabel(reason: string) {
  return REASON_LABELS[reason] ?? reason.replace(/_/g, " ");
}

export function matchesGithubInboxFilter(item: GithubInboxItem, filter: GithubInboxFilter) {
  return filter === "participating" || item.reason === filter;
}

/** Threads marked read or done here whose write has not come back yet. */
export type GithubInboxPending = {
  read: ReadonlySet<string>;
  done: ReadonlySet<string>;
};

export function githubInboxItemsWithPending(items: readonly GithubInboxItem[], pending: GithubInboxPending) {
  if (pending.read.size === 0 && pending.done.size === 0) return items;
  return items
    .filter((item) => !pending.done.has(item.thread_id))
    .map((item) => (item.unread && pending.read.has(item.thread_id) ? { ...item, unread: false } : item));
}

export function githubInboxCounts(items: readonly GithubInboxItem[]) {
  const counts = Object.fromEntries(GITHUB_INBOX_FILTERS.map(({ id }) => [id, 0])) as Record<GithubInboxFilter, number>;
  for (const item of items) {
    if (!item.unread) continue;
    for (const { id } of GITHUB_INBOX_FILTERS) {
      if (matchesGithubInboxFilter(item, id)) counts[id] += 1;
    }
  }
  return counts;
}

/** Repositories with notifications, most unread first, then by name. */
export function githubInboxRepositories(items: readonly GithubInboxItem[]) {
  const byName = new Map<string, { repository: string; unread: number; total: number }>();
  for (const item of items) {
    const entry = byName.get(item.repository) ?? { repository: item.repository, unread: 0, total: 0 };
    entry.total += 1;
    if (item.unread) entry.unread += 1;
    byName.set(item.repository, entry);
  }
  return [...byName.values()].sort((left, right) => right.unread - left.unread
    || left.repository.localeCompare(right.repository));
}

/** The sidebar badge: unread participating threads, once the inbox is on. */
export function githubInboxUnreadCount(inbox: GithubInbox | undefined) {
  if (!inbox?.enabled) return 0;
  return inbox.items.reduce((count, item) => count + (item.unread ? 1 : 0), 0);
}
