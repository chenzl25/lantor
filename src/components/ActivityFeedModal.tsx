import { ArrowUp, Bell, Check, Hash, Inbox, MessageSquare, UserRound, X } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties, PointerEvent } from "react";
import type { Agent, ActivityFeedItem, ActivityFeedKind, OwnerProfile } from "../types";
import { firstLines, formatTime, ownerAsAvatarAgent } from "../ui-utils";
import { AgentAvatar } from "./AgentAvatar";

type ActivityFeedFilter = "all" | "unread" | ActivityFeedKind;

type ActivityFeedModalProps = {
  open: boolean;
  items: ActivityFeedItem[];
  agents: Agent[];
  ownerProfile: OwnerProfile;
  onOpenItem: (item: ActivityFeedItem) => void;
  onMarkItemRead: (item: ActivityFeedItem) => void;
  onDismissItem: (item: ActivityFeedItem) => void;
  onDismissItems: (items: ActivityFeedItem[]) => void;
  onMarkAllRead: (items: ActivityFeedItem[]) => void;
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
const ACTIVITY_FEED_INITIAL_VISIBLE = 30;
const ACTIVITY_FEED_LOAD_MORE_STEP = 30;

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

function activityTimestampValue(item: ActivityFeedItem) {
  const value = new Date(item.timestamp).getTime();
  return Number.isFinite(value) ? value : 0;
}

function sortActivityFeedItems(items: ActivityFeedItem[]) {
  return [...items].sort((left, right) => {
    if (left.unread !== right.unread) return left.unread ? -1 : 1;
    return activityTimestampValue(right) - activityTimestampValue(left);
  });
}

export function ActivityFeedModal({
  open,
  items,
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
  const [visibleCount, setVisibleCount] = useState(ACTIVITY_FEED_INITIAL_VISIBLE);
  const [displayItems, setDisplayItems] = useState<ActivityFeedItem[]>(() => sortActivityFeedItems(items));
  const [swipeState, setSwipeState] = useState<{
    itemId: string;
    startX: number;
    startY: number;
    offsetX: number;
    tracking: boolean;
  } | null>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
  const suppressNextClickRef = useRef(false);
  const unreadCount = displayItems.filter((item) => item.unread).length;
  const pendingItems = useMemo(() => {
    const displayedById = new Map(displayItems.map((item) => [item.id, item]));
    return items.filter((item) => {
      const displayed = displayedById.get(item.id);
      if (!displayed) return true;
      return activityTimestampValue(item) > activityTimestampValue(displayed);
    });
  }, [displayItems, items]);
  const filteredItems = useMemo(() => {
    if (filter === "all") return displayItems;
    if (filter === "unread") return displayItems.filter((item) => item.unread);
    return displayItems.filter((item) => item.kind === filter);
  }, [displayItems, filter]);
  const filteredUnreadCount = filteredItems.filter((item) => item.unread).length;
  const visibleItems = useMemo(
    () => filteredItems.slice(0, visibleCount),
    [filteredItems, visibleCount],
  );
  const hiddenCount = Math.max(0, filteredItems.length - visibleItems.length);

  useEffect(() => {
    setVisibleCount(ACTIVITY_FEED_INITIAL_VISIBLE);
  }, [displayItems.length, filter]);

  useEffect(() => {
    if (!open) return;
    setVisibleCount(ACTIVITY_FEED_INITIAL_VISIBLE);
  }, [open]);

  useEffect(() => {
    if (open) return;
    setDisplayItems(sortActivityFeedItems(items));
  }, [items, open]);

  useEffect(() => {
    if (!open) return;
    setDisplayItems((current) => {
      const latestById = new Map(items.map((item) => [item.id, item]));
      let changed = false;
      const next: ActivityFeedItem[] = [];

      for (const displayed of current) {
        const latest = latestById.get(displayed.id);
        if (!latest) {
          changed = true;
          continue;
        }
        if (activityTimestampValue(latest) <= activityTimestampValue(displayed)) {
          if (latest !== displayed) changed = true;
          next.push(latest);
        } else {
          next.push(displayed);
        }
      }

      return changed ? sortActivityFeedItems(next) : current;
    });
  }, [items, open]);

  useEffect(() => {
    if (!open) return;
    function onKey(event: KeyboardEvent) {
      if (event.key === "Escape") onClose();
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

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
      onDismissItem(item);
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
    setDisplayItems(items);
    setVisibleCount(ACTIVITY_FEED_INITIAL_VISIBLE);
    window.requestAnimationFrame(() => {
      bodyRef.current?.scrollTo({ top: 0, behavior: "smooth" });
    });
  }

  return (
    <div className="search-backdrop" onClick={onClose}>
      <section className="activity-feed-panel activity-panel" onClick={(event) => event.stopPropagation()}>
        <header className="activity-feed-head">
          <div>
            <h2>Activity</h2>
            <p>{displayItems.length} active · {unreadCount} unread</p>
          </div>
          <div className="activity-feed-head-actions">
            <button
              className="activity-feed-mark-all"
              disabled={filteredUnreadCount === 0}
              onClick={() => onMarkAllRead(filteredItems)}
            >
              Mark all read
            </button>
            <button
              className="activity-feed-dismiss-all"
              disabled={filteredItems.length === 0}
              onClick={() => onDismissItems(filteredItems)}
            >
              Dismiss all
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
              onClick={() => setFilter(item.value)}
            >
              {item.label}
            </button>
          ))}
        </div>

        <div className="activity-feed-body" ref={bodyRef}>
          {pendingItems.length > 0 && (
            <div className="activity-feed-new-activity">
              <button type="button" onClick={showPendingItems}>
                <ArrowUp size={16} />
                <span>
                  {pendingItems.length === 1 ? "1 new activity" : `${pendingItems.length} new activities`}
                </span>
              </button>
            </div>
          )}

          {filteredItems.length === 0 && (
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
                  <div className="activity-feed-row-main">
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
                        onClick={(event) => {
                          event.stopPropagation();
                          onMarkItemRead(item);
                        }}
                      >
                        <Check size={19} />
                      </button>
                    ) : null}
                    <button
                      className="activity-feed-dismiss"
                      title="Dismiss"
                      onClick={(event) => {
                        event.stopPropagation();
                        onDismissItem(item);
                      }}
                    >
                      <X size={18} />
                    </button>
                  </div>
                </article>
              </div>
            );
          })}

          {hiddenCount > 0 && (
            <div className="activity-feed-load-more">
              <button
                type="button"
                onClick={() =>
                  setVisibleCount((current) =>
                    Math.min(filteredItems.length, current + ACTIVITY_FEED_LOAD_MORE_STEP),
                  )
                }
              >
                Show {Math.min(hiddenCount, ACTIVITY_FEED_LOAD_MORE_STEP)} more
                <span>{hiddenCount} hidden</span>
              </button>
            </div>
          )}
        </div>
      </section>
    </div>
  );
}
