import { DialogHeader } from "./DialogHeader";
import { DialogSurface } from "./DialogSurface";
import { Check, MessageSquare } from "lucide-react";
import { type MouseEvent as ReactMouseEvent } from "react";
import { Channel, Message } from "../types";
import { firstLines, formatTime } from "../ui-utils";
import { markdownPreviewText } from "../markdown-preview";

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
        <DialogHeader title="Threads" subtitle={`${threads.length} active`} closeLabel="Close threads" onClose={onClose} />

        <div className="thread-browser-body">
          {threads.length === 0 && (
            <div className="search-empty">
              <MessageSquare size={32} />
              <h3>No threads yet</h3>
              <p>A thread starts when a message gets replies.</p>
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
                  <p>{firstLines(markdownPreviewText(thread.body), 2)}</p>
                  <small>
                    {replies} {replies === 1 ? "reply" : "replies"}
                    {unread > 0 ? ` · ${unread} new` : ""}
                  </small>
                </div>
                {thread.thread_followed && (
                  <button
                    className="thread-browser-follow"
                    title="Unfollow"
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
