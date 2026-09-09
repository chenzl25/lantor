import { DialogSurface } from "./DialogSurface";
import { ArrowUp, ArrowLeft, ArrowRight, RefreshCw, Bell, Check, Hash, Inbox, MessageSquare, UserRound, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { CSSProperties, PointerEvent } from "react";
import type { Agent, ActivityFeedItem, ActivityFeedKind, ActivityFeedFilter, ActivityFeedCounts, ActivityFeedPage, ActivityFeedCursor, OwnerProfile } from "../types";
import { firstLines, formatTime, ownerAsAvatarAgent } from "../ui-utils";
import { AgentAvatar } from "./AgentAvatar";

import { apiInvoke } from "../apiClient";

type ActivityFeedModalProps = {
  open: boolean;
  counts: ActivityFeedCounts | null;
  revision: object;
  mentionHandles: string[];
  agents: Agent[];
  ownerProfile: OwnerProfile;
  onOpenItem: (item: ActivityFeedItem) => void;
  onMarkItemRead: (item: ActivityFeedItem) => Promise<void>;
  onDismissItem: (item: ActivityFeedItem) => Promise<void>;
  onDismissItems: (items: ActivityFeedItem[]) => Promise<void>;
  onMarkAllRead: (items: ActivityFeedItem[]) => Promise<void>;
  onClose: () => void;
};

const FILTERS: { value: ActivityFeedFilter; label: string }[] = [
  { value: "all", label: "All" },
  { value: "unread", label: "Unread" },
  { value: "mention", label: "Mentions" },
  { value: "dm", label: "DMs" },
  { value: "thread", label: "Threads" },
  { value: "task", label: "Tasks" },
  { value: "reminder", label: "Reminders" },
];

const SWIPE_DISMISS_THRESHOLD_PX = 86;
const SWIPE_REVEAL_MAX_PX = 96;

function iconFor(kind: ActivityFeedKind) {
  if (kind === "reminder") return Bell;
  if (kind === "dm") return UserRound;
  if (kind === "thread" || kind === "mention") return MessageSquare;
  return Hash;
}

function kindLabel(kind: ActivityFeedKind) {
  return kind === "dm" ? "DM" : kind;
}

function actorAvatarAgent(item: ActivityFeedItem, agents: Agent[], ownerProfile: OwnerProfile) {
  if (item.actorAgentId) return agents.find((agent) => agent.id === item.actorAgentId) ?? null;
  if (item.actorRole === "owner") return ownerAsAvatarAgent(ownerProfile);
  return null;
}

export function ActivityFeedModal({
  open,
  counts,
  revision,
  mentionHandles,
  agents,
  ownerProfile,
  onOpenItem,
  onMarkItemRead,
  onDismissItem,
  onDismissItems,
  onMarkAllRead,
  onClose,
}: ActivityFeedModalProps) {
  const [filter, setFilter] = useState<ActivityFeedFilter>("all");
  const [cursor, setCursor] = useState<{ after?: ActivityFeedCursor; before?: ActivityFeedCursor }>({});
  const [refresh, setRefresh] = useState(0);
  const [page, setPage] = useState<ActivityFeedPage | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [acting, setActing] = useState(false);
  const [hasUpdates, setHasUpdates] = useState(false);
  const generation = useRef(0);
  const running = useRef(false);
  const pending = useRef<null | { id: number; request: { filter: ActivityFeedFilter; mentionHandles: string[]; after?: ActivityFeedCursor; before?: ActivityFeedCursor } }>(null);
  const revisionRef = useRef(revision);
  const [swipeState, setSwipeState] = useState<{
    itemId: string; startX: number; startY: number; offsetX: number; tracking: boolean;
  } | null>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
  const suppressNextClickRef = useRef(false);
  const visibleItems = page?.items ?? [];
  const filteredUnreadCount = visibleItems.filter(item => item.unread).length;

  useEffect(() => {
    if (revisionRef.current !== revision) {
      revisionRef.current = revision;
      if (open) setHasUpdates(true);
    }
  }, [revision, open]);

  useEffect(() => {
    const id = ++generation.current;
    pending.current = null;
    setPage(null);
    setSwipeState(null);
    if (!open) {
      setLoading(false);
      setCursor(current => current.after || current.before ? {} : current);
      return;
    }
    setLoading(true);
    setError("");
    setHasUpdates(false);
    pending.current = { id, request: { filter, mentionHandles, ...cursor } };
    // Serialize requests and retain only the latest queued selection.
    async function drain() {
      if (running.current) return;
      running.current = true;
      try {
        while (pending.current) {
          const job = pending.current;
          pending.current = null;
          try {
            const result = await apiInvoke("load_activity_feed", { request: job.request });
            if (generation.current !== job.id) continue;
            setPage(result);
            bodyRef.current?.scrollTo({ top: 0 });
          } catch (err) {
            if (generation.current === job.id) setError(String(err));
          } finally {
            if (generation.current === job.id) setLoading(false);
          }
        }
      } finally { running.current = false; }
    }
    void drain();
    return () => { generation.current++; pending.current = null; };
  }, [open, filter, cursor, refresh, mentionHandles]);

  async function act(operation: () => Promise<void>) {
    setActing(true);
    setError("");
    try { await operation(); setRefresh(value => value + 1); }
    catch (err) { setError(String(err)); }
    finally { setActing(false); }
  }

  function selectFilter(value: ActivityFeedFilter) {
    setPage(null);
    setLoading(true);
    setFilter(value);
    setCursor({});
  }

  function navigate(value: { before?: ActivityFeedCursor; after?: ActivityFeedCursor }) {
    setPage(null);
    setLoading(true);
    setCursor(value);
  }

  if (!open) return null;

  function startSwipe(item: ActivityFeedItem, event: PointerEvent<HTMLElement>) {
    if (event.pointerType === "mouse") return;
    if ((event.target as HTMLElement).closest("button")) return;
    event.currentTarget.setPointerCapture?.(event.pointerId);
    setSwipeState({
      itemId: item.id,
      startX: event.clientX,
      startY: event.clientY,
      offsetX: 0,
      tracking: true,
    });
  }

  function moveSwipe(item: ActivityFeedItem, event: PointerEvent<HTMLElement>) {
    setSwipeState((current) => {
      if (!current || current.itemId !== item.id || !current.tracking) return current;
      const deltaX = event.clientX - current.startX;
      const deltaY = event.clientY - current.startY;
      if (Math.abs(deltaY) > Math.abs(deltaX) && Math.abs(deltaY) > 12) {
        return { ...current, tracking: false, offsetX: 0 };
      }
      if (deltaX >= 0) return { ...current, offsetX: 0 };
      event.preventDefault();
      return { ...current, offsetX: Math.max(-SWIPE_REVEAL_MAX_PX, deltaX) };
    });
  }

  function endSwipe(item: ActivityFeedItem) {
    const current = swipeState;
    setSwipeState(null);
    if (!current || current.itemId !== item.id) return;
    if (Math.abs(current.offsetX) > 8) {
      suppressNextClickRef.current = true;
      window.setTimeout(() => {
        suppressNextClickRef.current = false;
      }, 0);
    }
    if (current.offsetX <= -SWIPE_DISMISS_THRESHOLD_PX) {
      if (!acting) void act(() => onDismissItem(item));
    }
  }

  function openItem(item: ActivityFeedItem) {
    if (suppressNextClickRef.current) {
      suppressNextClickRef.current = false;
      return;
    }
    onOpenItem(item);
  }

  function showPendingItems() {
    navigate({});
    setRefresh(value => value + 1);
  }

  return (
    <DialogSurface label="Activity" backdropClassName="search-backdrop" className="activity-feed-panel activity-panel" onClose={onClose}>
        <header className="activity-feed-head">
          <div>
            <h2>Activity</h2>
            <p>{counts ? `${counts.total} active · ${counts.unread} unread` : "Activity"}</p>
          </div>
          <div className="activity-feed-head-actions">
            <button
              className="activity-feed-mark-all"
              disabled={acting || loading || filteredUnreadCount === 0}
              onClick={() => void act(() => onMarkAllRead(visibleItems))}
            >
              Mark page read
            </button>
            <button
              className="activity-feed-dismiss-all"
              disabled={acting || loading || visibleItems.length === 0}
              onClick={() => void act(() => onDismissItems(visibleItems))}
            >
              Dismiss page
            </button>
          </div>
          <button className="activity-feed-back" onClick={onClose} aria-label="Close activity">
            <X size={18} />
          </button>
        </header>

        <div className="activity-feed-filters">
          {FILTERS.map((item) => (
            <button
              key={item.value}
              className={filter === item.value ? "active" : ""}
              disabled={acting}
              onClick={() => selectFilter(item.value)}
            >
              {item.label}
            </button>
          ))}
        </div>

        <div className="activity-feed-body" ref={bodyRef} aria-busy={loading}>
          {loading && <p role="status">Loading activity...</p>}
          {error && <div role="alert">{error}<button type="button" title="Retry loading activity" onClick={() => setRefresh(value => value + 1)}><RefreshCw size={16} /></button></div>}
          {hasUpdates && !loading && (
            <div className="activity-feed-new-activity">
              <button type="button" onClick={showPendingItems}>
                <ArrowUp size={16} />
                <span>
                  Updates available
                </span>
              </button>
            </div>
          )}

          {!loading && !error && visibleItems.length === 0 && (
            <div className="search-empty">
              <Inbox size={34} />
              <h3>No activity</h3>
              <p>Mentions, DMs, followed thread updates, active tasks, and due reminders will appear here.</p>
            </div>
          )}

          {visibleItems.map((item) => {
            const Icon = iconFor(item.kind);
            const avatarAgent = actorAvatarAgent(item, agents, ownerProfile);
            const swipeOffset = swipeState?.itemId === item.id ? swipeState.offsetX : 0;
            const excerpt = item.excerpt.trim() === item.title.trim() ? "" : item.excerpt;
            const rowClassName = [
              "activity-feed-row",
              item.unread ? "unread" : "",
              item.kind === "thread" && item.unread ? "new-thread" : "",
            ].filter(Boolean).join(" ");
            return (
              <div
                key={item.id}
                className={`activity-feed-row-shell ${swipeOffset < 0 ? "swiping" : ""}`}
                style={{ "--activity-feed-swipe-x": `${swipeOffset}px` } as CSSProperties}
                onPointerDown={(event) => startSwipe(item, event)}
                onPointerMove={(event) => moveSwipe(item, event)}
                onPointerUp={() => endSwipe(item)}
                onPointerCancel={() => setSwipeState(null)}
              >
                <div className="activity-feed-swipe-action" aria-hidden="true">
                  <X size={18} />
                  <span>Dismiss</span>
                </div>
                <article
                  className={rowClassName}
                  onClick={() => openItem(item)}
                >
                  <span className="activity-feed-row-avatar" aria-hidden="true">
                    {avatarAgent ? (
                      <AgentAvatar agent={avatarAgent} size="md" showStatus={false} />
                    ) : (
                      <span className="search-result-fallback-avatar">{item.actor?.slice(0, 1) || kindLabel(item.kind).slice(0, 1)}</span>
                    )}
                  </span>
                  <div className="activity-feed-row-main" role="button" tabIndex={0} aria-label={`Open ${item.title}`}
                    onKeyDown={(event) => {
                      if (event.target !== event.currentTarget || (event.key !== "Enter" && event.key !== " ")) return;
                      event.preventDefault();
                      openItem(item);
                    }}>
                    <div className="activity-feed-row-meta">
                      {item.actor && <strong>{item.actor}</strong>}
                      <span>{item.surface}</span>
                      <time>{formatTime(item.timestamp)}</time>
                      <em>{kindLabel(item.kind)}</em>
                    </div>
                    <h3>
                      <Icon size={18} />
                      <span>{item.title}</span>
                    </h3>
                    {excerpt && <p>{firstLines(excerpt, 3)}</p>}
                    {item.newCount > 0 ? <small><b>{item.newCount} new</b></small> : null}
                  </div>
                  <div className="activity-feed-row-actions">
                    {item.unread ? <span className="activity-feed-unread-dot" aria-label="Unread" /> : null}
                    {item.unread ? (
                      <button
                        className="activity-feed-check"
                        title="Mark read"
                        disabled={acting || loading}
                        onClick={(event) => {
                          event.stopPropagation();
                          void act(() => onMarkItemRead(item));
                        }}
                      >
                        <Check size={19} />
                      </button>
                    ) : null}
                    <button
                      className="activity-feed-dismiss"
                      title="Dismiss"
                      disabled={acting || loading}
                      onClick={(event) => {
                        event.stopPropagation();
                        void act(() => onDismissItem(item));
                      }}
                    >
                      <X size={18} />
                    </button>
                  </div>
                </article>
              </div>
            );
          })}


        </div>
        <footer className="activity-feed-pagination">
          <button type="button" title="Latest activity" disabled={loading || acting} onClick={showPendingItems}><RefreshCw size={18} /></button>
          <button type="button" title="Previous page" disabled={loading || acting || !page?.previousCursor} onClick={() => navigate({ before: page!.previousCursor! })}><ArrowLeft size={18} /></button>
          <span>{visibleItems.length} items</span>
          <button type="button" title="Next page" disabled={loading || acting || !page?.nextCursor} onClick={() => navigate({ after: page!.nextCursor! })}><ArrowRight size={18} /></button>
        </footer>
    </DialogSurface>
  );
}
