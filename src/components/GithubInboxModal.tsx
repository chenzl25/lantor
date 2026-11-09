import { ExternalLink, Github, RefreshCw } from "lucide-react";
import { type ReactNode, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import {
  GITHUB_INBOX_FILTERS,
  type GithubInboxFilter,
  githubInboxCounts,
  githubInboxRepositories,
  matchesGithubInboxFilter,
} from "../github-inbox";
import { getGithubReferenceIndex, subscribeGithubReferences } from "../github-references";
import { useGithubInboxMarks } from "../hooks/useGithubInboxMarks";
import { useGithubInboxOrder } from "../hooks/useGithubInboxOrder";
import type { Agent, Channel, GithubInbox, GithubInboxItem } from "../types";
import { formatTime } from "../ui-utils";
import { DialogHeader } from "./DialogHeader";
import { DialogSurface } from "./DialogSurface";
import { GithubInboxRow } from "./GithubInboxRow";
import { GithubReviewDialog, type GithubReviewTarget } from "./GithubReviewDialog";

/** Opening the inbox syncs it when the last sync is older than this. */
const STALE_AFTER_MS = 30_000;
const GITHUB_NOTIFICATIONS_URL = "https://github.com/notifications";

type GithubInboxModalProps = {
  open: boolean;
  mobileNavigation?: ReactNode;
  inbox: GithubInbox | undefined;
  /** Names the channels bound to a thread's repository. */
  channels: readonly Channel[];
  /** Agents that can review a pull request in a channel. */
  reviewAgents: (channelId: string) => Agent[];
  onRefresh: () => Promise<void>;
  onMarkRead: (threadId: string) => Promise<void>;
  onMarkDone: (threadId: string) => Promise<void>;
  onOpenUrl: (url: string) => void;
  onOpenThread: (threadRootId: string) => void;
  /** Opens the GitHub tab of the thread's channel, at the thread. */
  onOpenChannel: (item: GithubInboxItem) => void;
  onCreateReviewTask: (channelId: string, pullNumber: number, agentId: string) => Promise<unknown>;
  onClose: () => void;
};

export function GithubInboxModal({
  open,
  mobileNavigation,
  inbox,
  channels,
  reviewAgents,
  onRefresh,
  onMarkRead,
  onMarkDone,
  onOpenUrl,
  onOpenThread,
  onOpenChannel,
  onCreateReviewTask,
  onClose,
}: GithubInboxModalProps) {
  const [filter, setFilter] = useState<GithubInboxFilter>("participating");
  const [repository, setRepository] = useState("");
  const [refreshing, setRefreshing] = useState(false);
  const [review, setReview] = useState<{ channelId: string; target: GithubReviewTarget } | null>(null);
  const refreshingRef = useRef(false);
  const references = useSyncExternalStore(subscribeGithubReferences, getGithubReferenceIndex, getGithubReferenceIndex);
  const { items, mark, actionError, setActionError } = useGithubInboxMarks(inbox?.items ?? [], onMarkRead, onMarkDone);
  const channelNames = useMemo(() => new Map(channels.map((channel) => [channel.id, channel.name])), [channels]);
  const repositories = useMemo(() => githubInboxRepositories(items), [items]);
  const repositoryItems = useMemo(
    () => (repository ? items.filter((item) => item.repository === repository) : items),
    [items, repository],
  );
  const counts = useMemo(() => githubInboxCounts(repositoryItems), [repositoryItems]);
  const filteredItems = useMemo(
    () => repositoryItems.filter((item) => matchesGithubInboxFilter(item, filter)),
    [repositoryItems, filter],
  );
  // Each opening of the panel orders unread threads first again.
  const visibleItems = useGithubInboxOrder(filteredItems, open);

  async function refresh() {
    if (refreshingRef.current) return;
    refreshingRef.current = true;
    setRefreshing(true);
    setActionError(null);
    try {
      await onRefresh();
    } catch (error) {
      setActionError(error instanceof Error ? error.message : String(error));
    } finally {
      refreshingRef.current = false;
      setRefreshing(false);
    }
  }

  useEffect(() => {
    if (!open) return;
    const syncedAt = inbox?.synced_at ? new Date(inbox.synced_at).getTime() : 0;
    if (!inbox?.enabled || !Number.isFinite(syncedAt) || Date.now() - syncedAt > STALE_AFTER_MS) {
      void refresh();
    }
    // Only when the panel opens; the backend keeps it current while it stays open.
  }, [open]);

  useEffect(() => {
    if (repository && !repositories.some((entry) => entry.repository === repository)) setRepository("");
  }, [repositories, repository]);

  if (!open) return null;

  function openItem(item: GithubInboxItem) {
    onOpenUrl(item.url);
    if (item.unread) void mark(item, "read");
  }

  function startReview(item: GithubInboxItem) {
    if (!item.channel_id || item.number === null) return;
    setReview({
      channelId: item.channel_id,
      target: { repository: item.repository, number: item.number, title: item.title, authorLogin: null },
    });
  }

  const unread = counts.participating;
  const error = actionError ?? inbox?.error ?? null;
  const subtitle = inbox?.account_login
    ? `@${inbox.account_login} · ${unread} unread`
    : refreshing ? "Connecting to GitHub…" : "Your GitHub notifications";

  return (
    <DialogSurface
      label="GitHub notifications"
      backdropClassName="search-backdrop"
      className="activity-feed-panel github-inbox-panel"
      onClose={onClose}
    >
      <DialogHeader
        title="GitHub"
        subtitle={subtitle}
        closeLabel="Close GitHub notifications"
        onClose={onClose}
        actions={<>
          <button
            type="button"
            className="github-inbox-refresh"
            disabled={refreshing}
            title={inbox?.synced_at ? `Checked ${formatTime(inbox.synced_at)}` : "Check GitHub now"}
            aria-label="Check GitHub now"
            onClick={() => void refresh()}
          >
            <RefreshCw size={16} className={refreshing ? "spin" : undefined} />
          </button>
          <button
            type="button"
            className="github-inbox-open-github"
            title="Open notifications on GitHub"
            aria-label="Open notifications on GitHub"
            onClick={() => onOpenUrl(GITHUB_NOTIFICATIONS_URL)}
          >
            <ExternalLink size={16} />
          </button>
        </>}
      />

      <div className="activity-feed-filters github-inbox-filters" role="tablist" aria-label="Notification filters">
        {GITHUB_INBOX_FILTERS.map(({ id, label }) => (
          <button
            key={id}
            type="button"
            role="tab"
            aria-selected={filter === id}
            className={filter === id ? "active" : ""}
            onClick={() => setFilter(id)}
          >
            {label}
            {counts[id] > 0 && <span className="github-inbox-filter-count">{counts[id]}</span>}
          </button>
        ))}
        {repositories.length > 1 && (
          <select
            className="github-inbox-repository"
            aria-label="Repository"
            value={repository}
            onChange={(event) => setRepository(event.target.value)}
          >
            <option value="">All repositories</option>
            {repositories.map((entry) => (
              <option key={entry.repository} value={entry.repository}>
                {entry.repository}{entry.unread > 0 ? ` (${entry.unread})` : ""}
              </option>
            ))}
          </select>
        )}
      </div>

      {error && (
        <div className="github-inbox-error" role="alert">
          <span>{error}</span>
          {!actionError && inbox?.error && <small>Lantor reads notifications with the GitHub CLI; check <code>gh auth status</code>.</small>}
        </div>
      )}

      <div className="activity-feed-body">
        {visibleItems.length === 0 && (
          <div className="search-empty">
            <Github size={34} />
            {!inbox?.enabled || (refreshing && items.length === 0) ? (
              <>
                <h3>{refreshing ? "Loading notifications" : "GitHub notifications"}</h3>
                <p>Review requests, mentions and threads you take part in, from every repository.</p>
              </>
            ) : (
              <>
                <h3>Nothing here</h3>
                <p>{filter === "participating" ? "No notifications you take part in." : "No notifications for this filter."}</p>
              </>
            )}
          </div>
        )}

        {visibleItems.map((item) => {
          // The channel's GitHub tab lists pull requests and issues, not releases or advisories.
          const listedInChannel = item.number !== null
            && (item.subject_type === "PullRequest" || item.subject_type === "Issue");
          const channelName = item.channel_id && listedInChannel ? channelNames.get(item.channel_id) ?? null : null;
          const reviewable = item.subject_type === "PullRequest" && item.number !== null && channelName !== null
            && !item.thread_root_id && reviewAgents(item.channel_id!).length > 0;
          return (
            <GithubInboxRow
              key={item.thread_id}
              item={item}
              references={references}
              channelName={channelName}
              onOpen={openItem}
              onDone={(target) => void mark(target, "done")}
              onOpenThread={(target, threadRootId) => {
                if (target.unread) void mark(target, "read");
                onOpenThread(threadRootId);
              }}
              onOpenChannel={onOpenChannel}
              onReview={reviewable ? startReview : undefined}
            />
          );
        })}
      </div>
      {mobileNavigation}
      <GithubReviewDialog
        target={review?.target ?? null}
        agents={review ? reviewAgents(review.channelId) : []}
        onStart={(agentId) => onCreateReviewTask(review!.channelId, review!.target.number, agentId)}
        onClose={() => setReview(null)}
      />
    </DialogSurface>
  );
}
