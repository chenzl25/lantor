import {
  Bell,
  Check,
  CircleDot,
  ExternalLink,
  GitCommitHorizontal,
  Github,
  GitPullRequest,
  MailOpen,
  MessageSquare,
  MessagesSquare,
  RefreshCw,
  ShieldAlert,
  Tag,
} from "lucide-react";
import { type ReactNode, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import {
  GITHUB_INBOX_FILTERS,
  type GithubInboxFilter,
  githubInboxCounts,
  githubInboxItemsWithPending,
  githubInboxReasonLabel,
  githubInboxRepositories,
  matchesGithubInboxFilter,
} from "../github-inbox";
import { getGithubReferenceIndex, subscribeGithubReferences } from "../github-references";
import type { GithubInbox, GithubInboxItem } from "../types";
import { formatRelativeTime, formatTime } from "../ui-utils";
import { DialogHeader } from "./DialogHeader";
import { DialogSurface } from "./DialogSurface";
import { GithubReferenceLink } from "./GithubReferenceLink";

/** Opening the inbox syncs it when the last sync is older than this. */
const STALE_AFTER_MS = 30_000;
const GITHUB_NOTIFICATIONS_URL = "https://github.com/notifications";

type GithubInboxModalProps = {
  open: boolean;
  mobileNavigation?: ReactNode;
  inbox: GithubInbox | undefined;
  onRefresh: () => Promise<void>;
  onMarkRead: (threadId: string) => Promise<void>;
  onMarkDone: (threadId: string) => Promise<void>;
  onOpenUrl: (url: string) => void;
  onOpenThread: (threadRootId: string) => void;
  onClose: () => void;
};

function SubjectIcon({ type }: { type: string }) {
  const props = { size: 18, "aria-hidden": true } as const;
  if (type === "PullRequest") return <GitPullRequest {...props} />;
  if (type === "Issue") return <CircleDot {...props} />;
  if (type === "Release") return <Tag {...props} />;
  if (type === "Discussion") return <MessagesSquare {...props} />;
  if (type === "Commit") return <GitCommitHorizontal {...props} />;
  if (type === "RepositoryAdvisory" || type === "RepositoryVulnerabilityAlert") return <ShieldAlert {...props} />;
  return <Bell {...props} />;
}

function errorText(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

export function GithubInboxModal({
  open,
  mobileNavigation,
  inbox,
  onRefresh,
  onMarkRead,
  onMarkDone,
  onOpenUrl,
  onOpenThread,
  onClose,
}: GithubInboxModalProps) {
  const [filter, setFilter] = useState<GithubInboxFilter>("participating");
  const [repository, setRepository] = useState("");
  const [refreshing, setRefreshing] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [pendingRead, setPendingRead] = useState<ReadonlySet<string>>(() => new Set());
  const [pendingDone, setPendingDone] = useState<ReadonlySet<string>>(() => new Set());
  const refreshingRef = useRef(false);
  const references = useSyncExternalStore(subscribeGithubReferences, getGithubReferenceIndex, getGithubReferenceIndex);

  const items = useMemo(
    () => githubInboxItemsWithPending(inbox?.items ?? [], { read: pendingRead, done: pendingDone }),
    [inbox?.items, pendingRead, pendingDone],
  );
  const repositories = useMemo(() => githubInboxRepositories(items), [items]);
  const repositoryItems = useMemo(
    () => (repository ? items.filter((item) => item.repository === repository) : items),
    [items, repository],
  );
  const counts = useMemo(() => githubInboxCounts(repositoryItems), [repositoryItems]);
  const visibleItems = useMemo(
    () => repositoryItems.filter((item) => matchesGithubInboxFilter(item, filter)),
    [repositoryItems, filter],
  );

  async function refresh() {
    if (refreshingRef.current) return;
    refreshingRef.current = true;
    setRefreshing(true);
    setActionError(null);
    try {
      await onRefresh();
    } catch (error) {
      setActionError(errorText(error));
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

  async function mark(item: GithubInboxItem, kind: "read" | "done") {
    const setPending = kind === "read" ? setPendingRead : setPendingDone;
    const without = (current: ReadonlySet<string>) => {
      const next = new Set(current);
      next.delete(item.thread_id);
      return next;
    };
    setPending((current) => new Set(current).add(item.thread_id));
    setActionError(null);
    try {
      await (kind === "read" ? onMarkRead(item.thread_id) : onMarkDone(item.thread_id));
    } catch (error) {
      setActionError(errorText(error));
    } finally {
      // The response carries the stored inbox, which now has the mark (or not).
      setPending(without);
    }
  }

  function openItem(item: GithubInboxItem) {
    onOpenUrl(item.url);
    if (item.unread) void mark(item, "read");
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
          const link = item.number !== null && (item.subject_type === "PullRequest" || item.subject_type === "Issue")
            ? { nameWithOwner: item.repository, number: item.number }
            : null;
          return (
            <article
              key={item.thread_id}
              className={`activity-feed-row github-inbox-row ${item.unread ? "unread" : ""}`}
              data-thread-id={item.thread_id}
              onClick={() => openItem(item)}
            >
              <span className="activity-feed-row-avatar github-inbox-type" aria-hidden="true">
                <SubjectIcon type={item.subject_type} />
              </span>
              <div
                className="activity-feed-row-main"
                role="button"
                tabIndex={0}
                aria-label={`Open ${item.title} on GitHub`}
                onKeyDown={(event) => {
                  if (event.target !== event.currentTarget || (event.key !== "Enter" && event.key !== " ")) return;
                  event.preventDefault();
                  openItem(item);
                }}
              >
                <div className="activity-feed-row-meta">
                  {link ? (
                    <GithubReferenceLink
                      href={item.url}
                      link={link}
                      references={references}
                      compactLabel
                      onClick={(event) => {
                        event.preventDefault();
                        event.stopPropagation();
                        openItem(item);
                      }}
                    />
                  ) : (
                    <strong>{item.repository}</strong>
                  )}
                  <time dateTime={item.updated_at} title={formatTime(item.updated_at)}>
                    {formatRelativeTime(item.updated_at)}
                  </time>
                  <em>{githubInboxReasonLabel(item.reason)}</em>
                </div>
                <h3>
                  <span>{item.title}</span>
                </h3>
                {item.thread_root_id && (
                  <button
                    type="button"
                    className="github-inbox-thread"
                    onClick={(event) => {
                      event.stopPropagation();
                      if (item.unread) void mark(item, "read");
                      onOpenThread(item.thread_root_id!);
                    }}
                  >
                    <MessageSquare size={14} />
                    Open Lantor thread
                  </button>
                )}
              </div>
              <div className="activity-feed-row-actions">
                {item.unread ? <span className="activity-feed-unread-dot" aria-label="Unread" /> : null}
                {item.unread ? (
                  <button
                    type="button"
                    className="activity-feed-check"
                    title="Mark as read"
                    aria-label="Mark as read"
                    onClick={(event) => {
                      event.stopPropagation();
                      void mark(item, "read");
                    }}
                  >
                    <MailOpen size={18} />
                  </button>
                ) : null}
                <button
                  type="button"
                  className="activity-feed-dismiss github-inbox-done"
                  title="Done"
                  aria-label="Done"
                  onClick={(event) => {
                    event.stopPropagation();
                    void mark(item, "done");
                  }}
                >
                  <Check size={18} />
                </button>
              </div>
            </article>
          );
        })}
      </div>
      {mobileNavigation}
    </DialogSurface>
  );
}
