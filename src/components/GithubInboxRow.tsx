import {
  Bell,
  Bot,
  Check,
  CircleDot,
  GitCommitHorizontal,
  GitPullRequest,
  Hash,
  MailOpen,
  MessageSquare,
  MessagesSquare,
  ShieldAlert,
  Tag,
} from "lucide-react";

import { githubInboxReasonLabel } from "../github-inbox";
import type { GithubReferenceIndex } from "../github-references";
import type { GithubInboxItem } from "../types";
import { formatRelativeTime, formatTime } from "../ui-utils";
import { GithubReferenceLink } from "./GithubReferenceLink";

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

type GithubInboxRowProps = {
  item: GithubInboxItem;
  references: GithubReferenceIndex;
  highlighted?: boolean;
  /** The bound channel's name, shown as a link to its GitHub tab. */
  channelName?: string | null;
  /** Opens the thread's GitHub page and marks it read. */
  onOpen: (item: GithubInboxItem) => void;
  onMark: (item: GithubInboxItem, kind: "read" | "done") => void;
  onOpenThread: (item: GithubInboxItem, threadRootId: string) => void;
  onOpenChannel?: (item: GithubInboxItem) => void;
  /** Hands the pull request to an agent; absent when it cannot be reviewed here. */
  onReview?: (item: GithubInboxItem) => void;
};

/** One GitHub notification thread, in the GitHub inbox and in a channel's GitHub tab. */
export function GithubInboxRow({
  item,
  references,
  highlighted = false,
  channelName,
  onOpen,
  onMark,
  onOpenThread,
  onOpenChannel,
  onReview,
}: GithubInboxRowProps) {
  const link = item.number !== null && (item.subject_type === "PullRequest" || item.subject_type === "Issue")
    ? { nameWithOwner: item.repository, number: item.number }
    : null;
  const threadRootId = item.thread_root_id;
  const showChannel = Boolean(channelName && onOpenChannel);
  return (
    <article
      className={["activity-feed-row", "github-inbox-row", item.unread ? "unread" : "", highlighted ? "highlighted" : ""]
        .filter(Boolean).join(" ")}
      data-thread-id={item.thread_id}
      onClick={() => onOpen(item)}
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
          onOpen(item);
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
                onOpen(item);
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
        {(threadRootId || showChannel || onReview) && (
          <div className="github-inbox-links">
            {showChannel && (
              <button
                type="button"
                className="github-inbox-channel"
                title={`Open the GitHub tab of #${channelName}`}
                onClick={(event) => {
                  event.stopPropagation();
                  onOpenChannel!(item);
                }}
              >
                <Hash size={13} aria-hidden="true" />
                {channelName}
              </button>
            )}
            {threadRootId ? (
              <button
                type="button"
                className="github-inbox-thread"
                onClick={(event) => {
                  event.stopPropagation();
                  onOpenThread(item, threadRootId);
                }}
              >
                <MessageSquare size={14} />
                Open Lantor thread
              </button>
            ) : onReview ? (
              <button
                type="button"
                className="github-inbox-review"
                onClick={(event) => {
                  event.stopPropagation();
                  onReview(item);
                }}
              >
                <Bot size={14} />
                Review with agent
              </button>
            ) : null}
          </div>
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
              onMark(item, "read");
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
            onMark(item, "done");
          }}
        >
          <Check size={18} />
        </button>
      </div>
    </article>
  );
}
