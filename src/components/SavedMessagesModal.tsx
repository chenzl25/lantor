import { Bookmark, Hash, MessageSquare, X } from "lucide-react";
import { useEffect } from "react";
import type { Agent, OwnerProfile, SavedMessage } from "../types";
import { firstLines, formatTime, ownerAsAvatarAgent } from "../ui-utils";
import { AgentAvatar } from "./AgentAvatar";

type SavedMessagesModalProps = {
  open: boolean;
  items: SavedMessage[];
  agents: Agent[];
  ownerProfile: OwnerProfile;
  onOpenItem: (item: SavedMessage) => void;
  onUnsaveItem: (item: SavedMessage) => void;
  onClose: () => void;
};

export function SavedMessagesModal({
  open,
  items,
  agents,
  ownerProfile,
  onOpenItem,
  onUnsaveItem,
  onClose,
}: SavedMessagesModalProps) {
  useEffect(() => {
    if (!open) return;
    function onKey(event: KeyboardEvent) {
      if (event.key === "Escape") onClose();
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;

  return (
    <div className="search-backdrop" onClick={onClose}>
      <section className="activity-feed-panel saved-panel" onClick={(event) => event.stopPropagation()}>
        <header className="activity-feed-head">
          <div>
            <h2>Saved</h2>
            <p>{items.length} saved {items.length === 1 ? "message" : "messages"}</p>
          </div>
          <button className="activity-feed-back" onClick={onClose} aria-label="Close saved messages">
            <X size={18} />
          </button>
        </header>

        <div className="activity-feed-body">
          {items.length === 0 && (
            <div className="search-empty">
              <Bookmark size={34} />
              <h3>No saved messages</h3>
              <p>Use the message save action or right-click a message to track it here.</p>
            </div>
          )}

          {items.map((item) => {
            const Icon = item.thread_root_id ? MessageSquare : Hash;
            const senderAgent = item.sender_role === "owner"
              ? ownerAsAvatarAgent(ownerProfile)
              : agents.find((agent) => agent.display_name === item.sender_name || agent.handle === item.sender_name.replace(/^@/, "")) ?? null;
            return (
              <article
                key={item.id}
                className="activity-feed-row saved-row"
                onClick={() => onOpenItem(item)}
              >
                <span className="activity-feed-row-avatar" aria-hidden="true">
                  {senderAgent ? (
                    <AgentAvatar agent={senderAgent} size="md" showStatus={false} />
                  ) : (
                    <span className="search-result-fallback-avatar">{item.sender_name.slice(0, 1) || "S"}</span>
                  )}
                </span>
                <div className="activity-feed-row-main">
                  <div className="activity-feed-row-meta">
                    <strong>{item.sender_name || "Saved message"}</strong>
                    <span>#{item.channel_name}</span>
                    <time>{formatTime(item.message_created_at)}</time>
                    <em>{item.thread_root_id ? "thread" : "channel"}</em>
                  </div>
                  <h3>
                    <Icon size={18} />
                    {item.sender_name || "Saved message"}
                  </h3>
                  <p>{firstLines(item.body, 3) || "Empty message"}</p>
                  <small>Open source</small>
                </div>
                <button
                  className="activity-feed-check saved-unsave"
                  title="Unsave"
                  onClick={(event) => {
                    event.stopPropagation();
                    onUnsaveItem(item);
                  }}
                >
                  <X size={18} />
                </button>
              </article>
            );
          })}
        </div>
      </section>
    </div>
  );
}
