import {
  AlertCircle,
  ArrowRightLeft,
  AtSign,
  Bell,
  CalendarClock,
  ChevronDown,
  CircleDashed,
  Cpu,
  Hash,
  ListChecks,
  LoaderCircle,
  Mail,
  MessageSquareReply,
  Pencil,
  RotateCw,
  Sparkles,
  Terminal,
  Wrench,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { useState } from "react";
import type { Agent, AgentActivity, AgentRun, AgentWorkItem, Message } from "../types";
import { messageHasVisibleContent, messageRunId } from "../message-grouping";
import { formatClockTime } from "../ui-utils";
import { AgentAvatar } from "./AgentAvatar";

type ActivityProgressDockProps = {
  messages: Message[];
  activities: AgentActivity[];
  runs: AgentRun[];
  workItems: AgentWorkItem[];
  agents: Agent[];
  channelId: string | null;
  threadRootId: string | null;
  onOpenWorkItem?: (item: AgentWorkItem, focusedMessageIdOverride?: string | null) => void;
};

export type SourceKindMeta = {
  icon: LucideIcon;
  label: string;
  tone: string;
  jumpable: boolean;
};

export function sourceKindMeta(workItem: AgentWorkItem | null): SourceKindMeta {
  if (!workItem) {
    return { icon: Cpu, label: "Runtime event", tone: "system", jumpable: false };
  }
  if (workItem.task_number) {
    return {
      icon: ListChecks,
      label: `Task #${workItem.task_number}`,
      tone: "task",
      jumpable: true,
    };
  }
  switch (workItem.source_kind) {
    case "mention":
      return { icon: AtSign, label: "Mention", tone: "mention", jumpable: true };
    case "dm":
      return { icon: Mail, label: "Direct message", tone: "dm", jumpable: true };
    case "thread_followup":
      return { icon: MessageSquareReply, label: "Thread follow-up", tone: "thread_followup", jumpable: true };
    case "channel_message":
      return { icon: Hash, label: "Channel message", tone: "channel_message", jumpable: true };
    case "task":
      return { icon: ListChecks, label: "Task run", tone: "task", jumpable: true };
    case "reminder":
      return { icon: Bell, label: "Reminder", tone: "reminder", jumpable: false };
    case "schedule":
      return { icon: CalendarClock, label: "Routine", tone: "schedule", jumpable: false };
    case "handoff":
    case "collaboration":
      return { icon: ArrowRightLeft, label: "Agent handoff", tone: "handoff", jumpable: true };
    case "self_wake":
      return { icon: RotateCw, label: "Self wake-up", tone: "self_wake", jumpable: false };
    case "system":
      return { icon: Cpu, label: "System", tone: "system", jumpable: false };
    case "manual":
      return { icon: Sparkles, label: "Manual request", tone: "manual", jumpable: true };
    default:
      return {
        icon: CircleDashed,
        label: "Agent request",
        tone: "default",
        jumpable: Boolean(workItem.channel_id),
      };
  }
}

export type ActiveAgentProgress = {
  key: string;
  agent: Pick<Agent, "handle" | "display_name" | "status"> &
    Partial<Pick<Agent, "id" | "runtime" | "model" | "role" | "avatar" | "description">>;
  state: "working" | "queued";
  workItem: AgentWorkItem | null;
  queuedItems: AgentWorkItem[];
  latestActivity: AgentActivity | null;
  history: AgentActivity[];
  latestAt: number;
};

type ProgressCandidate = {
  message: Message | null;
  workItem: AgentWorkItem | null;
  state: "working" | "queued";
  latestAt: number;
};

const HIDDEN_ACTIVITY_TITLES = new Set([
  "Request acknowledged",
  "Stream event accepted",
]);
const MAX_PROGRESS_HISTORY_ITEMS = 20;
const ACTIVE_RUN_STATUSES = new Set(["starting", "running", "stopping"]);
const ACTIVE_WORK_ITEM_STATUSES = new Set(["queued", "running", "cancelling"]);
const SETTLING_WORK_ITEM_STATUSES = new Set(["done", "failed", "cancelled", "silent"]);
const COMPLETION_SETTLE_WINDOW_MS = 15_000;
const ACTIVITY_STATUS_LABELS: Record<string, string> = {
  active: "Active",
  success: "Done",
  warning: "Needs attention",
  error: "Error",
  info: "Info",
};

function activityTitle(activity: AgentActivity) {
  return (activity.summary || activity.title || phaseLabel(activity.phase || activity.kind)).trim();
}

function userFacingActivityTitle(activity: AgentActivity) {
  const title = activityTitle(activity);
  const lowered = title.toLowerCase();
  if (lowered.includes("warm app-server ready") || lowered.includes("warm stream-json ready")) return "Runtime ready";
  if (lowered === "started working" || lowered === "run started" || lowered === "run created") return "Working";
  return title;
}

function statusForActivity(activity: AgentActivity) {
  return ACTIVITY_STATUS_LABELS[activity.status] ?? activity.status;
}

function phaseLabel(phase: string) {
  switch (phase) {
    case "thinking":
      return "Thinking";
    case "command":
      return "Running command";
    case "file_edit":
      return "Editing file";
    case "tools":
      return "Using tools";
    case "runtime":
      return "Runtime";
    case "run_retry":
      return "Provider retrying";
    case "work":
      return "Request";
    case "error":
    case "event_error":
    case "run_error":
      return "Error";
    default:
      return "Working";
  }
}

function activityCategory(activity: AgentActivity) {
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

function progressIcon(activity: AgentActivity | null): LucideIcon {
  if (activity?.status === "error") return AlertCircle;
  switch (activity?.phase || activity?.kind) {
    case "command":
      return Terminal;
    case "file_edit":
      return Pencil;
    case "tools":
      return Wrench;
    case "thinking":
      return CircleDashed;
    default:
      return LoaderCircle;
  }
}

function isProviderRetryActivity(activity: AgentActivity | null) {
  return activity?.kind === "run_retry" || activity?.phase === "run_retry";
}

function activityDetail(activity: AgentActivity) {
  const metadata = activity.metadata ?? {};
  const preferred = [
    metadata.command,
    metadata.file,
    metadata.tool,
    metadata.operation,
    metadata.reason,
  ].find((value) => typeof value === "string" && value.trim());
  if (typeof preferred === "string") return preferred.trim();

  const detail = activity.detail.trim();
  if (!detail || detail.startsWith("{") || detail.startsWith("[")) return "";
  if (detail === "pid unavailable") return "";
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
        .map(([key, value]) => `${key.replace(/_/g, " ")} ${value}`)
        .join(", ");
    }
  }
  return detail;
}

function compact(value: string, limit: number) {
  const normalized = value.replace(/\s+/g, " ").trim();
  if (normalized.length <= limit) return normalized;
  return `${normalized.slice(0, Math.max(0, limit - 1)).trim()}...`;
}

function isUsefulActivity(activity: AgentActivity) {
  const title = activityTitle(activity);
  if (!title || HIDDEN_ACTIVITY_TITLES.has(title)) return false;
  if (activity.kind === "event" && title === "Activity accepted") return false;
  return true;
}

function compactProgressActivities(activities: AgentActivity[]) {
  const seen = new Set<string>();
  let lastSignature = "";
  return activities.filter((activity) => {
    if (seen.has(activity.id)) return false;
    seen.add(activity.id);
    const signature = `${activity.phase || activity.kind || ""}|${userFacingActivityTitle(activity)}|${activity.detail.trim()}`;
    if (signature === lastSignature) return false;
    lastSignature = signature;
    return true;
  });
}

function timestamp(value: string) {
  const parsed = new Date(value).getTime();
  return Number.isFinite(parsed) ? parsed : 0;
}

function recentlyUpdated(value: string) {
  const updatedAt = timestamp(value);
  return updatedAt > 0 && Date.now() - updatedAt <= COMPLETION_SETTLE_WINDOW_MS;
}

function senderHandle(message: Message) {
  return message.sender_name.replace(/^@/, "").trim();
}

function activityAgentHandle(activity: AgentActivity | null) {
  return activity?.agent_handle?.replace(/^@/, "").trim() || "";
}

function isTerminalProgressActivity(activity: AgentActivity | null) {
  if (!activity) return false;
  const title = activityTitle(activity).toLowerCase();
  if (activity.phase === "runtime") {
    return title === "completed" || title === "failed" || title === "stopped";
  }
  if (activity.phase === "work") {
    return title === "request completed"
      || title === "request failed"
      || title === "request cancelled"
      || title === "no visible reply needed";
  }
  return false;
}

function workItemMatchesSurface(
  workItem: AgentWorkItem,
  channelId: string | null,
  threadRootId: string | null,
) {
  if (!channelId || workItem.channel_id !== channelId) return false;
  return (workItem.thread_root_id ?? null) === (threadRootId ?? null);
}

function agentForProgress(
  handle: string,
  activity: AgentActivity | null,
  agents: Agent[],
): ActiveAgentProgress["agent"] {
  const activityHandle = activity?.agent_handle?.replace(/^@/, "").trim();
  const lookup = activityHandle || handle;
  const agent = agents.find((candidate) => candidate.handle === lookup);
  if (agent) return agent;
  const displayName = lookup ? `@${lookup}` : "Agent";
  return {
    handle: lookup || "agent",
    display_name: displayName,
    status: "running",
  };
}

export function activeProgressByAgent(
  messages: Message[],
  activities: AgentActivity[],
  runs: AgentRun[],
  workItems: AgentWorkItem[],
  agents: Agent[],
  channelId: string | null,
  threadRootId: string | null,
) {
  const streamingMessages = messages
    .map((message) => ({ message, runId: messageRunId(message) }))
    .filter(({ message, runId }) =>
      runId
      && message.sender_role !== "owner"
      && message.sender_role !== "system"
      && message.delivery_state === "streaming"
      && !messageHasVisibleContent(message));

  const activitiesByRun = new Map<string, AgentActivity[]>();
  activities
    .filter(isUsefulActivity)
    .sort((left, right) => timestamp(right.created_at) - timestamp(left.created_at))
    .forEach((activity) => {
      if (!activity.run_id) return;
      const current = activitiesByRun.get(activity.run_id) ?? [];
      current.push(activity);
      activitiesByRun.set(activity.run_id, current);
    });

  const runsById = new Map(runs.map((run) => [run.id, run]));
  const surfaceWorkItems = workItems.filter((workItem) => workItemMatchesSurface(workItem, channelId, threadRootId));
  const candidatesByRun = new Map<string, ProgressCandidate>();
  const addCandidate = (runId: string, candidate: ProgressCandidate) => {
    const current = candidatesByRun.get(runId);
    if (!current || candidate.latestAt > current.latestAt) {
      candidatesByRun.set(runId, candidate);
    }
  };

  streamingMessages.forEach(({ message, runId }) => {
    if (!runId) return;
    addCandidate(runId, {
      message,
      workItem: null,
      state: "working",
      latestAt: timestamp(message.updated_at),
    });
  });

  surfaceWorkItems
    .filter((workItem) => workItem.run_id)
    .forEach((workItem) => {
      const runId = workItem.run_id;
      if (!runId) return;
      const run = runsById.get(runId);
      const latestActivity = activitiesByRun.get(runId)?.[0] ?? null;
      const activeRun = Boolean(run && ACTIVE_RUN_STATUSES.has(run.status));
      const activeWorkItem = ACTIVE_WORK_ITEM_STATUSES.has(workItem.status);
      const settlingWorkItem = SETTLING_WORK_ITEM_STATUSES.has(workItem.status)
        && !isTerminalProgressActivity(latestActivity)
        && recentlyUpdated(workItem.updated_at);

      if (!activeRun && !activeWorkItem && !settlingWorkItem) return;
      addCandidate(runId, {
        message: null,
        workItem,
        state: "working",
        latestAt: Math.max(
          timestamp(workItem.updated_at),
          timestamp(run?.started_at ?? ""),
          timestamp(run?.stopped_at ?? ""),
        ),
      });
    });

  const progressByAgent = new Map<string, ActiveAgentProgress>();
  candidatesByRun.forEach((candidate, runId) => {
    const runActivities = compactProgressActivities(activitiesByRun.get(runId) ?? []);
    const latestActivity = runActivities[0] ?? null;
    if (isTerminalProgressActivity(latestActivity)) return;

    const handle = activityAgentHandle(latestActivity)
      || candidate.workItem?.agent_handle
      || (candidate.message ? senderHandle(candidate.message) : "");
    const key = handle || candidate.message?.sender_name || runId;
    const latestAt = Math.max(timestamp(latestActivity?.created_at ?? ""), candidate.latestAt);
    const existing = progressByAgent.get(key);
    const history = [...runActivities, ...(existing?.history ?? [])]
      .sort((left, right) => timestamp(right.created_at) - timestamp(left.created_at));
    const compactHistory = compactProgressActivities(history).slice(0, MAX_PROGRESS_HISTORY_ITEMS);
    progressByAgent.set(key, {
      key,
      agent: existing?.agent ?? agentForProgress(handle, latestActivity, agents),
      workItem: candidate.workItem ?? existing?.workItem ?? null,
      queuedItems: existing?.queuedItems ?? [],
      state: existing?.state === "working" || candidate.state === "working" ? "working" : "queued",
      latestActivity: existing && existing.latestAt > latestAt ? existing.latestActivity : latestActivity,
      history: compactHistory,
      latestAt: Math.max(existing?.latestAt ?? 0, latestAt),
    });
  });

  surfaceWorkItems
    .filter((workItem) => workItem.status === "queued")
    .forEach((workItem) => {
      const key = workItem.agent_handle || workItem.agent_id;
      const existing = progressByAgent.get(key);
      const queuedItems = [...(existing?.queuedItems ?? [])];
      if (!queuedItems.some((item) => item.id === workItem.id)) queuedItems.push(workItem);
      const agent = existing?.agent
        ?? agents.find((candidate) => candidate.id === workItem.agent_id)
        ?? {
          id: workItem.agent_id,
          handle: workItem.agent_handle || "agent",
          display_name: workItem.agent_handle ? `@${workItem.agent_handle}` : "Agent",
          status: "idle",
        };

      progressByAgent.set(key, {
        key,
        agent,
        state: existing?.state ?? "queued",
        workItem: existing?.workItem ?? null,
        queuedItems,
        latestActivity: existing?.latestActivity ?? null,
        history: existing?.history ?? [],
        latestAt: Math.max(existing?.latestAt ?? 0, timestamp(workItem.updated_at)),
      });
    });

  return Array.from(progressByAgent.values())
    .sort((left, right) => right.latestAt - left.latestAt);
}

export function ActivityProgressDock({
  messages,
  activities,
  runs,
  workItems,
  agents,
  channelId,
  threadRootId,
  onOpenWorkItem,
}: ActivityProgressDockProps) {
  const [historyOpen, setHistoryOpen] = useState(false);
  const progress = activeProgressByAgent(messages, activities, runs, workItems, agents, channelId, threadRootId);
  if (progress.length === 0) return null;

  const workingCount = progress.filter((item) => item.state === "working").length;
  const latest = progress.find((item) => item.state === "working") ?? progress[0];
  const latestActivity = latest.latestActivity;
  const latestWorking = latest.state === "working";
  const Icon = progressIcon(latestActivity);
  const providerRetrying = isProviderRetryActivity(latestActivity);
  const queuedCount = progress.reduce((count, item) => count + item.queuedItems.length, 0);
  const latestSourceWorkItem = latest.workItem ?? latest.queuedItems[0] ?? null;
  const latestKindMeta = sourceKindMeta(latestSourceWorkItem);
  const KindIcon = latestKindMeta.icon;
  const jumpable = Boolean(latestSourceWorkItem) && latestKindMeta.jumpable && Boolean(onOpenWorkItem);
  const title = progress.length === 1
    ? providerRetrying
      ? `${latest.agent.display_name} is waiting on provider`
      : latestWorking
        ? `${latest.agent.display_name} is working`
        : `${latest.agent.display_name} has queued work`
    : workingCount > 0
      ? `${workingCount} ${workingCount === 1 ? "agent is" : "agents are"} working`
      : `${progress.length} agents have queued work`;
  const latestTitle = latestActivity ? userFacingActivityTitle(latestActivity) : latestWorking ? "Working" : "Queued";
  const latestDetail = latestActivity ? activityDetail(latestActivity) : "";
  const history = progress
    .flatMap((item) =>
      item.history.map((activity) => ({
        activity,
        agent: item.agent,
        workItem: item.workItem,
      })),
    )
    .sort((left, right) => timestamp(right.activity.created_at) - timestamp(left.activity.created_at))
    .slice(0, MAX_PROGRESS_HISTORY_ITEMS);
  const state = providerRetrying ? "provider-retrying" : latestWorking ? "working" : "queued";

  const handleJump = () => {
    if (!latestSourceWorkItem || !onOpenWorkItem) return;
    onOpenWorkItem(latestSourceWorkItem, latestSourceWorkItem.source_message_id ?? null);
  };

  return (
    <div className="activity-progress-dock" data-source-kind={latestKindMeta.tone}>
      <div className="activity-progress-summary" data-state={state}>
        <button
          type="button"
          className="activity-progress-summary-main"
          onClick={() => {
            if (jumpable) {
              handleJump();
            } else if (history.length > 0) {
              setHistoryOpen((current) => !current);
            }
          }}
          disabled={!jumpable && history.length === 0}
          aria-expanded={!jumpable ? historyOpen : undefined}
          aria-label={jumpable ? `Jump to ${latestKindMeta.label.toLowerCase()} source` : "Toggle activity history"}
        >
          <span className="activity-progress-avatar-stack" aria-hidden="true">
            {progress.slice(0, 3).map((item) => (
              <AgentAvatar key={item.key} agent={item.agent} size="sm" showStatus={false} />
            ))}
          </span>
          <span className="activity-progress-copy">
            <strong>{title}</strong>
            <small>
              <KindIcon className="activity-progress-kind-icon" size={13} aria-hidden="true" />
              <span className="activity-progress-kind-label">{latestKindMeta.label}</span>
              <Icon className="activity-progress-phase-icon" size={13} aria-hidden="true" />
              <span>{latestTitle}</span>
              {latestDetail && <em>{compact(latestDetail, 80)}</em>}
              {queuedCount > 0 && <em>{queuedCount} queued on this surface</em>}
            </small>
          </span>
          {jumpable && (
            <span className="activity-progress-jump-arrow" aria-hidden="true">→</span>
          )}
        </button>
        {history.length > 0 && (
          <button
            type="button"
            className="activity-progress-toggle"
            onClick={() => setHistoryOpen((current) => !current)}
            aria-expanded={historyOpen}
            aria-label={historyOpen ? "Hide activity history" : "Show activity history"}
          >
            <ChevronDown
              className="activity-progress-chevron"
              data-open={historyOpen ? "true" : "false"}
              size={14}
            />
          </button>
        )}
      </div>
      {historyOpen && history.length > 0 && (
        <ol className="activity-progress-history">
          {history.map(({ activity, agent, workItem }) => {
            const detail = activityDetail(activity);
            const rowMeta = sourceKindMeta(workItem ?? null);
            return (
              <li
                key={activity.id}
                className="activity-run-step"
                data-kind={activity.kind}
                data-phase={activity.phase}
                data-status={activity.status}
                data-source-kind={rowMeta.tone}
              >
                <time>{formatClockTime(activity.created_at)}</time>
                <span
                  className="activity-dot"
                  data-kind={activity.kind}
                  data-phase={activity.phase}
                  data-status={activity.status}
                  data-source-kind={rowMeta.tone}
                  aria-hidden="true"
                />
                <div className="activity-timeline-body">
                  <div className="activity-timeline-title">
                    <strong>{userFacingActivityTitle(activity)}</strong>
                    <span className={`activity-status status-${activity.status}`}>
                      {statusForActivity(activity)}
                    </span>
                  </div>
                  <div className="activity-structure-line">
                    <span>{activityCategory(activity)}</span>
                    <span>{agent.display_name}</span>
                  </div>
                  {detail && <p title={detail}>{compact(detail, 132)}</p>}
                </div>
              </li>
            );
          })}
        </ol>
      )}
    </div>
  );
}
