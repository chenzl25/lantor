import { ChevronRight } from "lucide-react";
import { useState } from "react";
import type { ActiveAgentProgress as ReplyProgress } from "./ActivityProgressDock";
import type { MessageRowData } from "../hooks/useMessageRows";
import { formatTime } from "../ui-utils";
import { AgentAvatar } from "./AgentAvatar";
import { isPrimaryUnmodifiedClick } from "../message-interactions";

function compactReplyProgressText(value: string, limit: number) {
  const normalized = value.replace(/\s+/g, " ").trim();
  if (!normalized) return "";
  if (normalized.length <= limit) return normalized;
  return `${normalized.slice(0, Math.max(0, limit - 1)).trim()}...`;
}

function userFacingReplyProgressTitle(value: string) {
  const title = value.trim() || "Working";
  const lowered = title.toLowerCase();
  if (lowered.includes("warm app-server ready") || lowered.includes("warm stream-json ready")) return "Runtime ready";
  if (lowered === "started working" || lowered === "run started" || lowered === "run created") return "Working";
  return title;
}

function userFacingReplyProgressDetail(value: string) {
  const detail = value.trim();
  if (!detail || detail.startsWith("{") || detail.startsWith("[")) return "";
  const parts = detail.split(/[,\n]/).map((part) => part.trim()).filter(Boolean);
  if (parts.length > 0) {
    const entries = parts.map((part) => {
      const separator = part.indexOf("=");
      return separator > 0
        ? [part.slice(0, separator).trim(), part.slice(separator + 1).trim()]
        : null;
    });
    if (entries.every(Boolean)) {
      return entries
        .filter((entry): entry is string[] => Boolean(entry))
        .filter(([key]) => !["pid", "thread_id", "session_id", "request_id", "run_id", "reference_id", "uuid"].includes(key))
        .map(([key, item]) => `${key.replace(/_/g, " ")} ${item}`)
        .join(", ");
    }
  }
  if (detail === "pid unavailable") return "";
  return detail;
}

function replyProgressSummary(progress: ReplyProgress) {
  if (progress.latestActivity) {
    const title = userFacingReplyProgressTitle(progress.latestActivity.summary || progress.latestActivity.title || "Working");
    const detail = compactReplyProgressText(userFacingReplyProgressDetail(progress.latestActivity.detail), 72);
    return {
      title,
      detail: detail && detail !== title ? detail : "",
    };
  }
  if (progress.state === "queued" && progress.queuedItems.length > 0) {
    return {
      title: progress.queuedItems.length === 1 ? "Queued" : `${progress.queuedItems.length} queued`,
      detail: "Waiting to start",
    };
  }
  return {
    title: "Working",
    detail: "",
  };
}

export function MessageReplySummary({ reply, replyCount, unreadReplyCount, onOpenThread }: {
  reply: NonNullable<MessageRowData["reply"]>;
  replyCount: number;
  unreadReplyCount: number;
  onOpenThread: () => void;
}) {
  const [activeReplyMenuPlacement, setPlacement] = useState<"above" | "below">("above");
  const activeReplyProgress = reply.progress;
  const hasActiveReplyProgress = activeReplyProgress.length > 0;
  const activeReplyStatus = hasActiveReplyProgress ? replyProgressSummary(activeReplyProgress[0]).title : "";
  const replyingAgents = activeReplyProgress.map((progress) => progress.agent.display_name).join(", ");
  const replyParticipants = reply.participants;
  const replySummaryClassName = ["thread-reply-summary", hasActiveReplyProgress ? "active-reply" : "", unreadReplyCount > 0 ? "unread-replies" : ""].filter(Boolean).join(" ");

  function updatePlacement(summary: HTMLElement) {
    const menu = summary.querySelector<HTMLElement>(".thread-reply-active-menu");
    if (!menu) return;
    const boundary = summary.closest(".message-list")?.getBoundingClientRect();
    const rect = summary.getBoundingClientRect();
    const height = menu.offsetHeight || menu.getBoundingClientRect().height;
    const below = (boundary?.bottom ?? window.innerHeight) - rect.bottom - 6;
    const above = rect.top - (boundary?.top ?? 0) - 6;
    setPlacement(below < height && above > below ? "above" : "below");
  }
  return <>
    {(hasActiveReplyProgress || (replyCount > 0 && reply.latestAt)) && (
      <button
        type="button"
        className={replySummaryClassName}
        data-active-menu-placement={hasActiveReplyProgress ? activeReplyMenuPlacement : undefined}
        title="View thread replies"
        aria-label={hasActiveReplyProgress
          ? `${activeReplyStatus}${replyingAgents ? `: ${replyingAgents}` : ""}. View thread`
          : `View ${replyCount} ${replyCount === 1 ? "reply" : "replies"} in thread`}
        onPointerEnter={(event) => {
          if (hasActiveReplyProgress) updatePlacement(event.currentTarget);
        }}
        onPointerDown={(event) => event.stopPropagation()}
        onFocus={(event) => {
          if (hasActiveReplyProgress) updatePlacement(event.currentTarget);
        }}
        onClick={(event) => {
          event.stopPropagation();
          if (!isPrimaryUnmodifiedClick(event)) return;
          onOpenThread();
        }}
      >
        {(activeReplyProgress.length > 0 || replyParticipants.length > 0) && (
          <div className="thread-reply-avatars">
            {activeReplyProgress.slice(0, 3).map((progress) => (
              <span key={`active:${progress.key}`}>
                <AgentAvatar agent={progress.agent} size="sm" showStatus={false} />
              </span>
            ))}
            {replyParticipants.map((participant) => (
              <span key={participant.key}>
                {participant.agent ? <AgentAvatar agent={participant.agent} size="sm" title={participant.title} showStatus={false} /> : <span className="thread-reply-fallback-avatar">{participant.fallback}</span>}
              </span>
            ))}
          </div>
        )}
        {hasActiveReplyProgress && (
          <span className="thread-reply-progress-dots" aria-hidden="true">...</span>
        )}
        {replyCount > 0 && (
          <strong>{`${replyCount} ${replyCount === 1 ? "reply" : "replies"}`}</strong>
        )}
        {hasActiveReplyProgress ? (
          <span className="thread-reply-summary-spacer" aria-hidden="true">
            <span className="thread-reply-active-menu" aria-hidden="true">
              {activeReplyProgress.map((progress) => {
                const summary = replyProgressSummary(progress);
                return (
                  <span key={`menu:${progress.key}`} className="thread-reply-active-agent">
                    <AgentAvatar agent={progress.agent} size="sm" showStatus={false} />
                    <span className="thread-reply-active-agent-copy">
                      <span className="thread-reply-active-agent-name">{progress.agent.display_name}</span>
                      <span className="thread-reply-active-agent-status">
                        <span>{summary.title}</span>
                        {summary.detail && <em>{summary.detail}</em>}
                      </span>
                    </span>
                  </span>
                );
              })}
            </span>
          </span>
        ) : reply.latestAt ? (
          <span className="thread-reply-summary-action">
            <time dateTime={reply.latestAt}>Last reply {formatTime(reply.latestAt)}</time>
            <span className="thread-reply-summary-open">View thread</span>
          </span>
        ) : null}
        <ChevronRight className="thread-reply-summary-icon" size={18} aria-hidden="true" />
      </button>
    )}
  </>;
}
