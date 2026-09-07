import { DialogSurface } from "./DialogSurface";
import { Check, MessageSquare, X } from "lucide-react";
import { type MouseEvent as ReactMouseEvent } from "react";
import { Channel, Message } from "../types";
import { firstLines, formatTime } from "../ui-utils";

type ThreadBrowserModalProps = {
  open: boolean;
  channels: Channel[];
  threads: Message[];
  activeThreadId: string | null;
  replyCounts: Record<string, number>;
  unreadCounts: Record<string, number>;
  onOpenThread: (message: Message) => void;
  onToggleFollow: (message: Message) => void | Promise<void>;
  onClose: () => void;
};

function channelLabel(channels: Channel[], channelId: string) {
  const channel = channels.find((item) => item.id === channelId);
  if (!channel) return "Unknown";
  if (channel.kind === "dm") return "Direct message";
  return `#${channel.name}`;
}

function isPrimaryUnmodifiedClick(event: ReactMouseEvent<HTMLElement>) {
  return event.button === 0 && !event.ctrlKey && !event.metaKey && !event.altKey && !event.shiftKey;
}

export function ThreadBrowserModal({
  open,
  channels,
  threads,
  activeThreadId,
  replyCounts,
  unreadCounts,
  onOpenThread,
  onToggleFollow,
  onClose,
}: ThreadBrowserModalProps) {
  if (!open) return null;

  return (
    <DialogSurface label="Threads" backdropClassName="search-backdrop" className="thread-browser-panel" onClose={onClose}>
        <header className="thread-browser-head">
          <div className="search-input-icon"><MessageSquare size={24} /></div>
          <div>
            <h2>Threads</h2>
            <p>{threads.length} active</p>
          </div>
          <button className="search-clear" onClick={onClose} aria-label="Close threads">
            <X size={18} />
          </button>
        </header>

        <div className="thread-browser-body">
          {threads.length === 0 && (
            <div className="search-empty">
              <MessageSquare size={32} />
              <h3>No active threads</h3>
              <p>Threads appear here after a channel or DM message gets replies.</p>
            </div>
          )}

          {threads.map((thread) => {
            const unread = unreadCounts[thread.id] ?? 0;
            const replies = replyCounts[thread.id] ?? 0;
            return (
              <article
                key={thread.id}
                className={`thread-browser-row ${thread.id === activeThreadId ? "selected" : ""} ${unread ? "has-unread" : ""}`}
                onClick={(event) => {
                  if (!isPrimaryUnmodifiedClick(event)) return;
                  onOpenThread(thread);
                }}
                onContextMenu={(event) => event.stopPropagation()}
              >
                <div className="thread-browser-content" role="button" tabIndex={0} aria-label={`Open thread by ${thread.sender_name}`}
                    onKeyDown={(event) => {
                      if (event.target !== event.currentTarget || (event.key !== "Enter" && event.key !== " ")) return;
                      event.preventDefault();
                      onOpenThread(thread);
                    }}>
                  <div className="meta">
                    <span>{channelLabel(channels, thread.channel_id)}</span>
                    <strong>{thread.sender_name}</strong>
                    <time>{formatTime(thread.created_at)}</time>
                  </div>
                  <p>{firstLines(thread.body, 2)}</p>
                  <small>
                    {replies} {replies === 1 ? "reply" : "replies"}
                    {unread > 0 ? ` · ${unread} new` : ""}
                  </small>
                </div>
                {thread.thread_followed && (
                  <button
                    className="thread-browser-follow"
                    title="Stop following this thread"
                    onClick={(event) => {
                      event.stopPropagation();
                      onToggleFollow(thread);
                    }}
                  >
                    <Check size={18} />
                  </button>
                )}
              </article>
            );
          })}
        </div>
    </DialogSurface>
  );
}
