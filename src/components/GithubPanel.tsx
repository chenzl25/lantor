import {
  Bot,
  CircleCheck,
  CircleDot,
  CircleX,
  Clock3,
  ExternalLink,
  GitPullRequest,
  Github,
  Bell,
  LoaderCircle,
  MailOpen,
  MessageCircle,
  RefreshCw,
  Search,
  Settings2,
} from "lucide-react";
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type CSSProperties,
} from "react";

import { apiInvoke, openExternalUrl } from "../apiClient";
import {
  githubInboxReasonLabel,
  githubInboxWorkItems,
  githubNotificationKey,
  githubNotificationsByResource,
} from "../github-inbox";
import { githubComparisonRequestKey, mergeGithubReviewComparisons } from "../github-review";
import { formatRelativeTime } from "../ui-utils";
import {
  derivedReferenceAlias,
  getGithubReferenceIndex,
  loadGithubReferences,
  setGithubReferences,
  subscribeGithubReferences,
} from "../github-references";
import { useGithubInboxMarks } from "../hooks/useGithubInboxMarks";
import type {
  Agent,
  Channel,
  GithubChannelOverview,
  GithubInbox,
  GithubInboxItem,
  GithubIssue,
  GithubIssueDetail,
  GithubIssueTaskResult,
  GithubLabel,
  GithubPullRequest,
  GithubReviewTaskResult,
  GithubReviewComparisons,
} from "../types";
import { GithubInboxRow } from "./GithubInboxRow";
import { GithubIssueDrawer } from "./GithubIssueDrawer";
import { GithubReviewDialog } from "./GithubReviewDialog";
import { Modal } from "./Modal";

type GithubPanelProps = {
  channel: Channel;
  agents: Agent[];
  /** The owner's GitHub inbox; its threads in the bound repository are this tab's unread state. */
  inbox: GithubInbox | undefined;
  /** A notification thread to show, from the GitHub inbox; `nonce` repeats a request. */
  focusThread: { threadId: string; nonce: number } | null;
  onFocusThreadShown: () => void;
  onCreateReviewTask: (pullNumber: number, agentId: string) => Promise<GithubReviewTaskResult>;
  onCreateIssueTask: (issueNumber: number, agentId: string) => Promise<GithubIssueTaskResult>;
  onOpenThread: (threadRootId: string) => void;
  onRefreshInbox: () => Promise<void>;
  onMarkInboxRead: (threadId: string) => Promise<void>;
  onMarkInboxDone: (threadId: string) => Promise<void>;
  onMarkRepositoryRead: (repository: string) => Promise<void>;
};

/** What the panel takes from the GitHub inbox, passed through the conversation. */
export type GithubPanelInboxProps = Pick<
  GithubPanelProps,
  | "inbox"
  | "focusThread"
  | "onFocusThreadShown"
  | "onRefreshInbox"
  | "onMarkInboxRead"
  | "onMarkInboxDone"
  | "onMarkRepositoryRead"
>;

type GithubResource = "pulls" | "issues";
type GithubPanelView = GithubResource | "notifications";
type GithubPullRequestFilter = "review_requested" | "authored" | "linked" | "all";
type GithubIssueFilter = "related" | "assigned" | "authored" | "linked" | "open";

const GITHUB_AUTO_REFRESH_INTERVAL_MS = 60_000;
const githubOverviewCache = new Map<string, GithubChannelOverview>();
const githubRefreshRequests = new Map<string, Promise<GithubChannelOverview>>();
const githubComparisonRequests = new Map<string, Promise<GithubReviewComparisons>>();

function cacheGithubOverview(channelId: string, overview: GithubChannelOverview) {
  githubOverviewCache.set(channelId, overview);
  return overview;
}

function githubQueueSyncedAt(overview: GithubChannelOverview, resource: GithubResource) {
  return resource === "pulls"
    ? overview.binding?.review_queue_synced_at
    : overview.binding?.issue_queue_synced_at;
}

function githubQueueNeedsRefresh(overview: GithubChannelOverview, resource: GithubResource) {
  const refreshedAt = githubQueueSyncedAt(overview, resource);
  if (!overview.binding || !refreshedAt) return Boolean(overview.binding);
  const refreshedAtMs = new Date(refreshedAt).getTime();
  return (
    !Number.isFinite(refreshedAtMs) ||
    Date.now() - refreshedAtMs >= GITHUB_AUTO_REFRESH_INTERVAL_MS
  );
}

function requestGithubRefresh(channelId: string, resource: GithubResource) {
  const requestKey = `${channelId}:${resource}`;
  const existing = githubRefreshRequests.get(requestKey);
  if (existing) return existing;
  const request = apiInvoke(
    resource === "pulls" ? "refresh_github_review_queue" : "refresh_github_issue_queue",
    { channelId },
  ).finally(() => {
    if (githubRefreshRequests.get(requestKey) === request) {
      githubRefreshRequests.delete(requestKey);
    }
  });
  githubRefreshRequests.set(requestKey, request);
  return request;
}

function errorMessage(error: unknown, fallback: string) {
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === "string" && error.trim()) return error;
  return fallback;
}

function formattedUpdate(value: string) {
  const date = new Date(value);
  if (!Number.isFinite(date.getTime())) return value;
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(date);
}

function statusLabel(value: string | null) {
  return value ? value.replace(/_/g, " ") : "";
}

function issueLabelStyle(label: GithubLabel): CSSProperties | undefined {
  const color = label.color.trim();
  if (!/^[0-9a-f]{6}$/i.test(color)) return undefined;
  return {
    backgroundColor: `#${color}1f`,
    borderColor: `#${color}66`,
  };
}

function loginMatches(left: string, right: string) {
  return left.localeCompare(right, undefined, { sensitivity: "accent" }) === 0;
}

function issueRelation(issue: GithubIssue, login: string) {
  const authored = loginMatches(issue.author_login, login);
  const assigned = issue.assignee_logins.some((assignee) => loginMatches(assignee, login));
  return { authored, assigned, involved: issue.is_related && !authored && !assigned };
}

function checkSummaryLabel(pullRequest: GithubPullRequest) {
  const { checks } = pullRequest;
  if (checks.status === "failure") {
    const visibleChecks = checks.failing_checks.slice(0, 2);
    const remaining = checks.failing_checks.length - visibleChecks.length;
    return ["CI failed", visibleChecks.join(", "), remaining > 0 ? `+${remaining}` : ""]
      .filter(Boolean)
      .join(" · ");
  }
  if (checks.status === "pending") {
    return `${checks.pending} ${checks.pending === 1 ? "check" : "checks"} running`;
  }
  if (checks.status === "success") {
    return `${checks.total} ${checks.total === 1 ? "check" : "checks"} passed`;
  }
  return "No CI checks";
}

function staleReviewLabel(pullRequest: GithubPullRequest) {
  const commits = pullRequest.review_commits_ahead;
  if (commits && commits > 0) {
    return `${commits} new ${commits === 1 ? "commit" : "commits"}`;
  }
  return "New commits";
}

function rereviewActionLabel(pullRequest: GithubPullRequest) {
  const commits = pullRequest.review_commits_ahead;
  if (commits && commits > 0) {
    return `Re-review ${commits} ${commits === 1 ? "commit" : "commits"}`;
  }
  return "Review again";
}

export function GithubPanel({
  channel,
  agents,
  inbox,
  focusThread,
  onFocusThreadShown,
  onCreateReviewTask,
  onCreateIssueTask,
  onOpenThread,
  onRefreshInbox,
  onMarkInboxRead,
  onMarkInboxDone,
  onMarkRepositoryRead,
}: GithubPanelProps) {
  const [overview, setOverview] = useState<GithubChannelOverview | null>(
    () => githubOverviewCache.get(channel.id) ?? null,
  );
  const [loading, setLoading] = useState(() => !githubOverviewCache.has(channel.id));
  const [activeResource, setActiveResource] = useState<GithubPanelView>("pulls");
  const [refreshingResource, setRefreshingResource] = useState<GithubResource | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [showBindingModal, setShowBindingModal] = useState(false);
  const [repositoryDraft, setRepositoryDraft] = useState("");
  const [localPathDraft, setLocalPathDraft] = useState("");
  const [aliasDraft, setAliasDraft] = useState("");
  const [reviewLoginDraft, setReviewLoginDraft] = useState("");
  const [bindingBusy, setBindingBusy] = useState(false);
  const [bindingError, setBindingError] = useState<string | null>(null);
  const [reviewTarget, setReviewTarget] = useState<
    { number: number; title: string; authorLogin: string | null } | null
  >(null);
  const [markingAllRead, setMarkingAllRead] = useState(false);
  const [refreshingInbox, setRefreshingInbox] = useState(false);
  const [highlightedThreadId, setHighlightedThreadId] = useState<string | null>(null);
  const pendingScrollThreadRef = useRef<string | null>(null);
  const [rereviewingPullNumber, setRereviewingPullNumber] = useState<number | null>(null);
  const [pullRequestFilter, setPullRequestFilter] =
    useState<GithubPullRequestFilter>("review_requested");
  const [pullRequestSearch, setPullRequestSearch] = useState("");
  const [issueFilter, setIssueFilter] = useState<GithubIssueFilter>("related");
  const [issueSearch, setIssueSearch] = useState("");
  const [selectedIssue, setSelectedIssue] = useState<GithubIssue | null>(null);
  const [issueDetail, setIssueDetail] = useState<GithubIssueDetail | null>(null);
  const [issueDetailLoading, setIssueDetailLoading] = useState(false);
  const [issueDetailError, setIssueDetailError] = useState<string | null>(null);
  const requestEpochRef = useRef(0);
  const detailRequestEpochRef = useRef(0);
  const attentionSnapshotRef = useRef({
    channelId: channel.id,
    count: channel.github_unread_count,
    syncedAt: channel.github_review_synced_at,
    issueSyncedAt: channel.github_issue_synced_at,
  });
  const references = useSyncExternalStore(
    subscribeGithubReferences,
    getGithubReferenceIndex,
    getGithubReferenceIndex,
  );
  const boundRepository = overview?.binding?.name_with_owner ?? null;
  const notificationsShown = channel.github_unread_from_inbox && Boolean(boundRepository);
  const repositoryNotifications = useMemo(
    () => (notificationsShown && boundRepository
      ? githubInboxWorkItems(inbox?.items ?? [], boundRepository)
      : []),
    [boundRepository, inbox?.items, notificationsShown],
  );
  const {
    items: notifications,
    mark: markNotification,
    actionError: notificationError,
    setActionError: setNotificationError,
  } = useGithubInboxMarks(repositoryNotifications, onMarkInboxRead, onMarkInboxDone);
  const notificationByResource = useMemo(
    () => githubNotificationsByResource(notifications),
    [notifications],
  );
  const unreadNotificationCount = notifications.filter((item) => item.unread).length;

  const enrichOverview = useCallback((next: GithubChannelOverview, requestEpoch: number) => {
    const requestKey = githubComparisonRequestKey(next);
    if (!requestKey) return;
    let request = githubComparisonRequests.get(requestKey);
    if (!request) {
      request = apiInvoke("load_github_review_comparisons", { channelId: channel.id })
        .finally(() => {
          if (githubComparisonRequests.get(requestKey) === request) {
            githubComparisonRequests.delete(requestKey);
          }
        });
      githubComparisonRequests.set(requestKey, request);
    }
    void request.then((comparisons) => {
      if (requestEpochRef.current !== requestEpoch) return;
      setOverview((current) => {
        if (!current) return current;
        const updated = mergeGithubReviewComparisons(current, comparisons);
        if (updated !== current) cacheGithubOverview(channel.id, updated);
        return updated;
      });
    }).catch(() => {
      // Counts are optional. Keep "New commits" and retry on the next load/refresh.
    });
  }, [channel.id]);

  const refreshOverview = useCallback(async (resource: GithubResource) => {
    const requestEpoch = requestEpochRef.current + 1;
    requestEpochRef.current = requestEpoch;
    setRefreshingResource(resource);
    setError(null);
    try {
      const next = await requestGithubRefresh(channel.id, resource);
      if (requestEpochRef.current !== requestEpoch) return;
      cacheGithubOverview(channel.id, next);
      setOverview(next);
      if (resource === "pulls") enrichOverview(next, requestEpoch);
    } catch (loadError) {
      if (requestEpochRef.current !== requestEpoch) return;
      setError(errorMessage(
        loadError,
        resource === "pulls"
          ? "Couldn’t update PRs"
          : "Couldn’t update issues",
      ));
    } finally {
      if (requestEpochRef.current === requestEpoch) setRefreshingResource(null);
    }
  }, [channel.id, enrichOverview]);

  const loadOverview = useCallback(async () => {
    const requestEpoch = requestEpochRef.current + 1;
    requestEpochRef.current = requestEpoch;
    const cached = githubOverviewCache.get(channel.id) ?? null;
    setOverview(cached);
    setLoading(!cached);
    setRefreshingResource(null);
    setError(null);
    try {
      const next = await apiInvoke("load_github_review_queue", {
        channelId: channel.id,
      });
      if (requestEpochRef.current !== requestEpoch) return;
      cacheGithubOverview(channel.id, next);
      setOverview(next);
      setLoading(false);
      if (activeResource === "notifications") {
        // The inbox keeps notifications current; queues refresh when shown.
      } else if (githubQueueNeedsRefresh(next, activeResource)) {
        void refreshOverview(activeResource);
      } else if (activeResource === "pulls") {
        enrichOverview(next, requestEpoch);
      }
    } catch (loadError) {
      if (requestEpochRef.current !== requestEpoch) return;
      if (!cached) setOverview(null);
      setError(errorMessage(loadError, "Couldn’t load PRs"));
      setLoading(false);
    }
  }, [activeResource, channel.id, enrichOverview, refreshOverview]);

  useEffect(() => {
    void loadOverview();
    return () => {
      requestEpochRef.current += 1;
    };
  }, [loadOverview]);

  useEffect(() => {
    const previous = attentionSnapshotRef.current;
    attentionSnapshotRef.current = {
      channelId: channel.id,
      count: channel.github_unread_count,
      syncedAt: channel.github_review_synced_at,
      issueSyncedAt: channel.github_issue_synced_at,
    };
    if (previous.channelId !== channel.id) return;
    const hasNewAttention = channel.github_unread_count > previous.count;
    // A queue refreshed elsewhere: by an agent, or in the background after
    // new activity in the GitHub inbox.
    const hasNewSync = Boolean(
      (channel.github_review_synced_at
        && channel.github_review_synced_at !== previous.syncedAt)
      || (channel.github_issue_synced_at
        && channel.github_issue_synced_at !== previous.issueSyncedAt),
    );
    if (!hasNewAttention && !hasNewSync) return;
    githubOverviewCache.delete(channel.id);
    void loadOverview();
  }, [
    channel.github_issue_synced_at,
    channel.github_review_synced_at,
    channel.github_unread_count,
    channel.id,
    loadOverview,
  ]);

  const focusThreadId = focusThread?.threadId ?? null;
  const focusNonce = focusThread?.nonce ?? 0;
  useEffect(() => {
    if (!focusThreadId) return;
    setActiveResource("notifications");
    setHighlightedThreadId(focusThreadId);
    pendingScrollThreadRef.current = focusThreadId;
    // Shown once: coming back to the tab later starts where the owner left it.
    onFocusThreadShown();
  }, [focusThreadId, focusNonce]);

  useEffect(() => {
    if (!highlightedThreadId) return;
    const timer = window.setTimeout(() => setHighlightedThreadId(null), 2400);
    return () => window.clearTimeout(timer);
  }, [highlightedThreadId]);

  useEffect(() => {
    const threadId = pendingScrollThreadRef.current;
    if (!threadId || activeResource !== "notifications") return;
    const row = Array.from(document.querySelectorAll<HTMLElement>(".github-notification-list [data-thread-id]"))
      .find((element) => element.dataset.threadId === threadId);
    if (!row) return;
    pendingScrollThreadRef.current = null;
    row.scrollIntoView({ block: "center" });
  });

  useEffect(() => {
    if (activeResource === "notifications" && !notificationsShown && overview) {
      setActiveResource("pulls");
    }
  }, [activeResource, notificationsShown, overview]);

  function openBindingModal() {
    setRepositoryDraft(overview?.binding?.name_with_owner ?? "");
    setLocalPathDraft(overview?.binding?.local_path ?? "");
    setAliasDraft(getGithubReferenceIndex().aliasByChannel.get(channel.id) ?? "");
    setReviewLoginDraft(overview?.binding?.review_login ?? overview?.account.login ?? "");
    setBindingError(null);
    setShowBindingModal(true);
  }

  async function saveBinding() {
    const repository = repositoryDraft.trim();
    if (!repository || bindingBusy) return;
    setBindingBusy(true);
    setBindingError(null);
    try {
      await apiInvoke("bind_github_repository", {
        channelId: channel.id,
        repository,
        localPath: localPathDraft.trim() || null,
        reviewLogin: reviewLoginDraft.trim() || null,
      });
      const alias = aliasDraft.trim();
      if (alias !== (getGithubReferenceIndex().aliasByChannel.get(channel.id) ?? "")) {
        setGithubReferences(await apiInvoke("set_github_reference_alias", { channelId: channel.id, alias }));
      } else {
        void loadGithubReferences();
      }
      setShowBindingModal(false);
      githubOverviewCache.delete(channel.id);
      await loadOverview();
    } catch (saveError) {
      setBindingError(errorMessage(saveError, "Couldn’t connect the repo"));
    } finally {
      setBindingBusy(false);
    }
  }

  function chooseReviewAgent(pullRequest: GithubPullRequest) {
    setReviewTarget({
      number: pullRequest.number,
      title: pullRequest.title,
      authorLogin: pullRequest.author_login,
    });
  }

  /** The notification about a pull request or issue, if the inbox has one. */
  function notificationFor(kind: "PullRequest" | "Issue", number: number) {
    return notificationByResource.get(githubNotificationKey(kind, number)) ?? null;
  }

  function readNotification(item: GithubInboxItem | null) {
    if (item?.unread) void markNotification(item, "read");
  }

  function openNotification(item: GithubInboxItem) {
    void openGithubUrl(item.url);
    readNotification(item);
  }

  async function markAllNotificationsRead() {
    if (!boundRepository || markingAllRead) return;
    setMarkingAllRead(true);
    setNotificationError(null);
    try {
      await onMarkRepositoryRead(boundRepository);
    } catch (markError) {
      setNotificationError(errorMessage(markError, "Couldn’t mark notifications read"));
    } finally {
      setMarkingAllRead(false);
    }
  }

  async function refreshInbox() {
    if (refreshingInbox) return;
    setRefreshingInbox(true);
    setNotificationError(null);
    try {
      await onRefreshInbox();
    } catch (refreshError) {
      setNotificationError(errorMessage(refreshError, "Couldn’t check GitHub"));
    } finally {
      setRefreshingInbox(false);
    }
  }

  async function rereviewPullRequest(pullRequest: GithubPullRequest) {
    if (rereviewingPullNumber !== null || !pullRequest.review_is_stale) return;
    setRereviewingPullNumber(pullRequest.number);
    setError(null);
    try {
      const result = await apiInvoke("rereview_github_pull_request", {
        channelId: channel.id,
        pullNumber: pullRequest.number,
      });
      setOverview((current) => {
        if (!current) return current;
        const next = {
          ...current,
          review_requests: current.review_requests.map((item) =>
            item.number === pullRequest.number
              ? {
                  ...item,
                  review_anchor_sha: result.head_sha,
                  review_is_stale: false,
                  review_commits_ahead: null,
                  linked_task_status: "in_progress",
                }
              : item
          ),
        };
        cacheGithubOverview(channel.id, next);
        return next;
      });
      onOpenThread(result.thread_root_id);
    } catch (rereviewError) {
      setError(errorMessage(rereviewError, "Couldn’t start the re-review"));
    } finally {
      setRereviewingPullNumber(null);
    }
  }

  async function loadIssueDetail(issue: GithubIssue) {
    const requestEpoch = detailRequestEpochRef.current + 1;
    detailRequestEpochRef.current = requestEpoch;
    setIssueDetail(null);
    setIssueDetailLoading(true);
    setIssueDetailError(null);
    try {
      const detail = await apiInvoke("load_github_issue_detail", {
        channelId: channel.id,
        issueNumber: issue.number,
      });
      if (detailRequestEpochRef.current !== requestEpoch) return;
      setIssueDetail(detail);
    } catch (detailError) {
      if (detailRequestEpochRef.current !== requestEpoch) return;
      setIssueDetailError(errorMessage(detailError, "Couldn’t load the issue"));
    } finally {
      if (detailRequestEpochRef.current === requestEpoch) setIssueDetailLoading(false);
    }
  }

  function openIssue(issue: GithubIssue) {
    setSelectedIssue(issue);
    void loadIssueDetail(issue);
  }

  function closeIssue() {
    detailRequestEpochRef.current += 1;
    setSelectedIssue(null);
    setIssueDetail(null);
    setIssueDetailLoading(false);
    setIssueDetailError(null);
  }

  async function openGithubUrl(url: string) {
    try {
      await openExternalUrl(url);
    } catch (openError) {
      setError(errorMessage(openError, "Couldn’t open GitHub"));
    }
  }

  const pullRequestCounts = useMemo(() => {
    const pullRequests = overview?.review_requests ?? [];
    return {
      review_requested: pullRequests.filter((pullRequest) => pullRequest.is_review_requested)
        .length,
      authored: pullRequests.filter((pullRequest) => pullRequest.is_authored).length,
      linked: pullRequests.filter((pullRequest) => Boolean(pullRequest.linked_thread_root_id))
        .length,
      all: pullRequests.length,
    };
  }, [overview]);

  const filteredPullRequests = useMemo(() => {
    const search = pullRequestSearch.trim().toLocaleLowerCase();
    return (overview?.review_requests ?? []).filter((pullRequest) => {
      const matchesFilter = pullRequestFilter === "review_requested"
        ? pullRequest.is_review_requested
        : pullRequestFilter === "authored"
          ? pullRequest.is_authored
          : pullRequestFilter === "linked"
            ? Boolean(pullRequest.linked_thread_root_id)
            : true;
      if (!matchesFilter) return false;
      if (!search) return true;
      return [
        pullRequest.number.toString(),
        `#${pullRequest.number}`,
        pullRequest.title,
        pullRequest.author_login,
      ]
        .join(" ")
        .toLocaleLowerCase()
        .includes(search);
    });
  }, [overview, pullRequestFilter, pullRequestSearch]);

  const issueCounts = useMemo(() => {
    const login = overview?.binding?.review_login ?? "";
    const issues = overview?.issues ?? [];
    return {
      related: issues.filter((issue) => issue.is_related).length,
      assigned: issues.filter((issue) => issueRelation(issue, login).assigned).length,
      authored: issues.filter((issue) => issueRelation(issue, login).authored).length,
      linked: issues.filter((issue) => Boolean(issue.linked_thread_root_id)).length,
      open: issues.length,
    };
  }, [overview]);

  const filteredIssues = useMemo(() => {
    const login = overview?.binding?.review_login ?? "";
    const search = issueSearch.trim().toLocaleLowerCase();
    return (overview?.issues ?? []).filter((issue) => {
      const relation = issueRelation(issue, login);
      const matchesFilter = issueFilter === "related"
        ? issue.is_related
        : issueFilter === "assigned"
          ? relation.assigned
          : issueFilter === "authored"
            ? relation.authored
            : issueFilter === "linked"
              ? Boolean(issue.linked_thread_root_id)
              : true;
      if (!matchesFilter) return false;
      if (!search) return true;
      const searchable = [
        issue.number.toString(),
        `#${issue.number}`,
        issue.title,
        issue.author_login,
        ...issue.assignee_logins,
        ...issue.labels.map((label) => label.name),
      ]
        .join(" ")
        .toLocaleLowerCase();
      return searchable.includes(search);
    });
  }, [issueFilter, issueSearch, overview]);

  if (loading && !overview) {
    return (
      <div className="github-panel github-panel-loading" aria-live="polite">
        <LoaderCircle className="spin" size={24} />
        <span>Loading GitHub review queue…</span>
      </div>
    );
  }

  if (error && !overview) {
    return (
      <div className="github-panel">
        <div className="empty-state github-empty-state" role="alert">
          <Github size={36} />
          <h2>Can’t reach GitHub</h2>
          <p>{error}</p>
          <button type="button" className="empty-state-action" onClick={() => void loadOverview()}>
            <RefreshCw size={15} />
            Retry
          </button>
        </div>
      </div>
    );
  }

  if (!overview) return null;

  const binding = overview.binding;
  const showingNotifications = activeResource === "notifications";
  const refreshing = showingNotifications ? refreshingInbox : refreshingResource === activeResource;
  const syncedAt = activeResource === "notifications"
    ? inbox?.synced_at ?? null
    : githubQueueSyncedAt(overview, activeResource);
  const refreshLabel = activeResource === "notifications"
    ? "notifications"
    : activeResource === "pulls" ? "pull requests" : "issues";
  const drawerIssue = selectedIssue
    ? overview.issues.find((issue) => issue.number === selectedIssue.number) ?? selectedIssue
    : null;
  return (
    <div className="github-panel">
      {binding ? (
        <>
          <header className="github-panel-toolbar">
            <div className="github-repository-summary">
              <button
                type="button"
                className="github-repository-link"
                onClick={() => void openGithubUrl(binding.url)}
                title="Open repository on GitHub"
              >
                <Github size={18} />
                <strong>{binding.name_with_owner}</strong>
                <ExternalLink size={14} />
              </button>
              <span>
                Personal queues for <b>@{binding.review_login}</b>
                {binding.local_path ? ` · ${binding.local_path}` : ""}
              </span>
              <span
                className="github-sync-status"
                title={syncedAt ? formattedUpdate(syncedAt) : undefined}
              >
                {refreshing && <LoaderCircle className="spin" size={12} />}
                {syncedAt
                  ? `${refreshing ? "Refreshing · " : ""}Last synced ${formatRelativeTime(
                      syncedAt,
                    )}`
                  : refreshing
                    ? `Syncing ${refreshLabel}…`
                    : "Not updated yet"}
              </span>
            </div>
            <div className="github-toolbar-actions">
              <button
                type="button"
                disabled={showingNotifications ? refreshingInbox : Boolean(refreshingResource)}
                onClick={() => {
                  if (activeResource === "notifications") void refreshInbox();
                  else void refreshOverview(activeResource);
                }}
                title={`Refresh ${refreshLabel}`}
              >
                <RefreshCw className={refreshing ? "spin" : ""} size={16} />
                Refresh
              </button>
              <button type="button" onClick={openBindingModal}>
                <Settings2 size={16} />
                Binding
              </button>
            </div>
          </header>

          <div className="github-resource-tabs" role="tablist" aria-label="GitHub work items">
            <button
              type="button"
              role="tab"
              aria-selected={activeResource === "pulls"}
              className={activeResource === "pulls" ? "active" : ""}
              onClick={() => setActiveResource("pulls")}
            >
              <GitPullRequest size={16} />
              Pull requests
              <mark>{pullRequestCounts.review_requested}</mark>
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={activeResource === "issues"}
              className={activeResource === "issues" ? "active" : ""}
              onClick={() => setActiveResource("issues")}
            >
              <CircleDot size={16} />
              Issues
              <mark>{issueCounts.related}</mark>
            </button>
            {notificationsShown && (
              <button
                type="button"
                role="tab"
                aria-selected={activeResource === "notifications"}
                className={[
                  activeResource === "notifications" ? "active" : "",
                  unreadNotificationCount > 0 ? "has-unread" : "",
                ].filter(Boolean).join(" ")}
                onClick={() => setActiveResource("notifications")}
              >
                <Bell size={16} />
                Notifications
                <mark aria-label={`${unreadNotificationCount} unread`}>{unreadNotificationCount}</mark>
              </button>
            )}
          </div>

          {activeResource === "notifications" ? (
            <section className="github-queue github-notifications" aria-label="GitHub notifications">
              <div className="github-issue-controls">
                <p className="github-notifications-summary">
                  Unread notifications here are the ones in your GitHub inbox; reading one in either
                  place reads it on GitHub.
                </p>
                <button
                  type="button"
                  className="github-notifications-read-all"
                  disabled={unreadNotificationCount === 0 || markingAllRead}
                  onClick={() => void markAllNotificationsRead()}
                >
                  {markingAllRead ? <LoaderCircle className="spin" size={15} /> : <MailOpen size={15} />}
                  Mark all read
                </button>
              </div>
              {(notificationError ?? inbox?.error) && (
                <div className="github-inline-error" role="alert">
                  {notificationError ?? inbox?.error}
                </div>
              )}
              {notifications.length === 0 ? (
                <div className="empty-state compact github-empty-state">
                  <Bell size={32} />
                  <h2>No notifications</h2>
                  <p>Review requests, mentions and replies on {binding.name_with_owner} show up here.</p>
                </div>
              ) : (
                <div className="github-notification-list">
                  {notifications.map((item) => {
                    const reviewable = item.subject_type === "PullRequest" && item.number !== null
                      && !item.thread_root_id && agents.length > 0;
                    return (
                      <GithubInboxRow
                        key={item.thread_id}
                        item={item}
                        references={references}
                        highlighted={highlightedThreadId === item.thread_id}
                        onOpen={openNotification}
                        onMark={(target, kind) => void markNotification(target, kind)}
                        onOpenThread={(target, threadRootId) => {
                          readNotification(target);
                          onOpenThread(threadRootId);
                        }}
                        onReview={reviewable
                          ? (target) => setReviewTarget({
                              number: target.number!,
                              title: target.title,
                              authorLogin: null,
                            })
                          : undefined}
                      />
                    );
                  })}
                </div>
              )}
            </section>
          ) : activeResource === "pulls" ? (
            <section className="github-queue" aria-label="GitHub pull requests">
              <div className="github-issue-controls">
                <div className="github-issue-filters" role="group" aria-label="Filter pull requests">
                  {([
                    ["review_requested", "Review requested"],
                    ["authored", "Authored"],
                    ["linked", "Linked"],
                    ["all", "All mine"],
                  ] as Array<[GithubPullRequestFilter, string]>).map(([filter, label]) => (
                    <button
                      type="button"
                      className={pullRequestFilter === filter ? "active" : ""}
                      aria-pressed={pullRequestFilter === filter}
                      onClick={() => setPullRequestFilter(filter)}
                      key={filter}
                    >
                      {label}
                      <span>{pullRequestCounts[filter]}</span>
                    </button>
                  ))}
                </div>
                <label className="github-issue-search">
                  <Search size={15} />
                  <input
                    value={pullRequestSearch}
                    onChange={(event) => setPullRequestSearch(event.target.value)}
                    placeholder="Search title or #"
                    aria-label="Search GitHub pull requests"
                  />
                </label>
              </div>

              {error && <div className="github-inline-error" role="alert">{error}</div>}
              {overview.review_requests.length === 0 &&
              refreshing &&
              !binding.review_queue_synced_at ? (
                <div className="empty-state compact github-empty-state" aria-live="polite">
                  <LoaderCircle className="spin" size={28} />
                  <h2>Loading review requests…</h2>
                  <p>Fetching your PRs for the first time…</p>
                </div>
              ) : overview.review_requests.length === 0 ? (
                <div className="empty-state compact github-empty-state">
                  <GitPullRequest size={32} />
                  <h2>No open PRs</h2>
                  <p>
                    No open pull requests are currently authored by or requesting review from @
                    {binding.review_login}.
                  </p>
                </div>
              ) : filteredPullRequests.length === 0 ? (
                <div className="empty-state compact github-empty-state">
                  <GitPullRequest size={32} />
                  <h2>No open PRs</h2>
                  <p>
                    No open pull requests match the {pullRequestFilter.replace(/_/g, " ")} filter
                    {pullRequestSearch.trim() ? " and current search" : ""}.
                  </p>
                </div>
              ) : (
                <div className="github-pull-list">
                  {filteredPullRequests.map((pullRequest) => {
                    const linked = Boolean(pullRequest.linked_thread_root_id);
                    const rereviewing = rereviewingPullNumber === pullRequest.number;
                    const checkLabel = checkSummaryLabel(pullRequest);
                    const notification = notificationFor("PullRequest", pullRequest.number);
                    const unread = Boolean(notification?.unread);
                    return (
                      <article
                        className={[
                          "github-pull-card",
                          linked ? "linked" : "",
                          pullRequest.review_is_stale ? "stale" : "",
                          unread ? "unread" : "",
                        ]
                          .filter(Boolean)
                          .join(" ")}
                        key={pullRequest.number}
                      >
                        <div className="github-pull-main">
                          <div className="github-pull-eyebrow">
                            <GitPullRequest
                              size={13}
                              className={pullRequest.is_draft ? "state-draft" : "state-open"}
                              aria-hidden="true"
                            />
                            <span>PR #{pullRequest.number}</span>
                            {unread && notification && (
                              <mark className="notification" title="Unread in your GitHub inbox">
                                {githubInboxReasonLabel(notification.reason)}
                              </mark>
                            )}
                            {pullRequest.is_review_requested &&
                              pullRequestFilter !== "review_requested" && (
                                <mark className="attention">Review requested</mark>
                              )}
                            {pullRequest.is_authored && pullRequestFilter !== "authored" && (
                              <mark>Authored</mark>
                            )}
                            {pullRequest.is_draft && <mark className="draft">Draft</mark>}
                            {linked && (
                              <mark
                                className="linked"
                                title={pullRequest.linked_assignee_name ?? undefined}
                              >
                                Task #{pullRequest.linked_task_number}
                                {pullRequest.linked_task_status
                                  ? ` · ${statusLabel(pullRequest.linked_task_status)}`
                                  : ""}
                              </mark>
                            )}
                            {pullRequest.review_is_stale && (
                              <mark
                                className="stale"
                                title={
                                  pullRequest.review_anchor_sha
                                    ? `Reviewed ${pullRequest.review_anchor_sha.slice(0, 7)}; current head ${pullRequest.head_sha.slice(0, 7)}`
                                    : undefined
                                }
                              >
                                {staleReviewLabel(pullRequest)}
                              </mark>
                            )}
                          </div>
                          <button
                            type="button"
                            className="github-pull-title"
                            onClick={() => {
                              void openGithubUrl(notification?.unread ? notification.url : pullRequest.url);
                              readNotification(notification);
                            }}
                          >
                            <span className="github-card-title-text">{pullRequest.title}</span>
                            <ExternalLink size={14} />
                          </button>
                          <p>
                            @{pullRequest.author_login}
                            <span aria-hidden="true"> · </span>
                            <span title={formattedUpdate(pullRequest.updated_at)}>
                              Updated {formatRelativeTime(pullRequest.updated_at)}
                            </span>
                          </p>
                          <div
                            className={`github-check-summary ${pullRequest.checks.status}`}
                            title={
                              pullRequest.checks.failing_checks.length > 0
                                ? pullRequest.checks.failing_checks.join(", ")
                                : checkLabel
                            }
                          >
                            {pullRequest.checks.status === "failure" ? (
                              <CircleX size={14} aria-hidden="true" />
                            ) : pullRequest.checks.status === "pending" ? (
                              <Clock3 size={14} aria-hidden="true" />
                            ) : pullRequest.checks.status === "success" ? (
                              <CircleCheck size={14} aria-hidden="true" />
                            ) : (
                              <CircleDot size={14} aria-hidden="true" />
                            )}
                            <span>{checkLabel}</span>
                          </div>
                        </div>
                        <div className="github-pull-actions">
                          {linked ? (
                            <>
                              {pullRequest.review_is_stale && (
                                <button
                                  type="button"
                                  className="github-primary-action github-rereview-action"
                                  disabled={rereviewingPullNumber !== null}
                                  onClick={() => void rereviewPullRequest(pullRequest)}
                                  aria-label={rereviewActionLabel(pullRequest)}
                                  title={`${rereviewActionLabel(
                                    pullRequest,
                                  )} in the existing review thread`}
                                >
                                  {rereviewing ? (
                                    <LoaderCircle className="spin" size={16} />
                                  ) : (
                                    <RefreshCw size={16} />
                                  )}
                                  {rereviewing ? "Starting…" : "Re-review"}
                                </button>
                              )}
                              <button
                                type="button"
                                className={
                                  pullRequest.review_is_stale
                                    ? "github-primary-action github-thread-action"
                                    : "github-primary-action"
                                }
                                onClick={() => {
                                  if (pullRequest.linked_thread_root_id) {
                                    onOpenThread(pullRequest.linked_thread_root_id);
                                  }
                                }}
                              >
                                Open thread
                              </button>
                            </>
                          ) : (
                            <button
                              type="button"
                              className="github-primary-action"
                              disabled={agents.length === 0}
                              onClick={() => chooseReviewAgent(pullRequest)}
                              title={
                                agents.length === 0
                                  ? "Add an agent to review this pull request"
                                  : undefined
                              }
                            >
                              <Bot size={16} />
                              Review with agent
                            </button>
                          )}
                        </div>
                      </article>
                    );
                  })}
                </div>
              )}
            </section>
          ) : (
            <section className="github-queue github-issue-queue" aria-label="GitHub issues">
              <div className="github-issue-controls">
                <div className="github-issue-filters" role="group" aria-label="Filter issues">
                  {([
                    ["related", "Related"],
                    ["assigned", "Assigned"],
                    ["authored", "Authored"],
                    ["linked", "Linked"],
                    ["open", "Open"],
                  ] as Array<[GithubIssueFilter, string]>).map(([filter, label]) => (
                    <button
                      type="button"
                      className={issueFilter === filter ? "active" : ""}
                      aria-pressed={issueFilter === filter}
                      onClick={() => setIssueFilter(filter)}
                      key={filter}
                    >
                      {label}
                      <span>{issueCounts[filter]}</span>
                    </button>
                  ))}
                </div>
                <label className="github-issue-search">
                  <Search size={15} />
                  <input
                    value={issueSearch}
                    onChange={(event) => setIssueSearch(event.target.value)}
                    placeholder="Search title, # or label"
                    aria-label="Search GitHub issues"
                  />
                </label>
              </div>

              {error && <div className="github-inline-error" role="alert">{error}</div>}
              {overview.issues.length === 0 &&
              refreshing &&
              !binding.issue_queue_synced_at ? (
                <div className="empty-state compact github-empty-state" aria-live="polite">
                  <LoaderCircle className="spin" size={28} />
                  <h2>Loading issues…</h2>
                  <p>Issues show up after the first update.</p>
                </div>
              ) : filteredIssues.length === 0 ? (
                <div className="empty-state compact github-empty-state">
                  <CircleDot size={32} />
                  <h2>{issueSearch.trim() ? "No matching issues" : "No open issues"}</h2>
                  <p>
                    {issueSearch.trim()
                      ? "Try another word or filter."
                      : `No open issues match the ${issueFilter} filter for @${binding.review_login}.`}
                  </p>
                </div>
              ) : (
                <div className="github-issue-list">
                  {filteredIssues.map((issue) => {
                    const linked = Boolean(issue.linked_thread_root_id);
                    const relation = issueRelation(issue, binding.review_login);
                    const notification = notificationFor("Issue", issue.number);
                    const unread = Boolean(notification?.unread);
                    return (
                      <article
                        className={[
                          "github-issue-card",
                          linked ? "linked" : "",
                          unread ? "unread" : "",
                        ].filter(Boolean).join(" ")}
                        key={issue.number}
                      >
                        <div className="github-issue-main">
                          <div className="github-issue-eyebrow">
                            <CircleDot size={13} className="state-open" aria-hidden="true" />
                            <span>Issue #{issue.number}</span>
                            {unread && notification && (
                              <mark className="notification" title="Unread in your GitHub inbox">
                                {githubInboxReasonLabel(notification.reason)}
                              </mark>
                            )}
                            {relation.assigned && issueFilter !== "assigned" && (
                              <mark className="attention">Assigned</mark>
                            )}
                            {relation.authored && issueFilter !== "authored" && (
                              <mark>Authored</mark>
                            )}
                            {relation.involved && <mark>Involved</mark>}
                            {linked && (
                              <mark
                                className="linked"
                                title={issue.linked_assignee_name ?? undefined}
                              >
                                Task #{issue.linked_task_number}
                                {issue.linked_task_status
                                  ? ` · ${statusLabel(issue.linked_task_status)}`
                                  : ""}
                              </mark>
                            )}
                          </div>
                          <div className="github-issue-title-row">
                            <button
                              type="button"
                              className="github-issue-title"
                              onClick={() => {
                                openIssue(issue);
                                readNotification(notification);
                              }}
                            >
                              <span className="github-card-title-text">{issue.title}</span>
                            </button>
                            <button
                              type="button"
                              className="github-issue-external"
                              onClick={() => {
                                void openGithubUrl(notification?.unread ? notification.url : issue.url);
                                readNotification(notification);
                              }}
                              aria-label={`Open issue #${issue.number} on GitHub`}
                              title="Open on GitHub"
                            >
                              <ExternalLink size={14} />
                            </button>
                          </div>
                          {issue.labels.length > 0 && (
                            <div className="github-issue-labels">
                              {issue.labels.slice(0, 3).map((label) => (
                                <span style={issueLabelStyle(label)} key={label.name}>
                                  {label.name}
                                </span>
                              ))}
                              {issue.labels.length > 3 && (
                                <span className="overflow">+{issue.labels.length - 3}</span>
                              )}
                            </div>
                          )}
                          <p>
                            @{issue.author_login}
                            <span aria-hidden="true"> · </span>
                            <MessageCircle size={13} />
                            {issue.comments_count}
                            <span aria-hidden="true"> · </span>
                            <span title={formattedUpdate(issue.updated_at)}>
                              Updated {formatRelativeTime(issue.updated_at)}
                            </span>
                          </p>
                        </div>
                        <div className="github-pull-actions">
                          {linked ? (
                            <button
                              type="button"
                              className="github-primary-action"
                              onClick={() => {
                                if (issue.linked_thread_root_id) {
                                  onOpenThread(issue.linked_thread_root_id);
                                }
                              }}
                            >
                              Open thread
                            </button>
                          ) : (
                            <button
                              type="button"
                              className="github-primary-action"
                              disabled={agents.length === 0}
                              onClick={() => openIssue(issue)}
                              title={
                                agents.length === 0
                                  ? "Add an agent to investigate this issue"
                                  : undefined
                              }
                            >
                              <Bot size={16} />
                              Investigate
                            </button>
                          )}
                        </div>
                      </article>
                    );
                  })}
                </div>
              )}
            </section>
          )}
        </>
      ) : (
        <div className="empty-state github-empty-state">
          <Github size={38} />
          <h2>Connect a repo</h2>
          <p>
            Signed in as @{overview.account.login}. See your PRs and issues here.
          </p>
          <button type="button" className="empty-state-action" onClick={openBindingModal}>
            <Github size={15} />
            Connect repo
          </button>
        </div>
      )}

      <GithubIssueDrawer
        channelId={channel.id}
        issue={drawerIssue}
        detail={issueDetail}
        loading={issueDetailLoading}
        error={issueDetailError}
        agents={agents}
        onClose={closeIssue}
        onOpenGithub={(url) => {
          void openGithubUrl(url);
        }}
        onRetry={() => {
          if (drawerIssue) void loadIssueDetail(drawerIssue);
        }}
        onCreateTask={onCreateIssueTask}
        onOpenThread={onOpenThread}
      />

      <Modal
        open={showBindingModal}
        title={binding ? "Repo settings" : "Connect a repo"}
        onClose={() => {
          if (!bindingBusy) setShowBindingModal(false);
        }}
        closeOnBackdrop={!bindingBusy}
        closeOnEscape={!bindingBusy}
        width={580}
      >
        <div className="modal-form">
          <label>
            <span>Repo</span>
            <input
              autoFocus
              value={repositoryDraft}
              onChange={(event) => setRepositoryDraft(event.target.value)}
              placeholder="owner/repo or link"
            />
          </label>
          <label>
            <span>Your GitHub username</span>
            <input
              value={reviewLoginDraft}
              onChange={(event) => setReviewLoginDraft(event.target.value)}
              placeholder={overview.account.login}
            />
          </label>
          <label>
            <span>Short name for references (optional)</span>
            <input
              value={aliasDraft}
              onChange={(event) => setAliasDraft(event.target.value)}
              placeholder={derivedReferenceAlias(repositoryDraft.split("/").pop() ?? "") || "e.g. rw"}
            />
          </label>
          <label>
            <span>Local folder (optional)</span>
            <input
              value={localPathDraft}
              onChange={(event) => setLocalPathDraft(event.target.value)}
              placeholder="/absolute/path/to/repository"
            />
          </label>
          <p className="github-modal-note">
            Authentication uses the active <code>gh</code> account @{overview.account.login}.
            This identity filters review requests and related issues. The local checkout is
            included in Agent tasks. Messages link <code>#123</code> in this channel, and
            <code>short-name#123</code> anywhere, to this repository.
          </p>
          {bindingError && <div className="github-inline-error" role="alert">{bindingError}</div>}
          <div className="modal-actions">
            <button type="button" disabled={bindingBusy} onClick={() => setShowBindingModal(false)}>
              Cancel
            </button>
            <button
              type="button"
              className="primary"
              disabled={!repositoryDraft.trim() || bindingBusy}
              onClick={() => void saveBinding()}
            >
              {bindingBusy ? "Connecting…" : binding ? "Save" : "Connect repo"}
            </button>
          </div>
        </div>
      </Modal>

      <GithubReviewDialog
        target={reviewTarget && binding
          ? { repository: binding.name_with_owner, ...reviewTarget }
          : null}
        agents={agents}
        onStart={(agentId) => onCreateReviewTask(reviewTarget!.number, agentId)}
        onClose={() => setReviewTarget(null)}
      />
    </div>
  );
}
