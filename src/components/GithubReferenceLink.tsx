import {
  Check,
  CircleCheck,
  CircleDot,
  CircleSlash,
  Clock3,
  GitMerge,
  GitPullRequest,
  GitPullRequestClosed,
  GitPullRequestDraft,
  X,
} from "lucide-react";
import { type AnchorHTMLAttributes, type ReactNode, useCallback, useEffect, useSyncExternalStore } from "react";
import {
  type GithubIssueLink,
  type GithubReferenceIndex,
  type GithubReferenceStatus,
  githubReferenceItem,
  githubReferenceKey,
  githubReferenceLabel,
  githubReferenceStatus,
  lookedUpGithubReference,
  requestGithubReferenceLookup,
  subscribeGithubReferenceLookups,
} from "../github-references";
import type { GithubReferenceItem } from "../types";

type GithubReferenceLinkProps = AnchorHTMLAttributes<HTMLAnchorElement> & {
  href: string;
  link: GithubIssueLink;
  references: GithubReferenceIndex;
  /** Replace the children (a bare URL) with `rw#123`. */
  compactLabel?: boolean;
  /** Show the PR/issue title after the label once its state is known. */
  showTitle?: boolean;
  /** Leave out the state icon when the surrounding row already shows it. */
  hideStatusIcon?: boolean;
  children?: ReactNode;
};

const STATUS_LABELS: Record<GithubReferenceStatus, string> = {
  open: "Open",
  draft: "Draft",
  merged: "Merged",
  closed: "Closed",
  completed: "Closed as completed",
  not_planned: "Closed as not planned",
};
const CHECK_LABELS: Record<string, string> = { success: "checks passed", failure: "checks failing", pending: "checks running" };

export function GithubStatusIcon({ kind, status, size = 12, className = "github-reference-icon" }: {
  kind: "pull" | "issue";
  status: GithubReferenceStatus;
  size?: number;
  className?: string;
}) {
  const props = { size, "aria-hidden": true, className } as const;
  if (kind === "issue") {
    if (status === "completed") return <CircleCheck {...props} />;
    if (status === "not_planned") return <CircleSlash {...props} />;
    return <CircleDot {...props} />;
  }
  if (status === "draft") return <GitPullRequestDraft {...props} />;
  if (status === "merged") return <GitMerge {...props} />;
  if (status === "closed") return <GitPullRequestClosed {...props} />;
  return <GitPullRequest {...props} />;
}

function ChecksIcon({ status }: { status: string }) {
  const props = { size: 11, "aria-hidden": true, className: `github-reference-checks ${status}` } as const;
  if (status === "success") return <Check {...props} />;
  if (status === "failure") return <X {...props} />;
  if (status === "pending") return <Clock3 {...props} />;
  return null;
}

/**
 * The state of a PR or issue: from the synced queues, or looked up on demand
 * (then null until the lookup finds it). Null for no link.
 */
export function useGithubReferenceItem(link: GithubIssueLink | null, references: GithubReferenceIndex): GithubReferenceItem | null {
  const queued = link ? githubReferenceItem(references, link) : null;
  const key = link ? githubReferenceKey(link) : "";
  const getLookedUp = useCallback(() => (key ? lookedUpGithubReference(key) : null), [key]);
  const lookedUp = useSyncExternalStore(subscribeGithubReferenceLookups, getLookedUp, getLookedUp);
  const nameWithOwner = link?.nameWithOwner;
  const number = link?.number;
  useEffect(() => {
    if (!queued && nameWithOwner && number !== undefined) requestGithubReferenceLookup({ nameWithOwner, number });
  }, [queued, nameWithOwner, number]);
  return queued ?? lookedUp;
}

/**
 * A link to a GitHub PR or issue with a state icon, a CI mark (open PRs), an
 * optional truncated title and the full title in a tooltip. Items in the synced
 * GitHub queues have their state locally; others are looked up on demand and
 * stay plain links until (or unless) that lookup finds them.
 */
export function GithubReferenceLink({
  href,
  link,
  references,
  compactLabel = false,
  showTitle = false,
  hideStatusIcon = false,
  children,
  className,
  ...props
}: GithubReferenceLinkProps) {
  const item = useGithubReferenceItem(link, references);
  const label = compactLabel ? githubReferenceLabel(references, link) : children;
  // Links generated from `#123` use /issues/; point them at the real page.
  const resolvedHref = item && href === `https://github.com/${link.nameWithOwner}/issues/${link.number}` ? item.url || href : href;
  const status = item ? githubReferenceStatus(item) : null;
  const checks = item?.kind === "pull" ? CHECK_LABELS[item.checks_status] : undefined;
  const title = item && status
    ? `${item.kind === "pull" ? "Pull request" : "Issue"} ${githubReferenceLabel(references, link)}: ${item.title}\n${STATUS_LABELS[status]}${checks ? ` · ${checks}` : ""}`
    : undefined;

  return (
    <a
      {...props}
      href={resolvedHref}
      className={["github-reference", className].filter(Boolean).join(" ")}
      data-status={status ?? undefined}
      title={title ?? props.title}
      target="_blank"
      rel="noreferrer"
    >
      <span className="github-reference-ref">
        {item && status && !hideStatusIcon && <GithubStatusIcon kind={item.kind} status={status} />}
        {label}
        {item && checks && <ChecksIcon status={item.checks_status} />}
      </span>
      {showTitle && item?.title && <span className="github-reference-title">{item.title}</span>}
    </a>
  );
}
