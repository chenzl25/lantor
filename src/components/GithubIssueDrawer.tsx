import { DialogSurface } from "./DialogSurface";
import {
  Bot,
  CircleDot,
  ExternalLink,
  LoaderCircle,
  MessageCircle,
  Milestone,
  RotateCcw,
  UserRound,
  X,
} from "lucide-react";
import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
} from "react";

import type {
  Agent,
  GithubIssue,
  GithubIssueDetail,
  GithubIssueTaskResult,
  GithubLabel,
} from "../types";
import { MessageMarkdown } from "./MessageMarkdown";
import { TaskAssigneePicker } from "./TaskAssigneePicker";

type GithubIssueDrawerProps = {
  issue: GithubIssue | null;
  detail: GithubIssueDetail | null;
  loading: boolean;
  error: string | null;
  agents: Agent[];
  onClose: () => void;
  onOpenGithub: (url: string) => void;
  onRetry: () => void;
  onCreateTask: (issueNumber: number, agentId: string) => Promise<GithubIssueTaskResult>;
  onOpenThread: (threadRootId: string) => void;
};

function formattedDate(value: string) {
  const date = new Date(value);
  if (!Number.isFinite(date.getTime())) return value;
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(date);
}

function labelStyle(label: GithubLabel): CSSProperties | undefined {
  const color = label.color.trim();
  if (!/^[0-9a-f]{6}$/i.test(color)) return undefined;
  return {
    backgroundColor: `#${color}1f`,
    borderColor: `#${color}66`,
  };
}

function errorMessage(error: unknown) {
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === "string" && error.trim()) return error;
  return "Failed to create the issue investigation task";
}

export function GithubIssueDrawer({
  issue,
  detail,
  loading,
  error,
  agents,
  onClose,
  onOpenGithub,
  onRetry,
  onCreateTask,
  onOpenThread,
}: GithubIssueDrawerProps) {
  const [selectedAgentId, setSelectedAgentId] = useState("");
  const [creating, setCreating] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);
  const lastIssueNumberRef = useRef<number | null>(null);

  useEffect(() => {
    const nextIssueNumber = issue?.number ?? null;
    const issueChanged = lastIssueNumberRef.current !== nextIssueNumber;
    lastIssueNumberRef.current = nextIssueNumber;
    setSelectedAgentId((currentAgentId) => {
      if (nextIssueNumber === null) return "";
      if (issueChanged || !agents.some((agent) => agent.id === currentAgentId)) {
        return agents[0]?.id ?? "";
      }
      return currentAgentId;
    });
    if (issueChanged) {
      setCreating(false);
      setCreateError(null);
    }
  }, [agents, issue?.number]);

  const selectedAgent = useMemo(
    () => agents.find((agent) => agent.id === selectedAgentId) ?? null,
    [agents, selectedAgentId],
  );

  if (!issue) return null;

  const issueNumber = issue.number;
  const title = detail?.title ?? issue.title;
  const labels = detail?.labels ?? issue.labels;
  const assigneeLogins = detail?.assignee_logins ?? issue.assignee_logins;
  const linked = Boolean(issue.linked_thread_root_id);

  async function createTask() {
    if (!selectedAgentId || creating) return;
    setCreating(true);
    setCreateError(null);
    try {
      await onCreateTask(issueNumber, selectedAgentId);
    } catch (taskError) {
      setCreateError(errorMessage(taskError));
      setCreating(false);
    }
  }

  return (
    <DialogSurface label={title} labelledBy="github-issue-drawer-title" backdropClassName="github-issue-drawer-backdrop"
      className="github-issue-drawer" onClose={onClose} closeOnBackdrop={!creating} closeOnEscape={!creating}>
        <header className="github-issue-drawer-head">
          <div>
            <span>
              <CircleDot size={14} />
              Issue #{issue.number}
            </span>
            <h2 id="github-issue-drawer-title">{title}</h2>
          </div>
          <div className="github-issue-drawer-head-actions">
            <button
              type="button"
              onClick={() => onOpenGithub(detail?.url ?? issue.url)}
              aria-label={`Open issue #${issue.number} on GitHub`}
              title="Open on GitHub"
            >
              <ExternalLink size={17} />
            </button>
            <button
              type="button"
              autoFocus
              disabled={creating}
              onClick={onClose}
              aria-label="Close issue details"
            >
              <X size={18} />
            </button>
          </div>
        </header>

        <div className="github-issue-drawer-body">
          <div className="github-issue-detail-meta">
            <span>
              <UserRound size={14} />
              Opened by @{detail?.author_login ?? issue.author_login}
            </span>
            <span>
              <MessageCircle size={14} />
              {issue.comments_count} {issue.comments_count === 1 ? "comment" : "comments"}
            </span>
            <span>Updated {formattedDate(detail?.updated_at ?? issue.updated_at)}</span>
          </div>

          {labels.length > 0 && (
            <div className="github-issue-labels" aria-label="Issue labels">
              {labels.map((label) => (
                <span style={labelStyle(label)} key={label.name}>
                  {label.name}
                </span>
              ))}
            </div>
          )}

          {assigneeLogins.length > 0 && (
            <div className="github-issue-detail-row">
              <strong>Assignees</strong>
              <span>{assigneeLogins.map((login) => `@${login}`).join(", ")}</span>
            </div>
          )}

          {detail?.milestone && (
            <div className="github-issue-detail-row">
              <strong>
                <Milestone size={14} />
                Milestone
              </strong>
              <span>{detail.milestone}</span>
            </div>
          )}

          {loading ? (
            <div className="github-issue-detail-loading" aria-live="polite">
              <LoaderCircle className="spin" size={22} />
              Loading issue details…
            </div>
          ) : error ? (
            <div className="github-issue-detail-error" role="alert">
              <p>{error}</p>
              <button type="button" onClick={onRetry}>
                <RotateCcw size={14} />
                Retry
              </button>
            </div>
          ) : detail ? (
            <section className="github-issue-description" aria-label="Issue description">
              <h3>Description</h3>
              {detail.body.trim() ? (
                <MessageMarkdown
                  body={detail.body}
                  enableLantorLinks={false}
                  scrollKey={`github-issue:${issue.number}`}
                />
              ) : (
                <p className="github-issue-no-description">No description provided.</p>
              )}
            </section>
          ) : null}
        </div>

        <footer className="github-issue-drawer-footer">
          {linked ? (
            <>
              <div className="github-linked-task">
                <strong>Task #{issue.linked_task_number}</strong>
                <span>
                  {(issue.linked_task_status ?? "").replace(/_/g, " ")}
                  {issue.linked_assignee_name ? ` · ${issue.linked_assignee_name}` : ""}
                </span>
              </div>
              <button
                type="button"
                className="github-primary-action"
                onClick={() => {
                  if (issue.linked_thread_root_id) onOpenThread(issue.linked_thread_root_id);
                }}
              >
                Open thread
              </button>
            </>
          ) : (
            <div className="github-issue-investigate">
              <div className="github-issue-investigate-copy">
                <Bot size={18} />
                <div>
                  <strong>Investigate with agent</strong>
                  <span>Creates a Task and canonical Lantor thread. No GitHub writes.</span>
                </div>
              </div>
              <TaskAssigneePicker
                agents={agents}
                assignee={selectedAgent}
                allowUnassigned={false}
                ariaLabel={`Assign investigation agent for issue #${issue.number}`}
                disabled={creating}
                onChange={setSelectedAgentId}
              />
              {createError && <div className="github-inline-error" role="alert">{createError}</div>}
              <button
                type="button"
                className="github-primary-action"
                disabled={!selectedAgentId || creating || loading || Boolean(error)}
                onClick={() => void createTask()}
              >
                {creating ? (
                  <>
                    <LoaderCircle className="spin" size={15} />
                    Starting…
                  </>
                ) : (
                  <>
                    <Bot size={15} />
                    Create task and investigate
                  </>
                )}
              </button>
            </div>
          )}
        </footer>
    </DialogSurface>
  );
}
