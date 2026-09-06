import {
  Bot,
  CircleCheck,
  CircleDot,
  CircleX,
  Clock3,
  ExternalLink,
  GitPullRequest,
  Github,
  LoaderCircle,
  MessageCircle,
  RefreshCw,
  Search,
  Settings2,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties } from "react";

import { apiInvoke, openExternalUrl } from "../apiClient";
import { githubComparisonRequestKey, mergeGithubReviewComparisons } from "../github-review";
import { formatRelativeTime } from "../ui-utils";
import type {
  Agent,
  Channel,
  GithubChannelOverview,
  GithubIssue,
  GithubIssueDetail,
  GithubIssueTaskResult,
  GithubLabel,
  GithubPullRequest,
  GithubReviewTaskResult,
  GithubReviewComparisons,
} from "../types";
import { GithubIssueDrawer } from "./GithubIssueDrawer";
import { Modal } from "./Modal";
import { TaskAssigneePicker } from "./TaskAssigneePicker";

type GithubPanelProps = {
  channel: Channel;
  agents: Agent[];
  onCreateReviewTask: (pullNumber: number, agentId: string) => Promise<GithubReviewTaskResult>;
  onCreateIssueTask: (issueNumber: number, agentId: string) => Promise<GithubIssueTaskResult>;
  onOpenThread: (threadRootId: string) => void;
};

type GithubResource = "pulls" | "issues";
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
  return "No checks reported";
}

function staleReviewLabel(pullRequest: GithubPullRequest) {
  const commits = pullRequest.review_commits_ahead;
  if (commits && commits > 0) {
    return `${commits} new ${commits === 1 ? "commit" : "commits"}`;
  }
  return "New head";
}

function rereviewActionLabel(pullRequest: GithubPullRequest) {
  const commits = pullRequest.review_commits_ahead;
  if (commits && commits > 0) {
    return `Re-review ${commits} ${commits === 1 ? "commit" : "commits"}`;
  }
  return "Re-review head";
}

export function GithubPanel({
  channel,
  agents,
  onCreateReviewTask,
  onCreateIssueTask,
  onOpenThread,
}: GithubPanelProps) {
  const [overview, setOverview] = useState<GithubChannelOverview | null>(
    () => githubOverviewCache.get(channel.id) ?? null,
  );
  const [loading, setLoading] = useState(() => !githubOverviewCache.has(channel.id));
  const [activeResource, setActiveResource] = useState<GithubResource>("pulls");
  const [refreshingResource, setRefreshingResource] = useState<GithubResource | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [showBindingModal, setShowBindingModal] = useState(false);
  const [repositoryDraft, setRepositoryDraft] = useState("");
  const [localPathDraft, setLocalPathDraft] = useState("");
  const [reviewLoginDraft, setReviewLoginDraft] = useState("");
  const [bindingBusy, setBindingBusy] = useState(false);
  const [bindingError, setBindingError] = useState<string | null>(null);
  const [reviewTarget, setReviewTarget] = useState<GithubPullRequest | null>(null);
  const [selectedAgentId, setSelectedAgentId] = useState("");
  const [reviewBusy, setReviewBusy] = useState(false);
  const [reviewError, setReviewError] = useState<string | null>(null);
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
  });

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
      // Counts are optional. Keep "New head" and retry on the next load/refresh.
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
          ? "Failed to refresh the GitHub review queue"
          : "Failed to refresh the GitHub issue queue",
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
      if (githubQueueNeedsRefresh(next, activeResource)) {
        void refreshOverview(activeResource);
      } else if (activeResource === "pulls") {
        enrichOverview(next, requestEpoch);
      }
    } catch (loadError) {
      if (requestEpochRef.current !== requestEpoch) return;
      if (!cached) setOverview(null);
      setError(errorMessage(loadError, "Failed to load the cached GitHub review queue"));
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
    };
    if (previous.channelId !== channel.id) return;
    const hasNewAttention = channel.github_unread_count > previous.count;
    const hasNewSync = Boolean(
      channel.github_review_synced_at
      && channel.github_review_synced_at !== previous.syncedAt,
    );
    if (!hasNewAttention && !hasNewSync) return;
    githubOverviewCache.delete(channel.id);
    void loadOverview();
  }, [
    channel.github_review_synced_at,
    channel.github_unread_count,
    channel.id,
    loadOverview,
  ]);

  function openBindingModal() {
    setRepositoryDraft(overview?.binding?.name_with_owner ?? "");
    setLocalPathDraft(overview?.binding?.local_path ?? "");
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
      setShowBindingModal(false);
      githubOverviewCache.delete(channel.id);
      await loadOverview();
    } catch (saveError) {
      setBindingError(errorMessage(saveError, "Failed to bind the GitHub repository"));
    } finally {
      setBindingBusy(false);
    }
  }

  function chooseReviewAgent(pullRequest: GithubPullRequest) {
    setReviewTarget(pullRequest);
    setSelectedAgentId(agents[0]?.id ?? "");
    setReviewError(null);
  }

  async function createReviewTask() {
    if (!reviewTarget || !selectedAgentId || reviewBusy) return;
    setReviewBusy(true);
    setReviewError(null);
    try {
      await onCreateReviewTask(reviewTarget.number, selectedAgentId);
      setReviewTarget(null);
    } catch (createError) {
      setReviewError(errorMessage(createError, "Failed to create the GitHub review task"));
    } finally {
      setReviewBusy(false);
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
      setError(errorMessage(rereviewError, "Failed to schedule the incremental review"));
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
      setIssueDetailError(errorMessage(detailError, "Failed to load the GitHub issue"));
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
      setError(errorMessage(openError, "Failed to open GitHub"));
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
          <h2>GitHub connection unavailable</h2>
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
  const selectedAgent = agents.find((agent) => agent.id === selectedAgentId) ?? null;
  const refreshing = refreshingResource === activeResource;
  const syncedAt = githubQueueSyncedAt(overview, activeResource);
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
                    ? `Syncing ${activeResource === "pulls" ? "pull requests" : "issues"}…`
                    : "Not synced yet"}
              </span>
            </div>
            <div className="github-toolbar-actions">
              <button
                type="button"
                disabled={Boolean(refreshingResource)}
                onClick={() => void refreshOverview(activeResource)}
                title={`Refresh ${activeResource === "pulls" ? "pull requests" : "issues"}`}
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
          </div>

          {activeResource === "pulls" ? (
            <section className="github-queue" aria-label="GitHub pull requests">
              <div className="github-issue-controls">
                <div className="github-issue-filters" role="group" aria-label="Filter pull requests">
                  {([
                    ["review_requested", "Review requested"],
                    ["authored", "Authored"],
                    ["linked", "Linked"],
                    ["all", "All personal"],
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
                  <p>The repository is ready while the first queue snapshot syncs.</p>
                </div>
              ) : overview.review_requests.length === 0 ? (
                <div className="empty-state compact github-empty-state">
                  <GitPullRequest size={32} />
                  <h2>Pull request queue is clear</h2>
                  <p>
                    No open pull requests are currently authored by or requesting review from @
                    {binding.review_login}.
                  </p>
                </div>
              ) : filteredPullRequests.length === 0 ? (
                <div className="empty-state compact github-empty-state">
                  <GitPullRequest size={32} />
                  <h2>This pull request queue is clear</h2>
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
                    return (
                      <article
                        className={[
                          "github-pull-card",
                          linked ? "linked" : "",
                          pullRequest.review_is_stale ? "stale" : "",
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
                            onClick={() => void openGithubUrl(pullRequest.url)}
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
                                  {rereviewing ? "Scheduling…" : "Re-review"}
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
                  <p>The cached queue will appear here after the first background sync.</p>
                </div>
              ) : filteredIssues.length === 0 ? (
                <div className="empty-state compact github-empty-state">
                  <CircleDot size={32} />
                  <h2>{issueSearch.trim() ? "No matching issues" : "This issue queue is clear"}</h2>
                  <p>
                    {issueSearch.trim()
                      ? "Try a different title, issue number, author, assignee, or label."
                      : `No open issues match the ${issueFilter} filter for @${binding.review_login}.`}
                  </p>
                </div>
              ) : (
                <div className="github-issue-list">
                  {filteredIssues.map((issue) => {
                    const linked = Boolean(issue.linked_thread_root_id);
                    const relation = issueRelation(issue, binding.review_login);
                    return (
                      <article
                        className={`github-issue-card ${linked ? "linked" : ""}`}
                        key={issue.number}
                      >
                        <div className="github-issue-main">
                          <div className="github-issue-eyebrow">
                            <CircleDot size={13} className="state-open" aria-hidden="true" />
                            <span>Issue #{issue.number}</span>
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
                              onClick={() => openIssue(issue)}
                            >
                              <span className="github-card-title-text">{issue.title}</span>
                            </button>
                            <button
                              type="button"
                              className="github-issue-external"
                              onClick={() => void openGithubUrl(issue.url)}
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
          <h2>Bind a GitHub repository</h2>
          <p>
            Signed in through GitHub CLI as @{overview.account.login}. Bind one repository to
            show pull requests and issues related to your work.
          </p>
          <button type="button" className="empty-state-action" onClick={openBindingModal}>
            <Github size={15} />
            Bind repository
          </button>
        </div>
      )}

      <GithubIssueDrawer
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
        title={binding ? "GitHub Repository Binding" : "Bind GitHub Repository"}
        onClose={() => {
          if (!bindingBusy) setShowBindingModal(false);
        }}
        closeOnBackdrop={!bindingBusy}
        closeOnEscape={!bindingBusy}
        width={580}
      >
        <div className="modal-form">
          <label>
            <span>Repository</span>
            <input
              autoFocus
              value={repositoryDraft}
              onChange={(event) => setRepositoryDraft(event.target.value)}
              placeholder="owner/repository or GitHub URL"
            />
          </label>
          <label>
            <span>GitHub identity for personal queues</span>
            <input
              value={reviewLoginDraft}
              onChange={(event) => setReviewLoginDraft(event.target.value)}
              placeholder={overview.account.login}
            />
          </label>
          <label>
            <span>Local checkout (optional)</span>
            <input
              value={localPathDraft}
              onChange={(event) => setLocalPathDraft(event.target.value)}
              placeholder="/absolute/path/to/repository"
            />
          </label>
          <p className="github-modal-note">
            Authentication uses the active <code>gh</code> account @{overview.account.login}.
            This identity filters review requests and related issues. The local checkout is
            included in Agent tasks.
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
              {bindingBusy ? "Binding…" : binding ? "Update binding" : "Bind repository"}
            </button>
          </div>
        </div>
      </Modal>

      <Modal
        open={Boolean(reviewTarget)}
        title={reviewTarget ? `Review ${binding?.name_with_owner}#${reviewTarget.number}` : "Review Pull Request"}
        onClose={() => {
          if (!reviewBusy) setReviewTarget(null);
        }}
        closeOnBackdrop={!reviewBusy}
        closeOnEscape={!reviewBusy}
        width={540}
      >
        {reviewTarget && (
          <div className="modal-form">
            <div className="github-review-target">
              <GitPullRequest size={20} />
              <div>
                <strong>{reviewTarget.title}</strong>
                <span>by @{reviewTarget.author_login}</span>
              </div>
            </div>
            <div className="modal-field github-agent-assignment">
              <span className="modal-field-label">Assign agent</span>
              <TaskAssigneePicker
                agents={agents}
                assignee={selectedAgent}
                allowUnassigned={false}
                ariaLabel={`Assign review agent for ${binding?.name_with_owner}#${reviewTarget.number}`}
                onChange={setSelectedAgentId}
              />
            </div>
            {selectedAgent && (
              <p className="github-modal-note">
                A Task and canonical thread will be created and dispatched to @{selectedAgent.handle}.
              </p>
            )}
            <p className="github-modal-note">
              The task is anchored to the latest PR head SHA. The Agent reports in Lantor and
              will not publish a GitHub review.
            </p>
            {reviewError && <div className="github-inline-error" role="alert">{reviewError}</div>}
            <div className="modal-actions">
              <button type="button" disabled={reviewBusy} onClick={() => setReviewTarget(null)}>
                Cancel
              </button>
              <button
                type="button"
                className="primary"
                disabled={!selectedAgentId || reviewBusy}
                onClick={() => void createReviewTask()}
              >
                {reviewBusy ? "Creating…" : "Create task and start review"}
              </button>
            </div>
          </div>
        )}
      </Modal>
    </div>
  );
}
