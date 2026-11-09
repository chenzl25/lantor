import {
  Bell,
  Bot,
  Check,
  CircleDot,
  GitCommitHorizontal,
  GitPullRequest,
  Hash,
  MessageSquare,
  MessagesSquare,
  ShieldAlert,
  Tag,
} from "lucide-react";

import { githubInboxReasonLabel } from "../github-inbox";
import { type GithubReferenceIndex, githubReferenceStatus } from "../github-references";
import type { GithubInboxItem } from "../types";
import { formatRelativeTime, formatTime } from "../ui-utils";
import { GithubReferenceLink, GithubStatusIcon, useGithubReferenceItem } from "./GithubReferenceLink";

function SubjectIcon({ type }: { type: string }) {
  const props = { size: 16, "aria-hidden": true } as const;
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
  onDone: (item: GithubInboxItem) => void;
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
  onDone,
  onOpenThread,
  onOpenChannel,
  onReview,
}: GithubInboxRowProps) {
  const link = item.number !== null && (item.subject_type === "PullRequest" || item.subject_type === "Issue")
    ? { nameWithOwner: item.repository, number: item.number }
    : null;
  // Like GitHub, the type icon shows the PR or issue state once it is known.
  const reference = useGithubReferenceItem(link, references);
  const status = reference ? githubReferenceStatus(reference) : null;
  const threadRootId = item.thread_root_id;
  const showChannel = Boolean(channelName && onOpenChannel);
  const reason = githubInboxReasonLabel(item.reason);
  const updated = (
    <time dateTime={item.updated_at} title={formatTime(item.updated_at)}>
      {formatRelativeTime(item.updated_at)}
    </time>
  );
  return (
    <article
      className={["github-inbox-row", item.unread ? "unread" : "", highlighted ? "highlighted" : ""]
        .filter(Boolean).join(" ")}
      data-thread-id={item.thread_id}
      onClick={() => onOpen(item)}
    >
      <span className="github-inbox-type" data-status={status ?? undefined} aria-hidden="true">
        {item.unread && <span className="github-inbox-unread-dot" />}
        {reference && status
          ? <GithubStatusIcon kind={reference.kind} status={status} size={16} className="github-inbox-type-icon" />
          : <SubjectIcon type={item.subject_type} />}
      </span>
      <div
        className="github-inbox-row-main"
        role="button"
        tabIndex={0}
        aria-label={`${item.unread ? "Unread: " : ""}Open ${item.title} on GitHub`}
        onKeyDown={(event) => {
          if (event.target !== event.currentTarget || (event.key !== "Enter" && event.key !== " ")) return;
          event.preventDefault();
          onOpen(item);
        }}
      >
        <div className="github-inbox-row-meta">
          {link ? (
            <GithubReferenceLink
              href={item.url}
              link={link}
              references={references}
              compactLabel
              hideStatusIcon
              onClick={(event) => {
                event.preventDefault();
                event.stopPropagation();
                onOpen(item);
              }}
            />
          ) : (
            <span className="github-inbox-row-repository">{item.repository}</span>
          )}
          {/* Phones have no reason and time columns; they follow the reference. */}
          <span className="github-inbox-row-inline">
            <em>{reason}</em>
            {updated}
          </span>
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
      <span className="github-inbox-row-reason">{reason}</span>
      {/* As on GitHub, the row's action takes the time's place under the pointer. */}
      <div className="github-inbox-row-end">
        {updated}
        <button
          type="button"
          className="github-inbox-done"
          title="Done"
          aria-label="Done"
          onClick={(event) => {
            event.stopPropagation();
            onDone(item);
          }}
        >
          <Check size={16} />
        </button>
      </div>
    </article>
  );
}
