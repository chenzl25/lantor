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
import { memo, useEffect, useRef, useState, useSyncExternalStore } from "react";
import { ACTIVE_RUN_STATUSES } from "../types";
import type { Agent, AgentActivity, AgentRun, AgentWorkItem, Message } from "../types";
import { messageHasVisibleContent, messageRunId } from "../message-grouping";
import { formatClockTime } from "../ui-utils";
import { streamingMessages } from "../streaming-message-store";
import { useEventCallback } from "../hooks/useEventCallback";
import { AgentAvatar } from "./AgentAvatar";

const MISSING_HISTORY_GRACE_MS = 4_000;
/** Past turns: 4% go two minutes without any activity, so this marks an unusual pause. */
const QUIET_AFTER_MS = 120_000;

type ActivityProgressDockProps = {
  progress: ActiveAgentProgress[];
  onOpenWorkItem?: (item: AgentWorkItem, focusedMessageIdOverride?: string | null) => void;
  /** Loads the given agents' recent activity when this client has none for a live run. */
  onLoadActivityHistory?: (agentIds: string[]) => Promise<unknown>;
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
  /** When the live run started, or null while only queued. */
  startedAt: number | null;
  /** Run start, latest run activity or reply write; text deltas are tracked by the dock. */
  lastUpdateAt: number | null;
  /** The run's streaming reply, whose text deltas also count as updates. */
  streamMessageId: string | null;
  /** Commands and file edits the run has started, or null before the server reports them. */
  toolCalls: RunToolCalls | null;
};

export type RunToolCalls = { commands: number; edits: number };

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
const ACTIVE_WORK_ITEM_STATUSES = new Set(["queued", "running", "cancelling"]);
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

function metadataCount(value: unknown) {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : null;
}

/**
 * The server stamps every run activity with the run's running totals, so the
 * newest activity this client holds is exact even after a reload trims history.
 */
export function runToolCalls(activities: AgentActivity[]): RunToolCalls | null {
  let reported = false;
  const totals: RunToolCalls = { commands: 0, edits: 0 };
  for (const activity of activities) {
    const commands = metadataCount(activity.metadata?.run_command_count);
    const edits = metadataCount(activity.metadata?.run_file_edit_count);
    if (commands === null || edits === null) continue;
    reported = true;
    totals.commands = Math.max(totals.commands, commands);
    totals.edits = Math.max(totals.edits, edits);
  }
  return reported ? totals : null;
}

function timestamp(value: string) {
  const parsed = new Date(value).getTime();
  return Number.isFinite(parsed) ? parsed : 0;
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

export function indexProgress(
  activities: AgentActivity[], runs: AgentRun[], workItems: AgentWorkItem[], agents: Agent[],
) {
  const activitiesByRun = new Map<string, AgentActivity[]>();
  // Sort useful history once for all roots, not once per visible thread.
  for (const activity of activities.filter(isUsefulActivity)
    .sort((left, right) => timestamp(right.created_at) - timestamp(left.created_at))) {
    if (!activity.run_id) continue;
    const group = activitiesByRun.get(activity.run_id) ?? [];
    group.push(activity);
    activitiesByRun.set(activity.run_id, group);
  }
  const workItemsByChannel = new Map<string, Map<string | null, AgentWorkItem[]>>();
  const workItemsByRun = new Map<string, AgentWorkItem>();
  for (const item of workItems) {
    if (item.run_id) workItemsByRun.set(item.run_id, item);
    if (!item.channel_id) continue;
    const channel = workItemsByChannel.get(item.channel_id) ?? new Map<string | null, AgentWorkItem[]>();
    const root = item.thread_root_id ?? null;
    const group = channel.get(root) ?? [];
    group.push(item);
    channel.set(root, group);
    workItemsByChannel.set(item.channel_id, channel);
  }
  const latestRunsByAgent = new Map<string, AgentRun>();
  for (const run of runs) {
    const latest = latestRunsByAgent.get(run.agent_id);
    if (!latest || timestamp(run.started_at) > timestamp(latest.started_at)) {
      latestRunsByAgent.set(run.agent_id, run);
    }
  }
  return {
    activitiesByRun,
    runsById: new Map(runs.map((run) => [run.id, run])),
    latestRunsByAgent,
    workItemsByRun,
    workItemsByChannel,
    agentsById: new Map(agents.map((agent) => [agent.id, agent])),
    agentsByHandle: new Map(agents.map((agent) => [agent.handle, agent])),
  };
}

export function activeProgressByAgent(
  messages: Message[],
  index: ReturnType<typeof indexProgress>,
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

  // Visible streaming replies; empty placeholders are not in the row lists.
  const streamMessageByRun = new Map<string, Message>();
  messages.forEach((message) => {
    const runId = message.delivery_state === "streaming" ? messageRunId(message) : null;
    if (runId) streamMessageByRun.set(runId, message);
  });

  const { activitiesByRun, runsById, latestRunsByAgent, workItemsByRun, agentsById, agentsByHandle } = index;
  const surfaceWorkItems = channelId
    ? index.workItemsByChannel.get(channelId)?.get(threadRootId) ?? []
    : [];
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
      const activeRun = Boolean(run && ACTIVE_RUN_STATUSES.has(run.status));
      const activeWorkItem = ACTIVE_WORK_ITEM_STATUSES.has(workItem.status);

      if (!activeRun && !activeWorkItem) return;
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
    const run = runsById.get(runId);
    // A crash, restart or failed launch can leave a streaming placeholder and
    // no final activity. The run's lifecycle is authoritative over both.
    if (run && !ACTIVE_RUN_STATUSES.has(run.status)) return;
    const runActivities = compactProgressActivities(activitiesByRun.get(runId) ?? []);
    const latestActivity = runActivities[0] ?? null;
    if (isTerminalProgressActivity(latestActivity)) return;

    const workItem = candidate.workItem ?? workItemsByRun.get(runId) ?? null;
    const agentId = run?.agent_id || workItem?.agent_id
      || candidate.message?.sender_agent_id || latestActivity?.agent_id;
    const handle = activityAgentHandle(latestActivity)
      || workItem?.agent_handle
      || (candidate.message ? senderHandle(candidate.message) : "");
    const agent = (agentId ? agentsById.get(agentId) : undefined) ?? agentsByHandle.get(handle);
    // Compact history can omit old runs. Do not revive their placeholders
    // from an idle profile, settled work, or a newer run on another surface.
    // A live run/work item still wins over a profile read that lags behind it.
    if (!run && !(workItem && ACTIVE_WORK_ITEM_STATUSES.has(workItem.status))) {
      if (workItem || (agent && !ACTIVE_RUN_STATUSES.has(agent.status))) return;
      const latestRun = latestRunsByAgent.get(agentId || agent?.id || "");
      if (latestRun && timestamp(latestRun.started_at) > timestamp(candidate.message?.created_at ?? "")) return;
    }
    const key = agent?.handle || handle || candidate.message?.sender_name || runId;
    const latestAt = Math.max(timestamp(latestActivity?.created_at ?? ""), candidate.latestAt);
    const startedAt = timestamp(run?.started_at ?? "") || timestamp(candidate.message?.created_at ?? "") || null;
    const streamMessage = streamMessageByRun.get(runId) ?? null;
    const lastUpdateAt = Math.max(
      timestamp(latestActivity?.created_at ?? ""),
      timestamp(streamMessage?.updated_at ?? ""),
      startedAt ?? 0,
    ) || null;
    const toolCalls = runToolCalls(runActivities);
    const existing = progressByAgent.get(key);
    // A newer run for the same agent supplies the summary's step and clock.
    const newer = existing && existing.latestAt > latestAt ? existing : null;
    const history = [...runActivities, ...(existing?.history ?? [])]
      .sort((left, right) => timestamp(right.created_at) - timestamp(left.created_at));
    const compactHistory = compactProgressActivities(history).slice(0, MAX_PROGRESS_HISTORY_ITEMS);
    progressByAgent.set(key, {
      key,
      agent: existing?.agent ?? agent ?? {
        handle: handle || "agent",
        display_name: handle ? `@${handle}` : "Agent",
        status: "running",
      },
      workItem: workItem ?? existing?.workItem ?? null,
      queuedItems: existing?.queuedItems ?? [],
      state: existing?.state === "working" || candidate.state === "working" ? "working" : "queued",
      latestActivity: newer ? newer.latestActivity : latestActivity,
      history: compactHistory,
      latestAt: Math.max(existing?.latestAt ?? 0, latestAt),
      startedAt: newer ? newer.startedAt : startedAt,
      lastUpdateAt: newer ? newer.lastUpdateAt : lastUpdateAt,
      streamMessageId: newer ? newer.streamMessageId : streamMessage?.id ?? null,
      toolCalls: newer ? newer.toolCalls : toolCalls,
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
        ?? agentsById.get(workItem.agent_id)
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
        startedAt: existing?.startedAt ?? null,
        lastUpdateAt: existing?.lastUpdateAt ?? null,
        streamMessageId: existing?.streamMessageId ?? null,
        toolCalls: existing?.toolCalls ?? null,
      });
    });

  return Array.from(progressByAgent.values())
    .sort((left, right) => right.latestAt - left.latestAt);
}

// One shared one-second clock for every visible dock, running only while one
// is mounted, so the summary ticks without re-rendering the dock each second.
const clockListeners = new Set<() => void>();
let clockNow = Date.now();
let clockTimer: number | null = null;

function subscribeClock(listener: () => void) {
  clockListeners.add(listener);
  if (clockTimer === null) {
    clockNow = Date.now();
    clockTimer = window.setInterval(() => {
      clockNow = Date.now();
      clockListeners.forEach((notify) => notify());
    }, 1_000);
  }
  return () => {
    clockListeners.delete(listener);
    if (clockListeners.size === 0 && clockTimer !== null) {
      window.clearInterval(clockTimer);
      clockTimer = null;
    }
  };
}

function useSecondClock() {
  return useSyncExternalStore(subscribeClock, () => clockNow, () => clockNow);
}

export function formatProgressDuration(ms: number) {
  const totalSeconds = Math.max(0, Math.floor(ms / 1_000));
  const hours = Math.floor(totalSeconds / 3_600);
  const minutes = Math.floor((totalSeconds % 3_600) / 60);
  const seconds = totalSeconds % 60;
  if (hours > 0) return `${hours}h ${String(minutes).padStart(2, "0")}m`;
  if (minutes > 0) return `${minutes}m ${String(seconds).padStart(2, "0")}s`;
  return `${seconds}s`;
}

function ProgressElapsed({ startedAt }: { startedAt: number }) {
  const now = useSecondClock();
  return (
    <time
      className="activity-progress-elapsed"
      dateTime={new Date(startedAt).toISOString()}
      title={`Started ${formatClockTime(new Date(startedAt).toISOString())}`}
    >
      {formatProgressDuration(now - startedAt)}
    </time>
  );
}

/**
 * Time since the run last showed a sign of life: its start, an activity, or
 * streamed reply text. Text deltas bypass the message list and carry no
 * timestamp, so the dock notes when they arrive while it is on screen.
 */
function ProgressLastUpdate({ lastUpdateAt, streamMessageId }: {
  lastUpdateAt: number;
  streamMessageId: string | null;
}) {
  const now = useSecondClock();
  const textAtRef = useRef(0);
  useEffect(() => {
    textAtRef.current = 0;
    if (!streamMessageId) return undefined;
    return streamingMessages.subscribe(streamMessageId, () => {
      textAtRef.current = Date.now();
    });
  }, [streamMessageId]);
  const quietMs = Math.max(0, now - Math.max(lastUpdateAt, textAtRef.current));
  const quiet = quietMs >= QUIET_AFTER_MS;
  return (
    <time className="activity-progress-updated" data-quiet={quiet ? "true" : "false"}>
      {quiet ? `No update for ${formatProgressDuration(quietMs)}` : `${formatProgressDuration(quietMs)} ago`}
    </time>
  );
}

function plural(count: number, one: string, many: string) {
  return `${count} ${count === 1 ? one : many}`;
}

/** Shows only nonzero totals, so a plain answer adds nothing to the heading. */
function ProgressToolCalls({ toolCalls }: { toolCalls: RunToolCalls }) {
  if (toolCalls.commands === 0 && toolCalls.edits === 0) return null;
  const label = [
    toolCalls.commands > 0 ? plural(toolCalls.commands, "command", "commands") : "",
    toolCalls.edits > 0 ? plural(toolCalls.edits, "file edit", "file edits") : "",
  ].filter(Boolean).join(", ");
  return (
    <span className="activity-progress-tool-calls" role="img" aria-label={label} title={label}>
      {toolCalls.commands > 0 && (
        <span data-tool-call="command">
          <Terminal size={12} aria-hidden="true" />
          {toolCalls.commands}
        </span>
      )}
      {toolCalls.edits > 0 && (
        <span data-tool-call="edit">
          <Pencil size={12} aria-hidden="true" />
          {toolCalls.edits}
        </span>
      )}
    </span>
  );
}

function progressAgentIds(progress: ActiveAgentProgress[]) {
  return [...new Set(progress.flatMap((item) => [item.agent.id, item.workItem?.agent_id]
    .filter((id): id is string => Boolean(id))))];
}

function ActivityProgressDockContent({ progress, onOpenWorkItem, onLoadActivityHistory }: ActivityProgressDockProps) {
  const [historyOpen, setHistoryOpen] = useState(false);
  const [historyLoad, setHistoryLoad] = useState<"idle" | "loading" | "done">("idle");
  // One recovery attempt per set of live runs; the live stream fills in the rest.
  const recoveredRunsRef = useRef("");
  const missingHistoryRuns = progress
    .filter((item) => item.state === "working" && item.history.length === 0)
    .map((item) => item.workItem?.run_id ?? item.key)
    .join("|");
  const missingHistoryAgents = progressAgentIds(progress.filter((item) => item.state === "working" && item.history.length === 0));
  const missingHistoryAgentKey = missingHistoryAgents.join("|");

  // A client can hold a live run without its activity: a resumed phone whose
  // event stream died, or a bootstrap trimmed to a few rows per agent. Load the
  // agents' recent activity once per set of runs instead of showing a bare
  // "Working"; the grace period leaves normal run start-up to the live stream.
  const loadMissingHistory = useEventCallback(() => {
    if (!onLoadActivityHistory || !missingHistoryRuns || missingHistoryAgents.length === 0) return;
    if (recoveredRunsRef.current === missingHistoryRuns) return;
    recoveredRunsRef.current = missingHistoryRuns;
    setHistoryLoad("loading");
    void onLoadActivityHistory(missingHistoryAgents)
      .catch(() => undefined)
      .finally(() => setHistoryLoad("done"));
  });
  useEffect(() => {
    if (!missingHistoryRuns || !missingHistoryAgentKey) return;
    const timer = window.setTimeout(loadMissingHistory, MISSING_HISTORY_GRACE_MS);
    return () => window.clearTimeout(timer);
  }, [missingHistoryRuns, missingHistoryAgentKey, loadMissingHistory]);

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
            <span className="activity-progress-heading">
              <strong>{title}</strong>
              {latestWorking && latest.startedAt !== null && <ProgressElapsed startedAt={latest.startedAt} />}
              {latestWorking && latest.toolCalls && <ProgressToolCalls toolCalls={latest.toolCalls} />}
            </span>
            <small>
              <KindIcon className="activity-progress-kind-icon" size={13} aria-hidden="true" />
              <span className="activity-progress-kind-label">{latestKindMeta.label}</span>
              <Icon className="activity-progress-phase-icon" size={13} aria-hidden="true" />
              <span>{latestTitle}</span>
              {latestDetail && <em>{compact(latestDetail, 80)}</em>}
              {queuedCount > 0 && <em>{queuedCount} queued on this surface</em>}
              {latestWorking && !providerRetrying && latest.lastUpdateAt !== null && (
                <ProgressLastUpdate lastUpdateAt={latest.lastUpdateAt} streamMessageId={latest.streamMessageId} />
              )}
            </small>
          </span>
          {jumpable && (
            <span className="activity-progress-jump-arrow" aria-hidden="true">→</span>
          )}
        </button>
        {(history.length > 0 || latestWorking) && (
          <button
            type="button"
            className="activity-progress-toggle"
            onClick={() => {
              if (!historyOpen && history.length === 0) loadMissingHistory();
              setHistoryOpen((current) => !current);
            }}
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
      {historyOpen && history.length === 0 && (
        <p className="activity-progress-history-empty" role="status">
          {historyLoad === "loading" ? "Loading activity…" : "No activity recorded for this run yet."}
        </p>
      )}
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

export const ActivityProgressDock = memo(ActivityProgressDockContent);
