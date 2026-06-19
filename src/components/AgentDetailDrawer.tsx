import { ArrowLeft, ArrowRight, Pencil, RotateCcw, Trash2, X } from "lucide-react";
import { useEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { apiInvoke } from "../apiClient";
import { APP_DISPLAY_NAME } from "../branding";
import {
  Agent,
  AgentActivity,
  AgentRun,
  AgentWorkItem,
  AgentWorkspaceEntry,
  AgentWorkspaceFile,
  AgentWorkspaceListing,
  Channel,
  CLAUDE_REASONING_EFFORTS,
  CODEX_REASONING_EFFORTS,
  CODEX_SERVICE_TIERS,
  Message,
  Reminder,
  RUNTIME_PRESETS,
  modelLabel,
} from "../types";
import { AgentAvatar } from "./AgentAvatar";
import { MessageMarkdown } from "./MessageMarkdown";
import { Modal } from "./Modal";
import { sourceKindMeta } from "./ActivityProgressDock";
import { formatTime } from "../ui-utils";

type AgentPhase = {
  kind: string;
  label: string;
  detail: string;
};

type AgentDetailTab = "profile" | "reminders" | "activity" | "workspace";

const AGENT_DETAIL_TABS: Array<{ id: AgentDetailTab; label: string }> = [
  { id: "profile", label: "Profile" },
  { id: "reminders", label: "Reminders" },
  { id: "activity", label: "Activity" },
  { id: "workspace", label: "Workspace" },
];

const DEFAULT_ACTIVITY_RUN_COLLAPSED_STEP_COUNT = 3;
const LATEST_ACTIVITY_RUN_COLLAPSED_STEP_COUNT = 5;

type AgentDetailDrawerProps = {
  agent: Agent;
  activeRun: AgentRun | null;
  phase: AgentPhase | null;
  activities: AgentActivity[];
  performance: AgentPerformance;
  workItems: AgentWorkItem[];
  reminders: Reminder[];
  channels: Channel[];
  messages: Message[];
  onClose: () => void;
  onDelete: (agent: Agent) => void;
  onEdit: (agent: Agent) => void;
  onRestart: (agent: Agent) => void;
  onOpenWorkItem: (item: AgentWorkItem, focusedMessageIdOverride?: string | null) => void;
  onResizeStart: (event: ReactPointerEvent<HTMLButtonElement>) => void;
};

export type AgentPerformance = {
  windowLabel: string;
  turns: number;
  completedTurns: number;
  failedTurns: number;
  activeTurns: number;
  p50FirstTokenMs: number | null;
  p95FirstTokenMs: number | null;
  p50TurnMs: number | null;
  p95TurnMs: number | null;
  errorRate: number;
  inputTokens: number;
  outputTokens: number;
  costMicros: number;
};

const ACTIVITY_STATUS_LABELS: Record<string, string> = {
  active: "Active",
  success: "Done",
  warning: "Needs attention",
  error: "Error",
  info: "Info",
};

function statusForActivity(activity: AgentActivity) {
  return ACTIVITY_STATUS_LABELS[activity.status] ?? activity.status;
}

function userFacingActivityTitle(activity: AgentActivity) {
  const title = activity.summary || activity.title;
  const lowered = title.toLowerCase();
  if (lowered.includes("warm app-server ready") || lowered.includes("warm stream-json ready")) return "Runtime ready";
  if (
    (lowered.includes("warm turn") && lowered.includes("started")) ||
    lowered === "started working" ||
    lowered === "run started" ||
    lowered === "run created"
  ) {
    return "Working";
  }
  if (lowered.includes("running command")) return "Running command";
  if (lowered.includes("command finished")) return "Command finished";
  if (lowered.includes("editing file")) return "Editing file";
  if (lowered.includes("file edit finished")) return "File edit finished";
  if (lowered.includes("turn accepted")) return "Request acknowledged";
  if (lowered.includes("turn interrupt") || lowered.includes("stop signal")) return "Stop requested";
  if (lowered.includes("first token")) return "Responding";
  if (lowered.includes("content block finished")) return "Finished step";
  if (lowered.includes("message finished")) return "Finished response";
  if (lowered.includes("stream initialized")) return "Runtime ready";
  if (lowered.includes("system event") || lowered.includes("status")) return "Runtime status";
  if (lowered.includes("rate limit")) return "Checking rate limit";
  if (lowered.includes("follow-up steer")) return title.includes("rejected") ? "Follow-up queued" : "Follow-up added";
  if (lowered.startsWith("work item ") || lowered.startsWith("agent request ")) {
    if (lowered.includes("running")) return "Request started";
    if (lowered.includes("done")) return "Request completed";
    if (lowered.includes("failed")) return "Request failed";
    if (lowered.includes("cancelled")) return "Request cancelled";
  }
  return title;
}

const INTERNAL_ACTIVITY_DETAIL_KEYS = new Set([
  "pid",
  "thread_id",
  "session_id",
  "request_id",
  "run_id",
  "reference_id",
  "uuid",
]);

function keyValueActivityDetail(detail: string) {
  const parts = detail.split(/[,\n]/).map((part) => part.trim()).filter(Boolean);
  if (parts.length === 0) return null;
  const entries: Array<[string, string]> = [];
  for (const part of parts) {
    const separator = part.indexOf("=");
    if (separator <= 0) return null;
    const key = part.slice(0, separator).trim();
    const value = part.slice(separator + 1).trim();
    if (!key || !value) return null;
    entries.push([key, value]);
  }
  return entries
    .filter(([key]) => !INTERNAL_ACTIVITY_DETAIL_KEYS.has(key))
    .map(([key, value]) => `${key.replace(/_/g, " ")} ${value}`)
    .join(", ");
}

function userFacingActivityCategory(activity: AgentActivity) {
  if (activity.status === "error") return "Error";
  switch (activity.phase || activity.kind) {
    case "thinking":
      return "Thinking";
    case "command":
      return "Command";
    case "file_edit":
      return "File edit";
    case "tools":
      return "Tool";
    case "acting":
      return "Response";
    case "work":
      return "Request";
    case "runtime":
      return "Runtime";
    case "profile":
      return "Profile";
    case "usage":
      return "Usage";
    case "memory":
      return "Memory";
    case "channel":
    case "membership":
      return "Collaboration";
    default:
      return "Working";
  }
}

function isStructuredActivityDetail(detail: string) {
  const trimmed = detail.trim();
  if (!trimmed.startsWith("{") && !trimmed.startsWith("[")) return false;
  try {
    const parsed = JSON.parse(trimmed);
    return parsed !== null && typeof parsed === "object";
  } catch {
    return false;
  }
}

function userFacingActivityDetail(activity: AgentActivity) {
  const detail = activity.detail.trim();
  if (!detail) return "";
  if (isStructuredActivityDetail(detail)) return "";
  if (/^[0-9a-f]{8}-[0-9a-f-]{27,}$/i.test(detail)) return "";
  if (["content_block_stop", "message_stop", "pid unavailable", "status"].includes(detail)) return "";
  const keyValueDetail = keyValueActivityDetail(detail);
  if (keyValueDetail !== null) return keyValueDetail;
  return detail
    .replace(/^codex warm turn failed:/i, "Failed:")
    .replace(/^claude warm turn failed:/i, "Failed:")
    .replace(/^duration=/i, "duration ");
}

function phaseClass(kind: string) {
  return `phase-${kind.replace(/[^a-z0-9_-]/gi, "-")}`;
}

function stringifyMetadata(value: unknown) {
  if (value === null || value === undefined) return "";
  if (typeof value === "string" || typeof value === "number" || typeof value === "boolean") {
    return String(value);
  }
  return JSON.stringify(value);
}

function compactValue(value: string, maxLength = 54) {
  const normalized = value.replace(/\s+/g, " ").trim();
  if (normalized.length <= maxLength) return normalized;
  return `${normalized.slice(0, maxLength - 1)}…`;
}

function metadataEntries(activity: AgentActivity) {
  return Object.entries(activity.metadata ?? {})
    .filter(([key]) => !["detail", "reference_id", "run_id", "thread_id"].includes(key))
    .map(([key, value]) => [key, stringifyMetadata(value)] as const)
    .filter(([, value]) => value.length > 0);
}

function visibleMetadataEntries(activity: AgentActivity) {
  const priority = ["command", "file", "operation", "tool", "duration_ms", "exit_code", "status", "type", "reason"];
  const entries = metadataEntries(activity)
    .filter(([key]) => !["rate_limit_info", "uuid", "pid", "session_id", "request_id"].includes(key));

  return entries
    .sort(([left], [right]) => {
      const leftIndex = priority.indexOf(left);
      const rightIndex = priority.indexOf(right);
      if (leftIndex === -1 && rightIndex === -1) return 0;
      if (leftIndex === -1) return 1;
      if (rightIndex === -1) return -1;
      return leftIndex - rightIndex;
    })
    .slice(0, 3);
}

function formatActivityTime(value: string) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return formatTime(value);
  return date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

function formatReminderTime(value: string) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return formatTime(value);
  return date.toLocaleString([], {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function reminderStatusLabel(status: string) {
  if (status === "scheduled") return "Scheduled";
  if (status === "fired") return "Due";
  if (status === "completed") return "Done";
  if (status === "cancelled") return "Cancelled";
  return status;
}

function workItemStatusLabel(status: string) {
  if (status === "queued") return "Queued";
  if (status === "running") return "Running";
  if (status === "cancelling") return "Cancelling";
  if (status === "done") return "Run done";
  if (status === "failed") return "Failed";
  if (status === "cancelled") return "Cancelled";
  return status.replace(/_/g, " ");
}

function runStatusLabel(status: string) {
  if (status === "starting") return "Starting";
  if (status === "running") return "Running";
  if (status === "stopping") return "Stopping";
  if (status === "completed") return "Completed";
  if (status === "failed") return "Failed";
  if (status === "cancelled") return "Cancelled";
  return status.replace(/_/g, " ");
}

function workItemLocationLabel(item: AgentWorkItem) {
  if (item.channel_name) return `#${item.channel_name}`;
  if (item.channel_id) return "Direct message";
  return "Agent inbox";
}

function turnDurationLabel(activities: AgentActivity[]) {
  if (activities.length === 0) return "";
  const stamps = activities
    .map((activity) => activityTimestamp(activity))
    .filter((value) => value > 0);
  if (stamps.length < 2) return "";
  const spread = Math.max(...stamps) - Math.min(...stamps);
  if (spread <= 0) return "";
  if (spread < 1000) return `${spread} ms`;
  if (spread < 60_000) return `${(spread / 1000).toFixed(spread < 10_000 ? 1 : 0)} s`;
  const minutes = Math.round(spread / 60_000);
  return `${minutes} min`;
}

function compactPreview(value: string, maxLength = 80) {
  const normalized = value.replace(/\s+/g, " ").trim();
  if (normalized.length <= maxLength) return normalized;
  return `${normalized.slice(0, maxLength - 1)}…`;
}

function firstLinePreview(value: string, maxLength = 80) {
  const stripped = value
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean)[0] ?? "";
  return compactPreview(stripped, maxLength);
}

function rootMessageTitle(message: Message | null) {
  if (!message) return "";
  if (message.is_task && message.task_number) {
    return `Task #${message.task_number}: ${firstLinePreview(message.body, 60)}`;
  }
  return firstLinePreview(message.body, 60);
}

function channelDisplayLabel(channel: Channel | null) {
  if (!channel) return "Agent inbox";
  if (channel.kind === "dm") return "Direct message";
  return `#${channel.name}`;
}

function workItemSurfaceKey(item: AgentWorkItem) {
  return `${item.channel_id ?? "agent"}:${item.thread_root_id ?? "root"}`;
}

function isProviderRetryActivity(activity: AgentActivity) {
  return activity.kind === "run_retry" || activity.phase === "run_retry";
}

function activityTimestamp(activity: AgentActivity) {
  const value = new Date(activity.created_at).getTime();
  return Number.isFinite(value) ? value : 0;
}

function formatDuration(value: number | null) {
  if (value === null || Number.isNaN(value)) return "n/a";
  if (value < 1000) return `${Math.round(value)} ms`;
  return `${(value / 1000).toFixed(value < 10_000 ? 1 : 0)} s`;
}

function formatTokenCount(value: number) {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
  if (value >= 1_000) return `${(value / 1_000).toFixed(1)}k`;
  return String(value);
}

function formatCost(value: number) {
  return `$${(value / 1_000_000).toFixed(value > 10_000 ? 2 : 4)}`;
}

function compactPath(value: string) {
  if (!value) return "";
  return value.replace(/^\/Users\/[^/]+/, "~");
}

function formatEntrySize(value: number | null) {
  if (value === null) return "";
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)} MB`;
  if (value >= 1_000) return `${(value / 1_000).toFixed(1)} KB`;
  return `${value} B`;
}

function workspaceKindLabel(kind: string) {
  if (kind === "dir") return "DIR";
  if (kind === "file") return "FILE";
  return "ITEM";
}

function workspaceEntryPath(entry: AgentWorkspaceEntry) {
  return entry.relative_path || entry.name;
}

function runtimeLabel(runtime: string) {
  return RUNTIME_PRESETS[runtime]?.label ?? runtime;
}

function codexReasoningEffortLabel(value: string) {
  return CODEX_REASONING_EFFORTS.find((effort) => effort.value === value)?.label ?? (value || "Medium");
}

function codexServiceTierLabel(value: string) {
  return CODEX_SERVICE_TIERS.find((tier) => tier.value === value)?.label ?? (value || "Standard");
}

function claudeEffortLabel(value: string) {
  return CLAUDE_REASONING_EFFORTS.find((effort) => effort.value === value)?.label ?? (value || "Default");
}

function agentModelSummary(agent: Agent) {
  const base = `${runtimeLabel(agent.runtime)} · ${modelLabel(agent.model)}`;
  if (agent.runtime === "codex") {
    return `${base} · ${codexReasoningEffortLabel(agent.reasoning_effort)} intelligence · ${codexServiceTierLabel(agent.service_tier)} speed`;
  }
  if (agent.runtime === "claude" && agent.reasoning_effort.trim()) {
    return `${base} · ${claudeEffortLabel(agent.reasoning_effort)} effort`;
  }
  return base;
}

function isMarkdownWorkspaceFile(file: AgentWorkspaceFile) {
  return file.language === "markdown" || /\.(md|markdown)$/i.test(file.name);
}

export function AgentDetailDrawer({
  agent,
  activeRun,
  phase,
  activities,
  performance,
  workItems,
  reminders,
  channels,
  messages,
  onClose,
  onDelete,
  onEdit,
  onRestart,
  onOpenWorkItem,
  onResizeStart,
}: AgentDetailDrawerProps) {
  const [expandedActivityId, setExpandedActivityId] = useState<string | null>(null);
  const [expandedActivityRuns, setExpandedActivityRuns] = useState<Set<string>>(new Set());
  const [pendingCollapsedRunScrollId, setPendingCollapsedRunScrollId] = useState<string | null>(null);
  const [expandedWorkspaceDirs, setExpandedWorkspaceDirs] = useState<Set<string>>(new Set());
  const [workspaceNodes, setWorkspaceNodes] = useState<Record<string, AgentWorkspaceEntry[]>>({});
  const [workspaceLoadingPath, setWorkspaceLoadingPath] = useState<string | null>(null);
  const [workspacePreview, setWorkspacePreview] = useState<AgentWorkspaceFile | null>(null);
  const [workspaceError, setWorkspaceError] = useState<string | null>(null);
  const [activeDetailTab, setActiveDetailTab] = useState<AgentDetailTab>("activity");
  const activityRunRefs = useRef(new Map<string, HTMLElement>());
  const deleteDisabled = Boolean(activeRun);
  const agentStatus = agent.status.toLowerCase();
  const restartDisabledReason = activeRun
    ? "Stop the active run before restarting this agent"
    : agentStatus === "queued" || agentStatus === "starting"
      ? "Agent is already starting"
      : agentStatus === "running"
        ? "Agent is already running"
        : agentStatus === "stopping"
          ? "Wait for the agent to stop before restarting"
          : agentStatus === "deleted"
            ? "Deleted agents cannot be restarted"
            : "";
  const restartDisabled = Boolean(restartDisabledReason);
  const restartLabel = agentStatus === "error" ? `Restart @${agent.handle}` : `Start @${agent.handle}`;
  const workspacePath = agent.working_directory.trim();
  const rootWorkspaceEntries = workspaceNodes[""] ?? agent.workspace_entries ?? [];
  const memoryPath = agent.workspace_memory_path || (workspacePath ? `${workspacePath}/MEMORY.md` : "");
  const agentReminders = reminders
    .filter((reminder) => reminder.creator_agent_id === agent.id)
    .sort((left, right) => new Date(left.due_at).getTime() - new Date(right.due_at).getTime());
  const liveAgentReminders = agentReminders.filter((reminder) => !["completed", "cancelled"].includes(reminder.status));
  const dueAgentReminders = liveAgentReminders.filter((reminder) => reminder.status === "fired");
  const visibleAgentReminders = liveAgentReminders.slice(0, 8);

  useEffect(() => {
    setActiveDetailTab("activity");
    setExpandedWorkspaceDirs(new Set());
    setWorkspaceNodes({ "": agent.workspace_entries ?? [] });
    setWorkspaceLoadingPath(null);
    setWorkspacePreview(null);
    setWorkspaceError(null);
  }, [agent.id]);

  useEffect(() => {
    if (!pendingCollapsedRunScrollId) return;
    const frameId = window.requestAnimationFrame(() => {
      activityRunRefs.current.get(pendingCollapsedRunScrollId)?.scrollIntoView({
        block: "start",
        behavior: "smooth",
      });
      setPendingCollapsedRunScrollId(null);
    });
    return () => window.cancelAnimationFrame(frameId);
  }, [expandedActivityRuns, pendingCollapsedRunScrollId]);

  async function toggleWorkspaceDir(entry: AgentWorkspaceEntry) {
    const path = workspaceEntryPath(entry);
    setWorkspaceError(null);
    if (expandedWorkspaceDirs.has(path)) {
      setExpandedWorkspaceDirs((current) => {
        const next = new Set(current);
        next.delete(path);
        return next;
      });
      return;
    }

    setExpandedWorkspaceDirs((current) => new Set(current).add(path));
    if (workspaceNodes[path]) return;

    setWorkspaceLoadingPath(path);
    try {
      const listing = await apiInvoke<AgentWorkspaceListing>("agent_workspace_list", {
        agentId: agent.id,
        path,
      });
      setWorkspaceNodes((current) => ({ ...current, [path]: listing.entries }));
    } catch (err) {
      setWorkspaceError(String(err));
    } finally {
      setWorkspaceLoadingPath(null);
    }
  }

  async function openWorkspaceFile(entry: AgentWorkspaceEntry) {
    const path = workspaceEntryPath(entry);
    setWorkspaceError(null);
    setWorkspaceLoadingPath(path);
    try {
      const file = await apiInvoke<AgentWorkspaceFile>("agent_workspace_read_file", {
        agentId: agent.id,
        path,
      });
      setWorkspacePreview(file);
    } catch (err) {
      setWorkspaceError(String(err));
    } finally {
      setWorkspaceLoadingPath(null);
    }
  }

  function renderWorkspaceEntries(entries: AgentWorkspaceEntry[], depth = 0) {
    return entries.map((entry) => {
      const entryPath = workspaceEntryPath(entry);
      const isDir = entry.kind === "dir";
      const expanded = expandedWorkspaceDirs.has(entryPath);
      const children = workspaceNodes[entryPath] ?? [];
      const loading = workspaceLoadingPath === entryPath;
      return (
        <div key={entry.path} className="workspace-tree-node">
          <button
            type="button"
            className={`workspace-tree-row ${isDir ? "is-dir" : "is-file"} ${workspacePreview?.relative_path === entryPath ? "selected" : ""}`}
            style={{ paddingLeft: 10 + depth * 16 }}
            onClick={() => {
              if (isDir) {
                void toggleWorkspaceDir(entry);
              } else {
                void openWorkspaceFile(entry);
              }
            }}
          >
            <span className="workspace-disclosure" aria-hidden="true">
              {isDir ? expanded ? "v" : ">" : ""}
            </span>
            <span className={`workspace-entry-kind kind-${entry.kind}`}>
              {isDir ? "DIR" : workspaceKindLabel(entry.kind)}
            </span>
            <div className="workspace-entry-main">
              <strong title={entry.path}>{entry.name}</strong>
              <small>{isDir ? compactPath(entry.path) : formatEntrySize(entry.size_bytes)}</small>
            </div>
            <span className="workspace-row-action">
              {loading ? "Loading" : isDir ? expanded ? "Collapse" : "Expand" : "Preview"}
            </span>
          </button>
          {isDir && expanded && (
            <div className="workspace-tree-children">
              {children.length > 0
                ? renderWorkspaceEntries(children, depth + 1)
                : !loading && <p className="workspace-empty-folder">Empty folder</p>}
            </div>
          )}
        </div>
      );
    });
  }

  function tabBadge(tab: AgentDetailTab) {
    if (tab === "profile") return agent.status;
    if (tab === "reminders") {
      if (dueAgentReminders.length > 0) return `${dueAgentReminders.length} due`;
      if (liveAgentReminders.length > 0) return `${liveAgentReminders.length} reminders`;
      return "None";
    }
    if (tab === "activity") {
      if (activeRun) return "Live";
      return activities.length > 0 ? String(activities.length) : "Idle";
    }
    return workspacePath ? agent.workspace_exists ? "Ready" : "Missing" : "Unset";
  }

  function renderProfilePanel() {
    const isCodex = agent.runtime === "codex";
    const isClaude = agent.runtime === "claude";
    return (
      <>
        <section className="detail-section model-section">
          <div className="detail-section-head">
            <h4>Model</h4>
            <span>{runtimeLabel(agent.runtime)}</span>
          </div>
          <div className="detail-grid">
            <div>
              <span>Runtime</span>
              <code>{runtimeLabel(agent.runtime)}</code>
            </div>
            <div>
              <span>Model</span>
              <code>{modelLabel(agent.model)}</code>
            </div>
            {isCodex && (
              <>
                <div>
                  <span>Intelligence</span>
                  <code>{codexReasoningEffortLabel(agent.reasoning_effort)}</code>
                </div>
                <div>
                  <span>Speed</span>
                  <code>{codexServiceTierLabel(agent.service_tier)}</code>
                </div>
              </>
            )}
            {isClaude && (
              <div>
                <span>Effort</span>
                <code>{claudeEffortLabel(agent.reasoning_effort)}</code>
              </div>
            )}
          </div>
        </section>
        <section className="detail-grid">
          <div>
            <span>Workspace</span>
            <code>{workspacePath ? agent.workspace_exists ? "Available" : "Missing" : "Not configured"}</code>
          </div>
          <div>
            <span>Active run</span>
            <code>{activeRun ? runStatusLabel(activeRun.status) : "No active run"}</code>
          </div>
          <div>
            <span>Role</span>
            <code>{agent.role || "agent"}</code>
          </div>
          <div>
            <span>Description</span>
            <code>{agent.description || "No notes"}</code>
          </div>
        </section>
        <section className="detail-section performance-section">
          <div className="detail-section-head">
            <h4>Performance</h4>
            <span>{performance.windowLabel}</span>
          </div>
          <div className="performance-grid">
            <div className="performance-card">
              <span>Turns</span>
              <strong>{performance.turns}</strong>
              <small>{performance.completedTurns} done · {performance.failedTurns} failed</small>
            </div>
            <div className="performance-card">
              <span>First token</span>
              <strong>{formatDuration(performance.p50FirstTokenMs)}</strong>
              <small>p95 {formatDuration(performance.p95FirstTokenMs)}</small>
            </div>
            <div className="performance-card">
              <span>Turn duration</span>
              <strong>{formatDuration(performance.p50TurnMs)}</strong>
              <small>p95 {formatDuration(performance.p95TurnMs)}</small>
            </div>
            <div className="performance-card">
              <span>Error rate</span>
              <strong>{Math.round(performance.errorRate * 100)}%</strong>
              <small>{performance.activeTurns} currently active</small>
            </div>
            <div className="performance-card">
              <span>Tokens</span>
              <strong>{formatTokenCount(performance.inputTokens + performance.outputTokens)}</strong>
              <small>{formatTokenCount(performance.inputTokens)} in · {formatTokenCount(performance.outputTokens)} out</small>
            </div>
            <div className="performance-card">
              <span>Cost est.</span>
              <strong>{formatCost(performance.costMicros)}</strong>
              <small>
                {agent.daily_budget_micros > 0
                  ? `${formatCost(agent.daily_budget_micros)} daily budget`
                  : "No daily cap"}
              </small>
            </div>
          </div>
        </section>
      </>
    );
  }

  function renderRemindersPanel() {
    return (
      <>
        <section className="detail-section agent-autonomy-card">
          <div>
            <h4>Agent-managed routines</h4>
            <p>
              Reminders are created from conversation intent by the agent through {APP_DISPLAY_NAME} APIs.
              They are shown here as read-only agent state instead of a manual user reminder form.
            </p>
          </div>
        </section>
        <section className="detail-section agent-reminders-section">
          <div className="detail-section-head">
            <h4>Reminders</h4>
            <span>
              {dueAgentReminders.length > 0
                ? `${dueAgentReminders.length} due`
                : liveAgentReminders.length > 0
                  ? `${liveAgentReminders.length} active`
                  : "None"}
            </span>
          </div>
          {liveAgentReminders.length === 0 ? (
            <p className="empty-mini">No active reminders created by @{agent.handle}.</p>
          ) : (
            <div className="agent-reminder-list" aria-label={`${agent.handle} reminders`}>
              {visibleAgentReminders.map((reminder) => (
                <article
                  key={reminder.id}
                  className={`agent-reminder-row status-${reminder.status}`}
                >
                  <div className="agent-reminder-icon" aria-hidden="true">
                    {reminder.status === "fired" ? "!" : reminder.recurrence !== "none" ? "R" : "."}
                  </div>
                  <div className="agent-reminder-body">
                    <div className="agent-reminder-title">
                      <strong>{reminder.title}</strong>
                      <span>{reminderStatusLabel(reminder.status)}</span>
                    </div>
                    {reminder.note && <p>{reminder.note}</p>}
                    <small>
                      {formatReminderTime(reminder.due_at)}
                      {reminder.recurrence !== "none" ? ` · ${reminder.recurrence}` : ""}
                      {reminder.channel_name ? ` · #${reminder.channel_name}` : ""}
                    </small>
                  </div>
                </article>
              ))}
              {liveAgentReminders.length > visibleAgentReminders.length && (
                <p className="agent-reminder-overflow">Showing {visibleAgentReminders.length} of {liveAgentReminders.length} active reminders.</p>
              )}
            </div>
          )}
        </section>
      </>
    );
  }

  function renderWorkspacePanel() {
    return (
      <section className="detail-section workspace-section">
        <div className="detail-section-head">
          <h4>Workspace</h4>
          <span>{workspacePath ? agent.workspace_exists ? "Ready" : "Missing" : "Not configured"}</span>
        </div>
        <div className="workspace-meta-grid">
          <div className="workspace-path-card">
            <div>
              <span>Path</span>
              <code title={workspacePath}>{workspacePath ? compactPath(workspacePath) : "Not configured"}</code>
            </div>
          </div>
          <div className={`workspace-memory-card ${agent.workspace_memory_exists ? "ready" : "missing"}`}>
            <div>
              <span>MEMORY.md</span>
              <code title={memoryPath}>{memoryPath ? compactPath(memoryPath) : "Not configured"}</code>
            </div>
          </div>
        </div>
        <div className="workspace-browser">
          <div className="workspace-tree-pane">
            <div className="workspace-pane-head">
              <h5>Files</h5>
              <span>{rootWorkspaceEntries.length} items</span>
            </div>
            {rootWorkspaceEntries.length > 0 ? (
              <div className="workspace-tree" aria-label={`${agent.handle} workspace files`}>
                {renderWorkspaceEntries(rootWorkspaceEntries)}
              </div>
            ) : (
              <p className="empty-mini">
                {workspacePath
                  ? agent.workspace_exists ? "No visible workspace files yet." : "Workspace directory does not exist yet."
                  : "Workspace not configured."}
              </p>
            )}
          </div>
        </div>
        {workspaceError && <p className="workspace-error">{workspaceError}</p>}
      </section>
    );
  }

  function renderActivityPanel() {
    const channelsById = new Map(channels.map((channel) => [channel.id, channel] as const));
    const messagesById = new Map(messages.map((message) => [message.id, message] as const));
    const workItemsByRun = new Map(
      workItems
        .filter((item) => item.run_id)
        .map((item) => [item.run_id as string, item]),
    );
    const groupedActivities = new Map<string, AgentActivity[]>();
    activities.forEach((activity) => {
      const key = activity.run_id ?? "system-runtime";
      groupedActivities.set(key, [...(groupedActivities.get(key) ?? []), activity]);
    });
    const activityGroups = Array.from(groupedActivities.entries())
      .map(([runId, groupActivities]) => {
        const sorted = [...groupActivities].sort((left, right) => activityTimestamp(right) - activityTimestamp(left));
        const workItem = runId === "system-runtime" ? null : workItemsByRun.get(runId) ?? null;
        const latest = sorted[0];
        const queuedBehind = workItem
          ? workItems.filter((item) =>
            item.id !== workItem.id
            && item.status === "queued"
            && item.agent_id === workItem.agent_id
            && workItemSurfaceKey(item) === workItemSurfaceKey(workItem))
          : [];
        const kindMeta = sourceKindMeta(workItem);
        const sourceMessage = workItem?.source_message_id
          ? messagesById.get(workItem.source_message_id) ?? null
          : null;
        const sourceMissing = Boolean(workItem?.source_message_id) && sourceMessage === null;
        const channel = workItem?.channel_id ? channelsById.get(workItem.channel_id) ?? null : null;
        const threadRoot = workItem?.thread_root_id
          ? messagesById.get(workItem.thread_root_id) ?? null
          : null;
        const channelLabel = channelDisplayLabel(channel);
        const pathSegments: string[] = [];
        if (workItem?.task_number) {
          pathSegments.push(`Task #${workItem.task_number}`);
          if (channel) pathSegments.push(channelLabel);
        } else if (workItem) {
          pathSegments.push(channelLabel);
          if (threadRoot) {
            const threadLabel = rootMessageTitle(threadRoot);
            if (threadLabel) pathSegments.push(threadLabel);
          }
        } else {
          pathSegments.push("Runtime");
        }
        const path = pathSegments.join(" › ");
        const previewSource = sourceMessage?.body
          ?? (workItem && !workItem.source_message_id && workItem.context ? workItem.context : "")
          ?? "";
        const preview = previewSource ? firstLinePreview(previewSource, 80) : "";
        return {
          runId,
          workItem,
          latest,
          activities: sorted,
          queuedBehind,
          providerRetrying: sorted.some(isProviderRetryActivity),
          kindMeta,
          path,
          preview,
          sourceMessage,
          sourceMissing,
          duration: turnDurationLabel(sorted),
        };
      })
      .sort((left, right) => activityTimestamp(right.latest) - activityTimestamp(left.latest));

    return (
      <section className="detail-section live-activity-section">
        <div className="detail-section-head">
          <h4>Recent activity</h4>
          {activityGroups.length > 0 && <span>{activityGroups.length} turns · {activities.length} steps</span>}
        </div>
        {activities.length === 0 && <p className="empty-mini">No activity yet.</p>}
        {activityGroups.length > 0 && (
          <div className="activity-run-list" role="log" aria-label={`${agent.handle} activity by run`}>
            {activityGroups.map((group, index) => {
              const KindIcon = group.kindMeta.icon;
              const jumpable = Boolean(group.workItem) && group.kindMeta.jumpable;
              const collapsedStepCount = index === 0
                ? LATEST_ACTIVITY_RUN_COLLAPSED_STEP_COUNT
                : DEFAULT_ACTIVITY_RUN_COLLAPSED_STEP_COUNT;
              const runExpanded = expandedActivityRuns.has(group.runId);
              const hasHiddenSteps = group.activities.length > collapsedStepCount;
              const visibleActivities = runExpanded
                ? group.activities
                : group.activities.slice(0, collapsedStepCount);
              return (
                <article
                  className="activity-run-card"
                  key={group.runId}
                  ref={(node) => {
                    if (node) {
                      activityRunRefs.current.set(group.runId, node);
                    } else {
                      activityRunRefs.current.delete(group.runId);
                    }
                  }}
                  data-provider-retrying={group.providerRetrying ? "true" : "false"}
                  data-source-kind={group.kindMeta.tone}
                  data-jumpable={jumpable ? "true" : "false"}
                >
                  <header className="activity-run-head">
                    <div className="activity-run-head-main">
                      <div className="activity-run-trigger">
                        <span className="activity-run-kind-icon" aria-hidden="true">
                          <KindIcon size={14} />
                        </span>
                        <span className="activity-run-trigger-label">
                          Triggered by <b>{group.kindMeta.label}</b>
                        </span>
                        <time>{formatActivityTime(group.latest.created_at)}</time>
                        {group.duration && <small>· {group.duration}</small>}
                        {group.providerRetrying && <b className="activity-run-retrying">Provider retrying</b>}
                        {jumpable && (
                          <ArrowRight className="activity-run-arrow" size={13} aria-hidden="true" />
                        )}
                      </div>
                      <p className="activity-run-context">
                        <span className="activity-run-path" title={group.path}>{group.path}</span>
                        {group.preview && (
                          <>
                            <span className="activity-run-context-divider" aria-hidden="true">·</span>
                            <span className="activity-run-preview" title={group.preview}>
                              {group.sourceMissing ? "Original message no longer exists" : `“${group.preview}”`}
                            </span>
                          </>
                        )}
                        {!group.preview && group.workItem?.status && (
                          <>
                            <span className="activity-run-context-divider" aria-hidden="true">·</span>
                            <span>{workItemStatusLabel(group.workItem.status)}</span>
                          </>
                        )}
                      </p>
                    </div>
                    {jumpable && group.workItem && (
                      <button
                        type="button"
                        className="activity-run-open"
                        onClick={() =>
                          onOpenWorkItem(group.workItem as AgentWorkItem, group.sourceMessage?.id ?? null)
                        }
                      >
                        Open
                      </button>
                    )}
                  </header>

                  {group.providerRetrying && (
                    <div className="activity-provider-note">
                      <strong>Lantor is retrying automatically.</strong>
                      <span>No action needed unless this turns into a stalled request.</span>
                    </div>
                  )}

                  {group.queuedBehind.length > 0 && (
                    <div className="activity-queued-note">
                      {group.queuedBehind.length} follow-up{group.queuedBehind.length === 1 ? "" : "s"} queued behind this run on the same surface.
                    </div>
                  )}

                  <ol className="activity-run-steps">
                    {visibleActivities.map((activity) => {
                      const title = userFacingActivityTitle(activity);
                      const detail = userFacingActivityDetail(activity);
                      const category = userFacingActivityCategory(activity);
                      const visibleMetadata = visibleMetadataEntries(activity);
                      const allMetadata = metadataEntries(activity);
                      return (
                        <li
                          key={activity.id}
                          className={`activity-run-step ${expandedActivityId === activity.id ? "expanded" : ""}`}
                          data-kind={activity.kind}
                          data-phase={activity.phase}
                          data-status={activity.status}
                          data-source-kind={group.kindMeta.tone}
                          onClick={() => setExpandedActivityId((current) => current === activity.id ? null : activity.id)}
                        >
                          <time>{formatActivityTime(activity.created_at)}</time>
                          <span
                            className="activity-dot"
                            data-kind={activity.kind}
                            data-phase={activity.phase}
                            data-status={activity.status}
                            data-source-kind={group.kindMeta.tone}
                            aria-hidden="true"
                          />
                          <div className="activity-timeline-body">
                            <div className="activity-timeline-title">
                              <strong>{title}</strong>
                              <span className={`activity-status status-${activity.status}`}>
                                {statusForActivity(activity)}
                              </span>
                            </div>
                            <div className="activity-structure-line">
                              <span>{category}</span>
                              {activity.status === "active" && <span>In progress</span>}
                            </div>
                            {visibleMetadata.length > 0 && (
                              <div className="activity-metadata">
                                {visibleMetadata.map(([key, value]) => (
                                  <span key={key} title={`${key}: ${value}`}>
                                    <b>{key}</b>
                                    {compactValue(value)}
                                  </span>
                                ))}
                              </div>
                            )}
                            {detail && (
                              <p title="Click to expand activity detail">{compactValue(detail, 120)}</p>
                            )}
                            {expandedActivityId === activity.id && allMetadata.length > 0 && (
                              <pre className="activity-raw">{JSON.stringify(activity.metadata, null, 2)}</pre>
                            )}
                          </div>
                        </li>
                      );
                    })}
                  </ol>

                  {hasHiddenSteps && (
                    <div className="activity-run-actions">
                      <button
                        type="button"
                        className="activity-run-toggle"
                        onClick={() => {
                          if (runExpanded) setPendingCollapsedRunScrollId(group.runId);
                          setExpandedActivityRuns((current) => {
                            const next = new Set(current);
                            if (next.has(group.runId)) {
                              next.delete(group.runId);
                            } else {
                              next.add(group.runId);
                            }
                            return next;
                          });
                        }}
                      >
                        {runExpanded
                          ? "Collapse steps"
                          : `Show all ${group.activities.length} steps`}
                      </button>
                      {!runExpanded && (
                        <span className="activity-run-truncation-note">
                          Showing latest {collapsedStepCount} of {group.activities.length}
                        </span>
                      )}
                    </div>
                  )}
                </article>
              );
            })}
          </div>
        )}
        {workItems.some((item) => item.status === "queued") && (
          <div className="activity-queued-summary">
            <strong>{workItems.filter((item) => item.status === "queued").length} queued</strong>
            <span>Queued follow-ups stay visible here while the agent is busy or retrying provider requests.</span>
          </div>
        )}
      </section>
    );
  }

  return (
    <>
    <aside className="agent-drawer">
      <button
        type="button"
        className="thread-resize-handle"
        aria-label="Resize agent detail panel"
        onPointerDown={onResizeStart}
      />
      <header className="agent-drawer-head">
        <button
          type="button"
          className="agent-mobile-back"
          onClick={onClose}
          aria-label="Back"
        >
          <ArrowLeft size={18} />
        </button>
        <div className="agent-title">
          <span className="hash-card agent-title-card" aria-hidden="true">
            <AgentAvatar agent={agent} size="sm" showStatus={false} />
          </span>
          <div>
            <h2>{agent.display_name}</h2>
          </div>
        </div>
        <div className="agent-drawer-head-actions">
          <button
            type="button"
            className="agent-head-action"
            aria-label={`Edit @${agent.handle}`}
            data-tooltip={`Edit @${agent.handle}`}
            onClick={() => onEdit(agent)}
          >
            <Pencil size={16} />
          </button>
          <button
            type="button"
            className="agent-head-action"
            disabled={restartDisabled}
            aria-label={restartLabel}
            title={restartDisabledReason || restartLabel}
            onClick={() => onRestart(agent)}
          >
            <RotateCcw size={16} />
          </button>
          <button
            type="button"
            className="agent-head-action danger"
            disabled={deleteDisabled}
            aria-label={`Delete @${agent.handle}`}
            data-tooltip={deleteDisabled ? "Stop the active run before deleting this agent" : `Delete @${agent.handle}`}
            onClick={() => onDelete(agent)}
          >
            <Trash2 size={16} />
          </button>
          <button type="button" className="agent-close" onClick={onClose} aria-label="Close agent detail">
            <X size={18} />
          </button>
        </div>
      </header>
      <div className="agent-drawer-body">
        <div className="agent-detail">
          <section className="agent-detail-hero">
            <AgentAvatar agent={agent} size="lg" />
            <div>
              <h3>{agent.display_name}</h3>
              <p>@{agent.handle} · {agentModelSummary(agent)}</p>
              {phase && (
                <div className="agent-phase-line">
                  <span className={`phase-badge ${phaseClass(phase.kind)}`}>{phase.label}</span>
                  <small>{phase.detail}</small>
                </div>
              )}
            </div>
          </section>
          <nav className="agent-detail-tabs" role="tablist" aria-label={`${agent.handle} detail sections`}>
            {AGENT_DETAIL_TABS.map((tab) => (
              <button
                key={tab.id}
                id={`agent-detail-tab-${tab.id}`}
                type="button"
                role="tab"
                aria-controls={`agent-detail-panel-${tab.id}`}
                aria-selected={activeDetailTab === tab.id}
                className={`agent-detail-tab ${activeDetailTab === tab.id ? "active" : ""}`}
                onClick={() => setActiveDetailTab(tab.id)}
              >
                <span>{tab.label}</span>
                <small>{tabBadge(tab.id)}</small>
              </button>
            ))}
          </nav>
          <div
            id={`agent-detail-panel-${activeDetailTab}`}
            className={`agent-detail-panel panel-${activeDetailTab}`}
            role="tabpanel"
            aria-labelledby={`agent-detail-tab-${activeDetailTab}`}
          >
            {activeDetailTab === "profile" && renderProfilePanel()}
            {activeDetailTab === "reminders" && renderRemindersPanel()}
            {activeDetailTab === "activity" && renderActivityPanel()}
            {activeDetailTab === "workspace" && renderWorkspacePanel()}
          </div>
        </div>
      </div>
    </aside>
    <Modal
      open={Boolean(workspacePreview)}
      title={workspacePreview?.name ?? "Workspace preview"}
      onClose={() => setWorkspacePreview(null)}
      width={860}
    >
      {workspacePreview && (
        <div className="workspace-preview">
          <div className="workspace-preview-head">
            <div>
              <strong>{workspacePreview.name}</strong>
              <span>
                {compactPath(workspacePreview.path)} · {formatEntrySize(workspacePreview.size_bytes)}
                {workspacePreview.truncated ? " · truncated" : ""}
              </span>
            </div>
          </div>
          {isMarkdownWorkspaceFile(workspacePreview) ? (
            <MessageMarkdown body={workspacePreview.content} scrollKey={`workspace:${workspacePreview.path}`} />
          ) : (
            <pre className="workspace-preview-raw">{workspacePreview.content}</pre>
          )}
        </div>
      )}
    </Modal>
    </>
  );
}
