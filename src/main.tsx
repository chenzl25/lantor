import {
  Component,
  Profiler,
  type CSSProperties,
  type ErrorInfo,
  type ProfilerOnRenderCallback,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { createRoot } from "react-dom/client";
import { Bookmark, Home, Inbox, Search } from "lucide-react";
import { apiInvoke, completeStartupSplash, isTauriRuntime, subscribeBackendEvents } from "./apiClient";
import { APP_DISPLAY_NAME } from "./branding";
import { AgentDetailDrawer } from "./components/AgentDetailDrawer";
import type { AgentPerformance } from "./components/AgentDetailDrawer";
import { AgentFormModal } from "./components/AgentFormModal";
import { randomDylanAvatarSpec } from "./avatar-utils";
import { ChannelAgentsModal } from "./components/ChannelAgentsModal";
import { ChannelSettingsModal } from "./components/ChannelSettingsModal";
import { ConfirmModal } from "./components/ConfirmModal";
import { Conversation } from "./components/Conversation";
import { CreateChannelModal } from "./components/CreateChannelModal";
import { ActivityFeedModal } from "./components/ActivityFeedModal";
import { OwnerProfileModal, ownerProfileToForm, type OwnerProfileForm } from "./components/OwnerProfileModal";
import { SavedMessagesModal } from "./components/SavedMessagesModal";
import { SearchModal } from "./components/SearchModal";
import { SettingsModal, type ChatTextSize, type ThemePreference } from "./components/SettingsModal";
import { Sidebar } from "./components/Sidebar";
import { ThreadPanel } from "./components/ThreadPanel";
import { UnreadBadge } from "./components/UnreadBadge";
import { isProgressOnlyMessage } from "./message-grouping";
import {
  ACTIVE_RUN_STATUSES,
  Agent,
  AgentActivity,
  AgentForm,
  AgentRun,
  AgentWorkItem,
  Artifact,
  Bootstrap,
  ChannelMember,
  DraftAttachment,
  EMPTY_AGENT_FORM,
  ActivityFeedItem,
  Message,
  MessageAttachment,
  RUNTIME_PRESETS,
  RuntimeCheck,
  SavedMessage,
  SearchResult,
  SearchScope,
  SearchTimeRange,
  Task,
  ThreadActivity,
  ThreadReplySummary,
} from "./types";
import { agentRequestSourceLabel, buildPresetCommand, firstLines, formatTime, visibleChannelDescription } from "./ui-utils";
import "./styles.css";

type BenchmarkCommit = {
  id: string;
  phase: string;
  actualDuration: number;
  baseDuration: number;
  startTime: number;
  commitTime: number;
};

declare global {
  interface Window {
    __LANTOR_BENCH_PROFILER__?: {
      commits: BenchmarkCommit[];
      reset: () => void;
    };
  }
}

function shouldEnableBenchProfiler() {
  if (typeof window === "undefined") return false;
  const params = new URLSearchParams(window.location.search);
  return params.has("lantorBenchProfiler") || window.localStorage.getItem("lantor:bench-profiler") === "1";
}

function ensureBenchProfilerStore() {
  if (!window.__LANTOR_BENCH_PROFILER__) {
    window.__LANTOR_BENCH_PROFILER__ = {
      commits: [],
      reset() {
        this.commits = [];
      },
    };
  }
  return window.__LANTOR_BENCH_PROFILER__;
}

const recordBenchCommit: ProfilerOnRenderCallback = (id, phase, actualDuration, baseDuration, startTime, commitTime) => {
  ensureBenchProfilerStore().commits.push({
    id,
    phase,
    actualDuration,
    baseDuration,
    startTime,
    commitTime,
  });
};

const ACTIVITY_PHASE_LABELS: Record<string, string> = {
  thinking: "Thinking",
  command: "Running command",
  file_edit: "Editing file",
  runtime: "Runtime",
  work: "Work",
  profile: "Profile",
  acting: "Acting",
  tools: "Using tools",
  error: "Error",
  event: "Acting",
  message: "Acting",
  task: "Acting",
  event_error: "Error",
  run_error: "Error",
  run_retry: "Retrying",
};

const LEGACY_DEFAULT_THREAD_PANEL_WIDTH = 420;
const DEFAULT_THREAD_PANEL_WIDTH = 560;
const MIN_THREAD_PANEL_WIDTH = 320;
const LEGACY_DEFAULT_AGENT_DRAWER_WIDTH = 420;
const DEFAULT_AGENT_DRAWER_WIDTH = 560;
const MIN_AGENT_DRAWER_WIDTH = 420;
const DEFAULT_SIDEBAR_WIDTH = 292;
const MIN_SIDEBAR_WIDTH = 240;
const MAX_SIDEBAR_WIDTH = 460;
const MIN_CONVERSATION_WIDTH = 480;
const MIN_AGENT_DETAIL_CONVERSATION_WIDTH = 360;
const COMPACT_LAYOUT_BREAKPOINT = 1100;
const MIN_COMPACT_CONTENT_WIDTH = 320;
const MIN_COMPACT_SIDEBAR_VISIBLE_WIDTH = 220;
const MOBILE_BREAKPOINT = 760;
const UI_REFRESH_DEBOUNCE_MS = 80;
const EPHEMERAL_FLUSH_FALLBACK_MS = 80;
const MIN_BOOT_SPLASH_MS = 3000;
const MAX_ATTACHMENT_BYTES = 25 * 1024 * 1024;
const ACTIVITY_HISTORY_LIMIT_PER_AGENT = 80;
// Issue #82: run states that must bypass the ephemeral coalescing buffer
// so terminal transitions land immediately. "stopped" is included to
// match runtime supervisor terminology even if it is rare in practice.
const RUN_TERMINAL_STATUSES = new Set(["completed", "failed", "cancelled", "stopped"]);
const DEFAULT_OWNER_DISPLAY_NAME = "Me";
const DEFAULT_OWNER_AVATAR = "dicebear:dylan:owner";
const DEFAULT_OWNER_DESCRIPTION = "local owner";
const OWNER_MENTION_HANDLES = ["@Theo", "@Dylan"];
const CHANNEL_THREAD_MEMORY_STORAGE_KEY = "lantor.channelThreadMemory";
const THREAD_PANEL_WIDTH_STORAGE_KEY = "lantor.threadPanelWidth";
const AGENT_DRAWER_WIDTH_STORAGE_KEY = "lantor.agentDrawerWidth";
const SIDEBAR_WIDTH_STORAGE_KEY = "lantor.sidebarWidth";
const THEME_PREFERENCE_STORAGE_KEY = "lantor.themePreference";
const CHAT_TEXT_SIZE_STORAGE_KEY = "lantor.chatTextSize";
const SHOW_IMAGE_THUMBNAILS_STORAGE_KEY = "lantor.showImageThumbnails";
const MOBILE_EDGE_SWIPE_START_PX = 24;
const MOBILE_EDGE_SWIPE_OPEN_PX = 72;
const MOBILE_EDGE_SWIPE_MAX_VERTICAL_PX = 48;
const MOBILE_SIDEBAR_PEEK_PX = 18;
const MOBILE_SIDEBAR_FLING_VELOCITY = 0.45;
const SAVED_MESSAGES_READ_DISMISS_ID = "saved-messages";
const CHAT_TEXT_SIZE_OPTIONS: ChatTextSize[] = ["compact", "default", "large", "xlarge"];
const UI_TYPE_SCALE: Record<ChatTextSize, Record<string, string>> = {
  compact: {
    "--type-size-caption": "10px",
    "--type-size-meta": "11px",
    "--type-size-body": "13px",
    "--type-size-message": "14px",
    "--type-size-control": "16px",
    "--type-size-title": "17px",
    "--type-size-display": "20px",
  },
  default: {
    "--type-size-caption": "11px",
    "--type-size-meta": "12px",
    "--type-size-body": "14px",
    "--type-size-message": "15px",
    "--type-size-control": "16px",
    "--type-size-title": "18px",
    "--type-size-display": "21px",
  },
  large: {
    "--type-size-caption": "12px",
    "--type-size-meta": "13px",
    "--type-size-body": "15px",
    "--type-size-message": "16px",
    "--type-size-control": "17px",
    "--type-size-title": "19px",
    "--type-size-display": "22px",
  },
  xlarge: {
    "--type-size-caption": "13px",
    "--type-size-meta": "14px",
    "--type-size-body": "16px",
    "--type-size-message": "17px",
    "--type-size-control": "18px",
    "--type-size-title": "20px",
    "--type-size-display": "23px",
  },
};

type UiBackendEvent =
  | { type: "refresh"; reason?: string }
  | { type: "batch"; events: string[] }
  | { type: "message_upsert"; reason?: string; message: Message }
  | { type: "message_delta"; reason?: string; message_id: string; append: string; delivery_state: Message["delivery_state"] }
  | { type: "message_delete"; reason?: string; message_id: string }
  | { type: "activity_upsert"; reason?: string; activity: AgentActivity }
  | { type: "agent_run_upsert"; reason?: string; run: Omit<AgentRun, "log"> & { log?: string } }
  | { type: "work_item_upsert"; reason?: string; work_item: Omit<AgentWorkItem, "context"> & { context?: string } }
  | { type: "artifact_upsert"; reason?: string; artifact: Artifact }
  | { type: "channel_member_upsert"; reason?: string; member: ChannelMember }
  | { type: "channel_member_remove"; reason?: string; channel_id: string; agent_id: string };

type ConfirmRequest = {
  title: string;
  body: string;
  confirmLabel: string;
  onConfirm: () => Promise<void> | void;
};

type ActiveTab = "chat" | "tasks";
type MobileModal = "search" | "activity" | "saved";

type AppHistoryState = {
  __lantorUiHistory?: true;
  __lantorMobileUi?: true;
  index: number;
  activeChannelId: string | null;
  activeThreadId: string | null;
  activeTab: "chat" | "tasks";
  showThread: boolean;
  showMobileSidebar: boolean;
  selectedAgentId: string | null;
  activeModal: MobileModal | null;
};

type AppErrorBoundaryState = {
  error: Error | null;
  info: ErrorInfo | null;
};

function phaseForActivity(kind: string) {
  return ACTIVITY_PHASE_LABELS[kind] ?? "Active";
}

function activityOwnerKey(activity: AgentActivity) {
  return activity.agent_id ?? `handle:${activity.agent_handle || "unknown"}`;
}

function timestampSortValue(value: string) {
  const parsed = new Date(value).getTime();
  return Number.isFinite(parsed) ? parsed : 0;
}

function compareActivityFeedItems(left: ActivityFeedItem, right: ActivityFeedItem) {
  if (left.unread !== right.unread) return left.unread ? -1 : 1;
  return timestampSortValue(right.timestamp) - timestampSortValue(left.timestamp);
}

function limitActivitiesPerAgent(activities: AgentActivity[]) {
  const counts = new Map<string, number>();
  return [...activities]
    .sort((left, right) => timestampSortValue(right.created_at) - timestampSortValue(left.created_at))
    .filter((activity) => {
      const key = activityOwnerKey(activity);
      const count = counts.get(key) ?? 0;
      if (count >= ACTIVITY_HISTORY_LIMIT_PER_AGENT) return false;
      counts.set(key, count + 1);
      return true;
    });
}

function isTextInput(target: EventTarget | null) {
  if (!(target instanceof HTMLElement)) return false;
  return ["INPUT", "TEXTAREA", "SELECT"].includes(target.tagName) || target.isContentEditable;
}

function isActionControl(target: EventTarget | null) {
  if (!(target instanceof HTMLElement)) return false;
  return Boolean(target.closest("button, a, [role='button'], [role='tab']"));
}

function isMobileViewport() {
  return window.innerWidth <= MOBILE_BREAKPOINT;
}

function isCompactDesktopViewport() {
  return window.innerWidth > MOBILE_BREAKPOINT && window.innerWidth <= COMPACT_LAYOUT_BREAKPOINT;
}

function isAppHistoryState(value: unknown): value is AppHistoryState {
  if (!value || typeof value !== "object") return false;
  const state = value as Record<string, unknown>;
  return (state.__lantorUiHistory === true || state.__lantorMobileUi === true)
    && typeof state.index === "number"
    && (state.activeChannelId === undefined || state.activeChannelId === null || typeof state.activeChannelId === "string")
    && (state.activeThreadId === undefined || state.activeThreadId === null || typeof state.activeThreadId === "string")
    && (state.activeTab === undefined || state.activeTab === "chat" || state.activeTab === "tasks")
    && typeof state.showThread === "boolean"
    && typeof state.showMobileSidebar === "boolean"
    && (state.selectedAgentId === null || typeof state.selectedAgentId === "string")
    && (
      state.activeModal === null ||
      state.activeModal === "search" ||
      state.activeModal === "activity" ||
      state.activeModal === "saved"
    );
}

function appHistoryKey(state: AppHistoryState) {
  return [
    state.activeChannelId ?? "",
    state.activeThreadId ?? "",
    state.activeTab,
    state.showThread ? "thread" : "conversation",
    state.showMobileSidebar ? "sidebar" : "content",
    state.selectedAgentId ?? "",
    state.activeModal ?? "",
  ].join("|");
}

function errorMessage(err: unknown, fallback: string) {
  if (err instanceof Error && err.message) return err.message;
  if (typeof err === "string" && err.trim()) return err;
  return fallback;
}

function BootSplash({ appError, onRetry }: { appError: string | null; onRetry: () => void }) {
  const statuses = [
    "Waking agents...",
    "Restoring workspace...",
    "Syncing threads...",
  ];

  return (
    <div className="boot" aria-live="polite">
      <div className="boot-panel">
        <div className="boot-constellation" aria-hidden="true">
          <div className="boot-orbit">
            <span className="boot-link link-one" />
            <span className="boot-link link-two" />
            <span className="boot-link link-three" />
            <span className="boot-link link-four" />
          </div>
          <div className="boot-core">
            <span className="boot-core-mark">L</span>
          </div>
        </div>
        <div className="boot-copy">
          <strong>{APP_DISPLAY_NAME}</strong>
          <div className="boot-status" aria-label={statuses.join(" ")}>
            {statuses.map((status, index) => (
              <span key={status} style={{ "--status-index": index } as CSSProperties}>{status}</span>
            ))}
          </div>
        </div>
        {appError ? (
          <div className="boot-error" role="alert">
            <p>{appError}</p>
            <button type="button" onClick={onRetry}>
              Retry
            </button>
          </div>
        ) : null}
      </div>
    </div>
  );
}

class AppErrorBoundary extends Component<{ children: ReactNode }, AppErrorBoundaryState> {
  state: AppErrorBoundaryState = { error: null, info: null };

  static getDerivedStateFromError(error: Error): AppErrorBoundaryState {
    return { error, info: null };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error(`${APP_DISPLAY_NAME} UI crashed`, error, info);
    this.setState({ info });
  }

  render() {
    if (!this.state.error) return this.props.children;
    const details = [
      this.state.error.stack || this.state.error.message,
      this.state.info?.componentStack,
    ].filter(Boolean).join("\n\n");

    return (
      <main className="fatal-shell">
        <section className="fatal-card" role="alert">
          <p className="eyebrow">{APP_DISPLAY_NAME} UI crashed</p>
          <h1>Frontend render failed</h1>
          <p>
            The backend is still running. Reload the app to recover; the details below are kept
            visible so this does not become a blank window.
          </p>
          <div className="fatal-actions">
            <button type="button" onClick={() => window.location.reload()}>Reload {APP_DISPLAY_NAME}</button>
          </div>
          <pre>{details}</pre>
        </section>
      </main>
    );
  }
}

function maxRightPanelWidth(
  sidebarWidth: number,
  minPanelWidth: number,
  capToViewport = true,
  minConversationWidth = MIN_CONVERSATION_WIDTH,
) {
  if (isMobileViewport()) return minPanelWidth;
  if (isCompactDesktopViewport()) {
    const availableWidth = Math.max(minPanelWidth, window.innerWidth - MIN_COMPACT_SIDEBAR_VISIBLE_WIDTH);
    return capToViewport
      ? Math.max(minPanelWidth, Math.min(Math.floor(window.innerWidth * (2 / 3)), availableWidth))
      : availableWidth;
  }
  const contentWidth = Math.max(0, window.innerWidth - sidebarWidth);
  const preserveConversationMax = contentWidth - minConversationWidth;
  const viewportMax = Math.floor(window.innerWidth * (2 / 3));
  const maxWidth = capToViewport
    ? Math.min(viewportMax, preserveConversationMax)
    : preserveConversationMax;
  return Math.max(minPanelWidth, maxWidth);
}

function maxThreadPanelWidth(sidebarWidth = DEFAULT_SIDEBAR_WIDTH) {
  return maxRightPanelWidth(sidebarWidth, MIN_THREAD_PANEL_WIDTH, false);
}

function maxAgentDrawerWidth(sidebarWidth = DEFAULT_SIDEBAR_WIDTH) {
  return maxRightPanelWidth(
    sidebarWidth,
    MIN_AGENT_DRAWER_WIDTH,
    true,
    MIN_AGENT_DETAIL_CONVERSATION_WIDTH,
  );
}

function matchesSearchTime(value: string | null, range: SearchTimeRange) {
  if (range === "any" || !value) return true;
  const timestamp = new Date(value).getTime();
  if (Number.isNaN(timestamp)) return true;
  const now = Date.now();
  if (range === "today") {
    const start = new Date();
    start.setHours(0, 0, 0, 0);
    return timestamp >= start.getTime();
  }
  const days = range === "7d" ? 7 : 30;
  return now - timestamp <= days * 24 * 60 * 60 * 1000;
}

function searchScopeAllows(scope: SearchScope, kind: SearchScope) {
  return scope === "all" || scope === kind;
}

function clientId() {
  if (globalThis.crypto && typeof globalThis.crypto.randomUUID === "function") {
    return globalThis.crypto.randomUUID();
  }
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
}

function draftAttachmentFromFile(file: File): DraftAttachment {
  return {
    id: `${file.name}-${file.size}-${file.lastModified}-${clientId()}`,
    file,
    original_name: file.name,
    mime_type: file.type || "application/octet-stream",
    size_bytes: file.size,
  };
}

type ComposerDraftState = {
  text: string;
  attachments: DraftAttachment[];
};

type ChannelThreadMemory = Record<string, string | null>;

const EMPTY_COMPOSER_DRAFT: ComposerDraftState = {
  text: "",
  attachments: [],
};

function isEmptyComposerDraft(draft: ComposerDraftState) {
  return draft.text.length === 0 && draft.attachments.length === 0;
}

function updateComposerDraftRecord(
  current: Record<string, ComposerDraftState>,
  key: string | null | undefined,
  updater: (draft: ComposerDraftState) => ComposerDraftState,
) {
  if (!key) return current;
  const previous = current[key] ?? EMPTY_COMPOSER_DRAFT;
  const nextDraft = updater(previous);
  const next = { ...current };
  if (isEmptyComposerDraft(nextDraft)) {
    delete next[key];
  } else {
    next[key] = nextDraft;
  }
  return next;
}

function loadChannelThreadMemory(): ChannelThreadMemory {
  try {
    const raw = window.localStorage.getItem(CHANNEL_THREAD_MEMORY_STORAGE_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    return Object.fromEntries(
      Object.entries(parsed as Record<string, unknown>)
        .filter(([channelId, threadId]) => channelId && (threadId === null || typeof threadId === "string")),
    ) as ChannelThreadMemory;
  } catch {
    return {};
  }
}

function getStoredNumber(key: string, fallback: number) {
  const stored = window.localStorage.getItem(key);
  return stored ? Number(stored) : fallback;
}

function getStoredThemePreference(): ThemePreference {
  const stored = window.localStorage.getItem(THEME_PREFERENCE_STORAGE_KEY);
  return stored === "light" || stored === "dark" || stored === "auto" ? stored : "auto";
}

function getStoredChatTextSize(): ChatTextSize {
  const stored = window.localStorage.getItem(CHAT_TEXT_SIZE_STORAGE_KEY);
  return CHAT_TEXT_SIZE_OPTIONS.includes(stored as ChatTextSize) ? stored as ChatTextSize : "default";
}

function getStoredShowImageThumbnails() {
  return window.localStorage.getItem(SHOW_IMAGE_THUMBNAILS_STORAGE_KEY) !== "false";
}

async function attachmentUploads(attachments: DraftAttachment[]) {
  return Promise.all(attachments.map(async (attachment) => {
    const buffer = await attachment.file.arrayBuffer();
    return {
      originalName: attachment.original_name,
      mimeType: attachment.mime_type,
      bytes: Array.from(new Uint8Array(buffer)),
    };
  }));
}

function defaultAgentWorkspace(handle: string) {
  const normalized = handle.trim().replace(/^@/, "").replace(/[^A-Za-z0-9_-]/g, "-");
  return normalized ? `~/Library/Application Support/Lantor/agents/${normalized}` : "";
}

function newAgentDraft(): AgentForm {
  return {
    ...EMPTY_AGENT_FORM,
    avatar: randomDylanAvatarSpec("new-agent"),
  };
}

function normalizeAgentHandle(value: string) {
  const cleaned = value
    .trim()
    .replace(/^@/, "")
    .replace(/[^A-Za-z0-9_-]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 32);
  const withLeadingLetter = /^[A-Za-z]/.test(cleaned)
    ? cleaned
    : cleaned
      ? `Agent-${cleaned}`
      : "Agent";
  return withLeadingLetter.length === 1 ? `${withLeadingLetter}Agent` : withLeadingLetter;
}

function availableAgentHandle(preferred: string, agents: Agent[], currentAgentId?: string) {
  const base = normalizeAgentHandle(preferred);
  const existing = new Set(
    agents
      .filter((agent) => agent.id !== currentAgentId)
      .map((agent) => agent.handle.toLowerCase()),
  );
  if (!existing.has(base.toLowerCase())) return base;
  for (let suffix = 2; suffix < 1000; suffix += 1) {
    const suffixText = String(suffix);
    const next = `${base.slice(0, Math.max(1, 32 - suffixText.length))}${suffixText}`;
    if (!existing.has(next.toLowerCase())) return next;
  }
  return `${base.slice(0, 24)}${Date.now().toString().slice(-8)}`;
}

function numericMetadata(value: unknown) {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "string") {
    const parsed = Number.parseFloat(value);
    return Number.isFinite(parsed) ? parsed : null;
  }
  return null;
}

function percentile(values: number[], ratio: number) {
  if (values.length === 0) return null;
  const sorted = [...values].sort((left, right) => left - right);
  const index = Math.min(sorted.length - 1, Math.ceil(sorted.length * ratio) - 1);
  return sorted[index];
}

function messageMentionsOwner(message: Message) {
  const body = message.body.toLowerCase();
  return OWNER_MENTION_HANDLES.some((handle) => body.includes(handle.toLowerCase()));
}

function budgetMicrosFromForm(value: string) {
  const parsed = Number.parseFloat(value);
  if (!Number.isFinite(parsed) || parsed <= 0) return 0;
  return Math.round(parsed * 1_000_000);
}

function budgetUsdFromMicros(value: number) {
  return value > 0 ? (value / 1_000_000).toFixed(2) : "";
}

function buildAgentPerformance(activities: AgentActivity[], runs: AgentRun[]): AgentPerformance {
  const cutoff = Date.now() - 24 * 60 * 60 * 1000;
  const recent = activities.filter((activity) => {
    const timestamp = new Date(activity.created_at).getTime();
    return Number.isNaN(timestamp) || timestamp >= cutoff;
  });
  const recentRuns = runs.filter((run) => {
    const timestamp = new Date(run.started_at).getTime();
    return Number.isNaN(timestamp) || timestamp >= cutoff;
  });
  const firstTokenMs = recent
    .map((activity) => numericMetadata(activity.metadata.first_token_ms))
    .filter((value): value is number => value !== null);
  const finishedTurns = recent.filter((activity) =>
    activity.phase === "runtime" &&
    ["Completed", "Failed", "Stopped"].includes(activity.title || activity.summary));
  const turnDurations = finishedTurns
    .filter((activity) => activity.status === "success")
    .map((activity) => numericMetadata(activity.metadata.duration_ms))
    .filter((value): value is number => value !== null);
  const failedTurns = finishedTurns.filter((activity) => activity.status === "error").length;
  const completedTurns = finishedTurns.filter((activity) => activity.status === "success").length;
  const activeTurns = recent.filter((activity) =>
    activity.phase === "runtime" &&
    activity.status === "active" &&
    (activity.title === "Started working" || activity.summary === "Started working")).length;
  const turns = completedTurns + failedTurns + activeTurns;
  const inputTokens = recentRuns.reduce((total, run) => total + (run.input_tokens || 0), 0);
  const outputTokens = recentRuns.reduce((total, run) => total + (run.output_tokens || 0), 0);
  const costMicros = recentRuns.reduce((total, run) => total + (run.cost_micros || 0), 0);

  return {
    windowLabel: "Last 24h",
    turns,
    completedTurns,
    failedTurns,
    activeTurns,
    p50FirstTokenMs: percentile(firstTokenMs, 0.5),
    p95FirstTokenMs: percentile(firstTokenMs, 0.95),
    p50TurnMs: percentile(turnDurations, 0.5),
    p95TurnMs: percentile(turnDurations, 0.95),
    errorRate: completedTurns + failedTurns === 0 ? 0 : failedTurns / (completedTurns + failedTurns),
    inputTokens,
    outputTokens,
    costMicros,
  };
}

function App() {
  const [bootStartedAt] = useState(() => performance.now());
  const [bootReady, setBootReady] = useState(false);
  const startupSplashCompletedRef = useRef(false);
  const [data, setData] = useState<Bootstrap | null>(null);
  const [activeChannelId, setActiveChannelId] = useState<string>("");
  const [activeThreadId, setActiveThreadId] = useState<string | null>(null);
  const [channelThreadMemory, setChannelThreadMemory] = useState<ChannelThreadMemory>(() => loadChannelThreadMemory());
  const [activeTab, setActiveTab] = useState<ActiveTab>("chat");
  const [rootComposerDrafts, setRootComposerDrafts] = useState<Record<string, ComposerDraftState>>({});
  const [replyComposerDrafts, setReplyComposerDrafts] = useState<Record<string, ComposerDraftState>>({});
  const [taskTitleDrafts, setTaskTitleDrafts] = useState<Record<string, string>>({});
  const [searchQuery, setSearchQuery] = useState("");
  const [searchScope, setSearchScope] = useState<SearchScope>("all");
  const [searchTimeRange, setSearchTimeRange] = useState<SearchTimeRange>("any");
  const [newChannel, setNewChannel] = useState("");
  const [newChannelNameSubmitError, setNewChannelNameSubmitError] = useState<string | null>(null);
  const [newChannelAgentIds, setNewChannelAgentIds] = useState<Set<string>>(() => new Set());
  const [channelNameDraft, setChannelNameDraft] = useState("");
  const [channelDescriptionDraft, setChannelDescriptionDraft] = useState("");
  const [ownerProfileDraft, setOwnerProfileDraft] = useState<OwnerProfileForm>({
    displayName: DEFAULT_OWNER_DISPLAY_NAME,
    avatar: DEFAULT_OWNER_AVATAR,
    description: DEFAULT_OWNER_DESCRIPTION,
  });
  const [agentDraft, setAgentDraft] = useState<AgentForm>(() => newAgentDraft());
  const [editingAgentId, setEditingAgentId] = useState<string | null>(null);
  const [agentEdit, setAgentEdit] = useState<AgentForm>(EMPTY_AGENT_FORM);
  const [showThread, setShowThread] = useState(() => window.innerWidth > MOBILE_BREAKPOINT);
  const [showCreateChannelModal, setShowCreateChannelModal] = useState(false);
  const [showChannelSettingsModal, setShowChannelSettingsModal] = useState(false);
  const [showChannelAgentsModal, setShowChannelAgentsModal] = useState(false);
  const [showCreateAgentModal, setShowCreateAgentModal] = useState(false);
  const [returnToCreateChannelAfterAgent, setReturnToCreateChannelAfterAgent] = useState(false);
  const [createChannelSubmitting, setCreateChannelSubmitting] = useState(false);
  const createChannelSubmittingRef = useRef(false);
  const createChannelOpenedFromMobileHomeRef = useRef(false);
  const createAgentOpenedFromMobileHomeRef = useRef(false);
  const [showSearchModal, setShowSearchModal] = useState(false);
  const [showActivityFeedModal, setShowActivityFeedModal] = useState(false);
  const [showSavedModal, setShowSavedModal] = useState(false);
  const [showOwnerProfileModal, setShowOwnerProfileModal] = useState(false);
  const [showSettingsModal, setShowSettingsModal] = useState(false);
  const [themePreference, setThemePreference] = useState<ThemePreference>(() => getStoredThemePreference());
  const [chatTextSize, setChatTextSize] = useState<ChatTextSize>(() => getStoredChatTextSize());
  const [showImageThumbnails, setShowImageThumbnails] = useState(() => getStoredShowImageThumbnails());
  const [showMobileSidebar, setShowMobileSidebar] = useState(() => isMobileViewport());
  const [mobileSidebarFocus, setMobileSidebarFocus] = useState<"home" | "dms">("home");
  const [mobileSidebarDragPx, setMobileSidebarDragPx] = useState(0);
  const [mobileDragSurface, setMobileDragSurface] = useState<"sidebar" | "panel" | null>(null);
  const [mobileComposerFocused, setMobileComposerFocused] = useState(false);
  const [selectedAgentId, setSelectedAgentId] = useState<string | null>(null);
  const [focusedMessageId, setFocusedMessageId] = useState<string | null>(null);
  const [appError, setAppError] = useState<string | null>(null);
  const [confirmRequest, setConfirmRequest] = useState<ConfirmRequest | null>(null);
  const [runtimeChecks, setRuntimeChecks] = useState<Record<string, RuntimeCheck>>({});
  const [threadPanelWidth, setThreadPanelWidth] = useState(() => {
    const value = getStoredNumber(
      THREAD_PANEL_WIDTH_STORAGE_KEY,
      DEFAULT_THREAD_PANEL_WIDTH,
    );
    const maxWidth = maxThreadPanelWidth(DEFAULT_SIDEBAR_WIDTH);
    if (!Number.isFinite(value)) return Math.min(maxWidth, DEFAULT_THREAD_PANEL_WIDTH);
    const preferredWidth = value <= LEGACY_DEFAULT_THREAD_PANEL_WIDTH
      ? DEFAULT_THREAD_PANEL_WIDTH
      : value;
    return Math.min(maxWidth, Math.max(MIN_THREAD_PANEL_WIDTH, preferredWidth));
  });
  const [agentDrawerWidth, setAgentDrawerWidth] = useState(() => {
    const value = getStoredNumber(
      AGENT_DRAWER_WIDTH_STORAGE_KEY,
      DEFAULT_AGENT_DRAWER_WIDTH,
    );
    const maxWidth = maxAgentDrawerWidth(DEFAULT_SIDEBAR_WIDTH);
    if (!Number.isFinite(value)) return Math.min(maxWidth, DEFAULT_AGENT_DRAWER_WIDTH);
    const preferredWidth = value <= LEGACY_DEFAULT_AGENT_DRAWER_WIDTH
      ? DEFAULT_AGENT_DRAWER_WIDTH
      : value;
    return Math.min(maxWidth, Math.max(MIN_AGENT_DRAWER_WIDTH, preferredWidth));
  });
  const [sidebarWidth, setSidebarWidth] = useState(() => {
    const value = getStoredNumber(
      SIDEBAR_WIDTH_STORAGE_KEY,
      DEFAULT_SIDEBAR_WIDTH,
    );
    return Number.isFinite(value)
      ? Math.min(MAX_SIDEBAR_WIDTH, Math.max(MIN_SIDEBAR_WIDTH, value))
      : DEFAULT_SIDEBAR_WIDTH;
  });
  const rootComposerDraft = activeChannelId ? rootComposerDrafts[activeChannelId] ?? EMPTY_COMPOSER_DRAFT : EMPTY_COMPOSER_DRAFT;
  const replyComposerDraft = activeThreadId ? replyComposerDrafts[activeThreadId] ?? EMPTY_COMPOSER_DRAFT : EMPTY_COMPOSER_DRAFT;
  const draft = rootComposerDraft.text;
  const draftAttachments = rootComposerDraft.attachments;
  const replyDraft = replyComposerDraft.text;
  const replyAttachments = replyComposerDraft.attachments;

  useEffect(() => {
    if (!focusedMessageId) return;
    const timer = window.setTimeout(() => setFocusedMessageId(null), 2600);
    return () => window.clearTimeout(timer);
  }, [focusedMessageId]);

  useEffect(() => {
    function clampRightPanels() {
      setThreadPanelWidth((current) => Math.min(maxThreadPanelWidth(sidebarWidth), current));
      setAgentDrawerWidth((current) => Math.min(maxAgentDrawerWidth(sidebarWidth), current));
    }

    clampRightPanels();
    window.addEventListener("resize", clampRightPanels);
    return () => window.removeEventListener("resize", clampRightPanels);
  }, [sidebarWidth]);

  const [channelAlertIds, setChannelAlertIds] = useState<Set<string>>(() => new Set());
  const [threadUnreadCounts, setThreadUnreadCounts] = useState<Record<string, number>>({});
  const [dismissedActivityFeedItems, setDismissedActivityFeedItems] = useState<Record<string, string>>({});
  const [readActivityFeedItems, setReadActivityFeedItems] = useState<Record<string, string>>({});
  const [locallyUnfollowedThreadIds, setLocallyUnfollowedThreadIds] = useState<Set<string>>(() => new Set());
  const knownMessageIdsRef = useRef<Set<string> | null>(null);
  const refreshTimerRef = useRef<number | null>(null);
  const refreshInFlightRef = useRef(false);
  const refreshQueuedRef = useRef(false);
  const refreshInvalidationRef = useRef(0);
  const messageDeltaBufferRef = useRef<Map<string, { append: string; deliveryState: Message["delivery_state"] }>>(new Map());
  const optimisticMessagesRef = useRef<Map<string, Message>>(new Map());
  const optimisticAttachmentUrlsRef = useRef<Map<string, string[]>>(new Map());
  // Per-message latest-intent sequence for save/unsave, so a late-failing
  // request cannot roll back over a newer toggle for the same message.
  const savedToggleSeqRef = useRef<Map<string, number>>(new Map());
  const pendingSavedToggleOverridesRef = useRef<Map<string, { saved: boolean; entry?: SavedMessage; seq: number }>>(new Map());
  const savedToggleRequestChainsRef = useRef<Map<string, Promise<void>>>(new Map());
  const messageDeltaFlushTimerRef = useRef<number | null>(null);
  // Ephemeral event buffer (issue #82): coalesces high-frequency progress
  // upserts keyed by (kind, id) — latest-wins — and flushes in a single
  // setData on the next animation frame, with an 80 ms fallback for
  // background tabs where rAF is throttled. Semantic events bypass the
  // buffer and apply immediately (see handleBackendEvent).
  const ephemeralActivityBufferRef = useRef<Map<string, AgentActivity>>(new Map());
  const ephemeralRunBufferRef = useRef<Map<string, Omit<AgentRun, "log"> & { log?: string }>>(new Map());
  const ephemeralFlushScheduledRef = useRef(false);
  const ephemeralFlushRafRef = useRef<number | null>(null);
  const ephemeralFlushTimerRef = useRef<number | null>(null);
  const appHistoryReadyRef = useRef(false);
  const appHistoryIndexRef = useRef(0);
  const appHistoryMaxIndexRef = useRef(0);
  const restoringAppHistoryRef = useRef(false);
  const replaceNextAppHistoryEntryRef = useRef(false);
  const lastAppHistoryKeyRef = useRef<string | null>(null);
  const searchResultThreadIdRef = useRef<string | null>(null);
  const searchResultAgentIdRef = useRef<string | null>(null);
  const [appHistoryIndex, setAppHistoryIndex] = useState(0);
  const [appHistoryMaxIndex, setAppHistoryMaxIndex] = useState(0);
  const activeMobileModal: MobileModal | null = showSearchModal
    ? "search"
    : showActivityFeedModal
      ? "activity"
      : showSavedModal
        ? "saved"
        : null;

  useEffect(() => {
    return () => {
      optimisticAttachmentUrlsRef.current.forEach((objectUrls) => {
        objectUrls.forEach((url) => URL.revokeObjectURL(url));
      });
      optimisticAttachmentUrlsRef.current.clear();
      optimisticMessagesRef.current.clear();
    };
  }, []);

  function setAppHistoryPosition(index: number, maxIndex?: number) {
    const nextMaxIndex = maxIndex ?? Math.max(appHistoryMaxIndexRef.current, index);
    appHistoryIndexRef.current = index;
    appHistoryMaxIndexRef.current = nextMaxIndex;
    setAppHistoryIndex(index);
    setAppHistoryMaxIndex(nextMaxIndex);
  }

  function buildAppHistoryState(index = appHistoryIndexRef.current): AppHistoryState {
    return {
      __lantorUiHistory: true,
      __lantorMobileUi: true,
      index,
      activeChannelId: activeChannelId || null,
      activeThreadId,
      activeTab,
      showThread,
      showMobileSidebar,
      selectedAgentId,
      activeModal: activeMobileModal,
    };
  }

  async function refreshRuntimeChecks() {
    if (!isTauriRuntime()) {
      setRuntimeChecks({});
      return;
    }
    const entries = await Promise.all(
      Object.keys(RUNTIME_PRESETS).map(async (runtime) => {
        const check = await apiInvoke<RuntimeCheck>("check_runtime", { runtime });
        return [runtime, check] as const;
      }),
    );
    setRuntimeChecks(Object.fromEntries(entries));
  }

  function sortedMessages(messages: Message[]) {
    return [...messages].sort((left, right) => new Date(left.created_at).getTime() - new Date(right.created_at).getTime());
  }

  function normalizeBootstrap(next: Bootstrap): Bootstrap {
    return {
      ...next,
      messages: sortedMessages(next.messages),
      agent_activities: limitActivitiesPerAgent(next.agent_activities),
    };
  }

  function invalidatePendingRefreshResult() {
    refreshInvalidationRef.current += 1;
  }

  async function refresh(includeOptimistic = true, preferredActiveChannelId?: string) {
    const refreshInvalidation = refreshInvalidationRef.current;
    const next = normalizeBootstrap(await apiInvoke<Bootstrap>("bootstrap"));
    const refreshed = withPendingSavedToggles(includeOptimistic ? withOptimisticMessages(next) : next);
    setData((current) => {
      if (!current || refreshInvalidation === refreshInvalidationRef.current) return refreshed;
      const refreshedMessageIds = new Set(refreshed.messages.map((message) => message.id));
      const currentOnlyMessages = current.messages.filter((message) => !refreshedMessageIds.has(message.id));
      if (currentOnlyMessages.length === 0) return refreshed;
      return {
        ...refreshed,
        messages: sortedMessages([...refreshed.messages, ...currentOnlyMessages]),
      };
    });
    setActiveChannelId((prev) => {
      if (preferredActiveChannelId && next.channels.some((item) => item.id === preferredActiveChannelId)) {
        return preferredActiveChannelId;
      }
      if (next.channels.some((item) => item.id === prev)) return prev;
      return next.channels[0]?.id || "";
    });
    setActiveThreadId((prev) => {
      const rootIds = new Set(next.messages.filter((item) => !item.thread_root_id).map((item) => item.id));
      if (prev && rootIds.has(prev)) return prev;
      return null;
    });
  }

  function refreshWithError(fallback: string) {
    if (refreshInFlightRef.current) {
      refreshQueuedRef.current = true;
      return;
    }
    refreshInFlightRef.current = true;
    refresh()
      .catch((err) => {
        setAppError(errorMessage(err, fallback));
        console.error(err);
      })
      .finally(() => {
        refreshInFlightRef.current = false;
        if (refreshQueuedRef.current) {
          refreshQueuedRef.current = false;
          requestRefresh(fallback);
        }
      });
  }

  function requestRefresh(fallback = `Failed to refresh ${APP_DISPLAY_NAME} state`) {
    invalidatePendingRefreshResult();
    if (refreshTimerRef.current !== null) return;
    refreshTimerRef.current = window.setTimeout(() => {
      refreshTimerRef.current = null;
      refreshWithError(fallback);
    }, UI_REFRESH_DEBOUNCE_MS);
  }

  function applyMessageUpsert(message: Message) {
    messageDeltaBufferRef.current.delete(message.id);
    invalidatePendingRefreshResult();
    setData((current) => {
      if (!current) {
        requestRefresh(`Failed to refresh ${APP_DISPLAY_NAME} state after message update`);
        return current;
      }
      const existingIndex = current.messages.findIndex((item) => item.id === message.id);
      const messages = existingIndex >= 0
        ? current.messages.map((item) => item.id === message.id ? message : item)
        : [...current.messages, message];
      return { ...current, messages: sortedMessages(messages) };
    });
  }

  function withOptimisticMessages(next: Bootstrap): Bootstrap {
    if (optimisticMessagesRef.current.size === 0) return next;
    const existingIds = new Set(next.messages.map((message) => message.id));
    const optimisticMessages = Array.from(optimisticMessagesRef.current.values())
      .filter((message) => !existingIds.has(message.id));
    if (optimisticMessages.length === 0) return next;
    return { ...next, messages: sortedMessages([...next.messages, ...optimisticMessages]) };
  }

  function savedMessagesWithState(savedMessages: SavedMessage[], messageId: string, saved: boolean, savedEntry?: SavedMessage) {
    const withoutMessage = savedMessages.filter((item) => item.message_id !== messageId);
    if (!saved) return withoutMessage.length === savedMessages.length ? savedMessages : withoutMessage;
    const existing = savedMessages.find((item) => item.message_id === messageId) ?? null;
    if (!savedEntry && existing) return savedMessages;
    if (!savedEntry) return savedMessages;
    return [savedEntry, ...withoutMessage];
  }

  function withPendingSavedToggles(next: Bootstrap): Bootstrap {
    if (pendingSavedToggleOverridesRef.current.size === 0) return next;
    let savedMessages = next.saved_messages;
    for (const [messageId, override] of pendingSavedToggleOverridesRef.current) {
      savedMessages = savedMessagesWithState(savedMessages, messageId, override.saved, override.entry);
    }
    return savedMessages === next.saved_messages ? next : { ...next, saved_messages: savedMessages };
  }

  function flushMessageDeltas() {
    if (messageDeltaFlushTimerRef.current !== null) {
      window.clearTimeout(messageDeltaFlushTimerRef.current);
      messageDeltaFlushTimerRef.current = null;
    }
    if (messageDeltaBufferRef.current.size === 0) return;
    invalidatePendingRefreshResult();
    const deltas = messageDeltaBufferRef.current;
    messageDeltaBufferRef.current = new Map();
    setData((current) => {
      if (!current) {
        requestRefresh(`Failed to refresh ${APP_DISPLAY_NAME} state after message delta`);
        return current;
      }
      let missing = false;
      let changed = false;
      const messages = current.messages.map((item) => {
        const delta = deltas.get(item.id);
        if (!delta) return item;
        changed = true;
        return { ...item, body: `${item.body}${delta.append}`, delivery_state: delta.deliveryState };
      });
      for (const messageId of deltas.keys()) {
        if (!current.messages.some((item) => item.id === messageId)) {
          missing = true;
          break;
        }
      }
      if (missing) {
        requestRefresh(`Failed to refresh ${APP_DISPLAY_NAME} state after message delta`);
      }
      if (!changed) return current;
      return { ...current, messages };
    });
  }

  function queueMessageDelta(messageId: string, append: string, deliveryState: Message["delivery_state"]) {
    const existing = messageDeltaBufferRef.current.get(messageId);
    messageDeltaBufferRef.current.set(messageId, {
      append: `${existing?.append ?? ""}${append}`,
      deliveryState,
    });
    if (messageDeltaFlushTimerRef.current !== null) return;
    messageDeltaFlushTimerRef.current = window.setTimeout(() => {
      flushMessageDeltas();
    }, 50);
  }

  function applyMessageDelete(messageId: string) {
    messageDeltaBufferRef.current.delete(messageId);
    invalidatePendingRefreshResult();
    setData((current) => {
      if (!current) {
        requestRefresh(`Failed to refresh ${APP_DISPLAY_NAME} state after message deletion`);
        return current;
      }
      return { ...current, messages: current.messages.filter((item) => item.id !== messageId) };
    });
  }

  function applyActivityUpsert(activity: AgentActivity) {
    setData((current) => {
      if (!current) {
        requestRefresh(`Failed to refresh ${APP_DISPLAY_NAME} state after activity update`);
        return current;
      }
      const existingIndex = current.agent_activities.findIndex((item) => item.id === activity.id);
      const agent_activities = existingIndex >= 0
        ? current.agent_activities.map((item) => item.id === activity.id ? activity : item)
        : [activity, ...current.agent_activities];
      return { ...current, agent_activities: limitActivitiesPerAgent(agent_activities) };
    });
  }

  function applyAgentRunUpsert(patch: Omit<AgentRun, "log"> & { log?: string }) {
    setData((current) => {
      if (!current) {
        requestRefresh(`Failed to refresh ${APP_DISPLAY_NAME} state after run update`);
        return current;
      }
      const existing = current.agent_runs.find((item) => item.id === patch.id);
      const run: AgentRun = {
        ...patch,
        log: patch.log ?? existing?.log ?? "",
      };
      const agent_runs = existing
        ? current.agent_runs.map((item) => item.id === patch.id ? { ...item, ...run } : item)
        : [run, ...current.agent_runs];
      agent_runs.sort((left, right) => new Date(right.started_at).getTime() - new Date(left.started_at).getTime());
      return { ...current, agent_runs: agent_runs.slice(0, 30) };
    });
  }

  function cancelEphemeralFlushTimers() {
    if (ephemeralFlushRafRef.current !== null) {
      cancelAnimationFrame(ephemeralFlushRafRef.current);
      ephemeralFlushRafRef.current = null;
    }
    if (ephemeralFlushTimerRef.current !== null) {
      window.clearTimeout(ephemeralFlushTimerRef.current);
      ephemeralFlushTimerRef.current = null;
    }
    ephemeralFlushScheduledRef.current = false;
  }

  function flushEphemeralBuffer() {
    cancelEphemeralFlushTimers();
    const activityBuffer = ephemeralActivityBufferRef.current;
    const runBuffer = ephemeralRunBufferRef.current;
    if (activityBuffer.size === 0 && runBuffer.size === 0) return;
    const activities = Array.from(activityBuffer.values());
    const runs = Array.from(runBuffer.values());
    ephemeralActivityBufferRef.current = new Map();
    ephemeralRunBufferRef.current = new Map();
    setData((current) => {
      if (!current) {
        requestRefresh(`Failed to refresh ${APP_DISPLAY_NAME} state after buffered updates`);
        return current;
      }
      let nextActivities = current.agent_activities;
      if (activities.length > 0) {
        const byId = new Map(nextActivities.map((item) => [item.id, item] as const));
        for (const activity of activities) byId.set(activity.id, activity);
        nextActivities = limitActivitiesPerAgent(Array.from(byId.values()));
      }
      let nextRuns = current.agent_runs;
      if (runs.length > 0) {
        const byId = new Map(nextRuns.map((item) => [item.id, item] as const));
        for (const patch of runs) {
          const existing = byId.get(patch.id);
          const run: AgentRun = {
            ...patch,
            log: patch.log ?? existing?.log ?? "",
          };
          byId.set(patch.id, existing ? { ...existing, ...run } : run);
        }
        nextRuns = Array.from(byId.values())
          .sort((left, right) => new Date(right.started_at).getTime() - new Date(left.started_at).getTime())
          .slice(0, 30);
      }
      if (nextActivities === current.agent_activities && nextRuns === current.agent_runs) {
        return current;
      }
      return { ...current, agent_activities: nextActivities, agent_runs: nextRuns };
    });
  }

  function scheduleEphemeralFlush() {
    if (ephemeralFlushScheduledRef.current) return;
    ephemeralFlushScheduledRef.current = true;
    ephemeralFlushRafRef.current = window.requestAnimationFrame(() => {
      ephemeralFlushRafRef.current = null;
      flushEphemeralBuffer();
    });
    // EPHEMERAL_FLUSH_FALLBACK_MS is the upper bound from issue #82 — also
    // covers background tabs where rAF is throttled / paused.
    ephemeralFlushTimerRef.current = window.setTimeout(() => {
      flushEphemeralBuffer();
    }, EPHEMERAL_FLUSH_FALLBACK_MS);
  }

  function bufferActivityEphemeral(activity: AgentActivity) {
    ephemeralActivityBufferRef.current.set(activity.id, activity);
    scheduleEphemeralFlush();
  }

  function bufferRunEphemeral(patch: Omit<AgentRun, "log"> & { log?: string }) {
    ephemeralRunBufferRef.current.set(patch.id, patch);
    scheduleEphemeralFlush();
  }

  function dropPendingRunEphemeral(runId: string) {
    ephemeralRunBufferRef.current.delete(runId);
  }

  function applyWorkItemUpsert(patch: Omit<AgentWorkItem, "context"> & { context?: string }) {
    setData((current) => {
      if (!current) {
        requestRefresh(`Failed to refresh ${APP_DISPLAY_NAME} state after agent request update`);
        return current;
      }
      const existing = current.agent_work_items.find((item) => item.id === patch.id);
      const workItem: AgentWorkItem = {
        ...patch,
        context: patch.context ?? existing?.context ?? "",
        source_kind: patch.source_kind ?? existing?.source_kind ?? "manual",
      };
      const agent_work_items = existing
        ? current.agent_work_items.map((item) => item.id === patch.id ? { ...item, ...workItem } : item)
        : [workItem, ...current.agent_work_items];
      agent_work_items.sort((left, right) => new Date(right.created_at).getTime() - new Date(left.created_at).getTime());
      return { ...current, agent_work_items: agent_work_items.slice(0, 80) };
    });
  }

  function applyArtifactUpsert(artifact: Artifact) {
    if (!artifact || typeof artifact.id !== "string" || typeof artifact.message_id !== "string") {
      requestRefresh(`Failed to refresh ${APP_DISPLAY_NAME} state after artifact update`);
      return;
    }
    setData((current) => {
      if (!current) {
        requestRefresh(`Failed to refresh ${APP_DISPLAY_NAME} state after artifact update`);
        return current;
      }
      const currentArtifacts = Array.isArray(current.artifacts) ? current.artifacts : [];
      const existingIndex = currentArtifacts.findIndex((item) => item.id === artifact.id);
      const artifacts = existingIndex >= 0
        ? currentArtifacts.map((item) => item.id === artifact.id ? artifact : item)
        : [...currentArtifacts, artifact];
      const messages = current.messages.map((message) => {
        if (message.id !== artifact.message_id) return message;
        const currentMessageArtifacts = Array.isArray(message.artifacts) ? message.artifacts : [];
        const existingArtifactIndex = currentMessageArtifacts.findIndex((item) => item.id === artifact.id);
        const messageArtifacts = existingArtifactIndex >= 0
          ? currentMessageArtifacts.map((item) => item.id === artifact.id ? artifact : item)
          : [...currentMessageArtifacts, artifact];
        return { ...message, artifacts: messageArtifacts };
      });
      return { ...current, artifacts, messages };
    });
  }

  function applyChannelMemberUpsert(member: ChannelMember) {
    if (
      !member
      || typeof member.channel_id !== "string"
      || typeof member.agent_id !== "string"
    ) {
      requestRefresh(`Failed to refresh ${APP_DISPLAY_NAME} state after membership update`);
      return;
    }
    setData((current) => {
      if (!current) {
        requestRefresh(`Failed to refresh ${APP_DISPLAY_NAME} state after membership update`);
        return current;
      }
      const filtered = current.channel_members.filter(
        (entry) => !(entry.channel_id === member.channel_id && entry.agent_id === member.agent_id),
      );
      return { ...current, channel_members: [...filtered, member] };
    });
  }

  function applyChannelMemberRemove(channelId: string, agentId: string) {
    if (typeof channelId !== "string" || typeof agentId !== "string") {
      requestRefresh(`Failed to refresh ${APP_DISPLAY_NAME} state after membership update`);
      return;
    }
    setData((current) => {
      if (!current) {
        requestRefresh(`Failed to refresh ${APP_DISPLAY_NAME} state after membership update`);
        return current;
      }
      const filtered = current.channel_members.filter(
        (entry) => !(entry.channel_id === channelId && entry.agent_id === agentId),
      );
      if (filtered.length === current.channel_members.length) return current;
      return { ...current, channel_members: filtered };
    });
  }

  async function openArtifact(artifact: Artifact) {
    try {
      const fullArtifact = await apiInvoke<Artifact>("artifact_read", { artifactId: artifact.id });
      const blob = new Blob([fullArtifact.content], { type: "text/plain;charset=utf-8" });
      const url = URL.createObjectURL(blob);
      window.open(url, "_blank", "noopener,noreferrer");
      window.setTimeout(() => URL.revokeObjectURL(url), 30_000);
    } catch (err) {
      setAppError(errorMessage(err, "Failed to open artifact"));
    }
  }

  function handleBackendEvent(payload: unknown) {
    try {
      if (typeof payload !== "string") {
        requestRefresh(`Failed to refresh ${APP_DISPLAY_NAME} state after backend update`);
        return;
      }
      const parsed = JSON.parse(payload) as UiBackendEvent;
      if (parsed.type === "batch") {
        for (const eventPayload of parsed.events) {
          handleBackendEvent(eventPayload);
        }
        return;
      }
      if (parsed.type === "message_upsert") {
        // Semantic message events (final body, owner/system messages, errors)
        // bypass the ephemeral buffer; they are decisive and inexpensive.
        // Streaming placeholder rewrites are batched on the backend through
        // the existing message_delta path, not here, so re-batching them
        // would race with deltas on the same message id.
        applyMessageUpsert(parsed.message);
        return;
      }
      if (parsed.type === "message_delta") {
        // Already coalesced by the dedicated message-delta buffer (50 ms).
        queueMessageDelta(parsed.message_id, parsed.append, parsed.delivery_state);
        return;
      }
      if (parsed.type === "message_delete") {
        applyMessageDelete(parsed.message_id);
        return;
      }
      if (parsed.type === "activity_upsert") {
        // Activities are inherently transient progress signals. Coalesce
        // by activity.id (latest wins) and flush once per animation frame.
        bufferActivityEphemeral(parsed.activity);
        return;
      }
      if (parsed.type === "agent_run_upsert") {
        // Run terminal transitions MUST land immediately so the UI shows
        // completion / failure without waiting for the buffer. Drop any
        // stale pending ephemeral for the same run id first to avoid the
        // buffered partial overwriting the terminal state.
        if (RUN_TERMINAL_STATUSES.has(parsed.run.status)) {
          dropPendingRunEphemeral(parsed.run.id);
          applyAgentRunUpsert(parsed.run);
        } else {
          bufferRunEphemeral(parsed.run);
        }
        return;
      }
      if (parsed.type === "work_item_upsert") {
        // Work-item / task state transitions are semantic; bypass buffer.
        applyWorkItemUpsert(parsed.work_item);
        return;
      }
      if (parsed.type === "artifact_upsert") {
        // Artifact upserts are semantic; bypass buffer.
        applyArtifactUpsert(parsed.artifact);
        return;
      }
      if (parsed.type === "channel_member_upsert") {
        // Targeted membership patch. Avoids a full bootstrap refresh on
        // every Add/Remove tap so mobile feels snappy and other UI
        // surfaces (e.g. agent detail drawer) see the change immediately.
        applyChannelMemberUpsert(parsed.member);
        return;
      }
      if (parsed.type === "channel_member_remove") {
        applyChannelMemberRemove(parsed.channel_id, parsed.agent_id);
        return;
      }
      requestRefresh(`Failed to refresh ${APP_DISPLAY_NAME} state after backend update`);
    } catch (err) {
      setAppError(errorMessage(err, `Failed to apply ${APP_DISPLAY_NAME} backend update`));
      console.error(`Failed to apply ${APP_DISPLAY_NAME} backend update`, err, payload);
      requestRefresh(`Failed to refresh ${APP_DISPLAY_NAME} state after backend update`);
    }
  }

  async function mutate<T = unknown>(command: string, args: Record<string, unknown> = {}): Promise<T> {
    try {
      const result = await apiInvoke<T>(command, args);
      await refresh();
      return result;
    } catch (err) {
      const message = errorMessage(err, `${command} failed`);
      setAppError(message);
      console.error(err);
      throw err;
    }
  }

  useEffect(() => {
    refresh().catch((err) => {
      setAppError(errorMessage(err, `Failed to load ${APP_DISPLAY_NAME} state`));
      console.error(err);
    });
    refreshRuntimeChecks().catch((err) => {
      setAppError(errorMessage(err, "Failed to check local runtimes"));
      console.error(err);
    });
  }, []);

  useEffect(() => {
    if ((!data && !appError) || bootReady) return;
    const elapsed = performance.now() - bootStartedAt;
    const delay = Math.max(0, MIN_BOOT_SPLASH_MS - elapsed);
    const timer = window.setTimeout(() => setBootReady(true), delay);
    return () => window.clearTimeout(timer);
  }, [appError, bootReady, bootStartedAt, data]);

  useEffect(() => {
    if (!bootReady || (!data && !appError) || startupSplashCompletedRef.current) return;
    startupSplashCompletedRef.current = true;
    completeStartupSplash().catch((err) => {
      console.error("Failed to complete startup splash", err);
    });
  }, [appError, bootReady, data]);

  useEffect(() => {
    if (!data || showOwnerProfileModal) return;
    setOwnerProfileDraft(ownerProfileToForm(data.owner_profile));
  }, [data?.owner_profile, showOwnerProfileModal]);

  useEffect(() => {
    function isFileDrag(event: DragEvent) {
      return Array.from(event.dataTransfer?.types ?? []).includes("Files");
    }

    function preventFileNavigation(event: DragEvent) {
      if (!isFileDrag(event)) return;
      event.preventDefault();
    }

    window.addEventListener("dragover", preventFileNavigation);
    window.addEventListener("drop", preventFileNavigation);
    return () => {
      window.removeEventListener("dragover", preventFileNavigation);
      window.removeEventListener("drop", preventFileNavigation);
    };
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let disposed = false;

    function connect() {
      if (disposed || unlisten) return;
      subscribeBackendEvents(handleBackendEvent)
        .then((handler) => {
          if (disposed) {
            handler();
            return;
          }
          unlisten = handler;
        })
        .catch((err) => {
          if (disposed) return;
          setAppError(errorMessage(err, `Failed to subscribe to ${APP_DISPLAY_NAME} updates`));
          console.error(err);
        });
    }

    function disconnect() {
      unlisten?.();
      unlisten = null;
    }

    function onPageHide() {
      disconnect();
    }

    function onPageShow() {
      connect();
      requestRefresh(`Failed to refresh ${APP_DISPLAY_NAME} state after page restore`);
    }

    connect();
    window.addEventListener("pagehide", onPageHide);
    window.addEventListener("pageshow", onPageShow);
    return () => {
      disposed = true;
      window.removeEventListener("pagehide", onPageHide);
      window.removeEventListener("pageshow", onPageShow);
      if (refreshTimerRef.current !== null) {
        window.clearTimeout(refreshTimerRef.current);
      }
      if (messageDeltaFlushTimerRef.current !== null) {
        window.clearTimeout(messageDeltaFlushTimerRef.current);
      }
      cancelEphemeralFlushTimers();
      disconnect();
    };
  }, []);

  useEffect(() => {
    const timer = window.setInterval(() => {
      if (document.visibilityState !== "visible") return;
      requestRefresh(`Failed to refresh ${APP_DISPLAY_NAME} state`);
    }, 5000);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    if (!data) return;
    if (!knownMessageIdsRef.current) {
      knownMessageIdsRef.current = new Set(data.messages.filter((message) => !isProgressOnlyMessage(message)).map((message) => message.id));
      return;
    }

    const known = knownMessageIdsRef.current;
    const newMessages = data.messages.filter((message) =>
      message.sender_role !== "owner" && !isProgressOnlyMessage(message) && !known.has(message.id)
    );
    if (newMessages.length === 0) return;
    newMessages.forEach((message) => known.add(message.id));

    setChannelAlertIds((current) => {
      let next: Set<string> | null = null;
      for (const message of newMessages) {
        if (message.channel_id === activeChannelId) continue;
        next ??= new Set(current);
        next.add(message.channel_id);
      }
      return next ?? current;
    });

    setThreadUnreadCounts((current) => {
      let next: Record<string, number> | null = null;
      for (const message of newMessages) {
        if (!message.thread_root_id || message.thread_root_id === activeThreadId) continue;
        next ??= { ...current };
        next[message.thread_root_id] = (next[message.thread_root_id] ?? 0) + 1;
      }
      return next ?? current;
    });
  }, [activeChannelId, activeThreadId, data]);

  useEffect(() => {
    if (!appError) return;
    const timer = window.setTimeout(() => setAppError(null), 6500);
    return () => window.clearTimeout(timer);
  }, [appError]);

  useEffect(() => {
    if (!activeChannelId) return;
    setChannelAlertIds((current) => {
      if (!current.has(activeChannelId)) return current;
      const next = new Set(current);
      next.delete(activeChannelId);
      return next;
    });
  }, [activeChannelId]);

  useEffect(() => {
    if (!activeThreadId) return;
    setThreadUnreadCounts((current) => {
      if (!current[activeThreadId]) return current;
      const next = { ...current };
      delete next[activeThreadId];
      return next;
    });
  }, [activeThreadId]);

  useEffect(() => {
    window.localStorage.setItem(THREAD_PANEL_WIDTH_STORAGE_KEY, String(threadPanelWidth));
  }, [threadPanelWidth]);

  useEffect(() => {
    window.localStorage.setItem(AGENT_DRAWER_WIDTH_STORAGE_KEY, String(agentDrawerWidth));
  }, [agentDrawerWidth]);

  useEffect(() => {
    window.localStorage.setItem(SIDEBAR_WIDTH_STORAGE_KEY, String(sidebarWidth));
  }, [sidebarWidth]);

  useEffect(() => {
    window.localStorage.setItem(CHANNEL_THREAD_MEMORY_STORAGE_KEY, JSON.stringify(channelThreadMemory));
  }, [channelThreadMemory]);

  useEffect(() => {
    window.localStorage.setItem(THEME_PREFERENCE_STORAGE_KEY, themePreference);
    const mediaQuery = window.matchMedia("(prefers-color-scheme: dark)");
    const applyTheme = () => {
      const resolvedTheme = themePreference === "auto"
        ? mediaQuery.matches ? "dark" : "light"
        : themePreference;
      document.documentElement.dataset.theme = resolvedTheme;
      document.documentElement.style.colorScheme = resolvedTheme;
    };
    applyTheme();
    if (themePreference !== "auto") return;
    mediaQuery.addEventListener("change", applyTheme);
    return () => mediaQuery.removeEventListener("change", applyTheme);
  }, [themePreference]);

  useEffect(() => {
    window.localStorage.setItem(CHAT_TEXT_SIZE_STORAGE_KEY, chatTextSize);
  }, [chatTextSize]);

  useEffect(() => {
    window.localStorage.setItem(SHOW_IMAGE_THUMBNAILS_STORAGE_KEY, String(showImageThumbnails));
  }, [showImageThumbnails]);

  useEffect(() => {
    setDismissedActivityFeedItems(data?.dismissed_inbox_items ?? {});
  }, [data?.dismissed_inbox_items]);

  useEffect(() => {
    setReadActivityFeedItems(data?.read_inbox_items ?? {});
  }, [data?.read_inbox_items]);

  useEffect(() => {
    const unreadCounts = Object.fromEntries(
      (data?.thread_activities ?? [])
        .filter((activity) => activity.unread_count > 0)
        .map((activity) => [activity.thread_root_id, activity.unread_count]),
    );
    setThreadUnreadCounts(unreadCounts);
  }, [data?.thread_activities]);

  function adjustChatTextSize(delta: number) {
    setChatTextSize((current) => {
      const index = CHAT_TEXT_SIZE_OPTIONS.indexOf(current);
      const nextIndex = Math.min(CHAT_TEXT_SIZE_OPTIONS.length - 1, Math.max(0, index + delta));
      return CHAT_TEXT_SIZE_OPTIONS[nextIndex] ?? "default";
    });
  }

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      const modifier = event.metaKey || event.ctrlKey;

      if (modifier && (event.key === "=" || event.key === "+" || event.code === "Equal" || event.code === "NumpadAdd")) {
        event.preventDefault();
        adjustChatTextSize(1);
        return;
      }

      if (modifier && (event.key === "-" || event.key === "_" || event.code === "Minus" || event.code === "NumpadSubtract")) {
        event.preventDefault();
        adjustChatTextSize(-1);
        return;
      }

      if (modifier && event.key === "0") {
        event.preventDefault();
        setChatTextSize("default");
        return;
      }

      if (modifier && event.key.toLowerCase() === "k") {
        event.preventDefault();
        openSearchModal();
        return;
      }

      if (modifier && event.key === ",") {
        event.preventDefault();
        setShowSettingsModal(true);
        return;
      }

      if (modifier && event.key === "[") {
        event.preventDefault();
        navigateBack(() => {
          if (showSearchModal) setShowSearchModal(false);
          else if (showActivityFeedModal) setShowActivityFeedModal(false);
          else if (showSavedModal) setShowSavedModal(false);
          else if (selectedAgentId) setSelectedAgentId(null);
          else if (showThread) setShowThread(false);
        });
        return;
      }

      if (modifier && event.key === "]") {
        event.preventDefault();
        navigateForward();
        return;
      }

      const modalOpen =
        showCreateChannelModal ||
        showChannelSettingsModal ||
        showChannelAgentsModal ||
        showCreateAgentModal ||
        showSearchModal ||
        showActivityFeedModal ||
        showSavedModal ||
        showOwnerProfileModal ||
        showSettingsModal ||
        Boolean(editingAgentId);
      if (event.key === "Escape" && !modalOpen && !isTextInput(event.target)) {
        if (selectedAgentId) {
          setSelectedAgentId(null);
        } else if (showThread) {
          setShowThread(false);
        }
      }
    }

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [
    activeChannelId,
    editingAgentId,
    selectedAgentId,
    showChannelAgentsModal,
    showChannelSettingsModal,
    showCreateAgentModal,
    showCreateChannelModal,
    showActivityFeedModal,
    showOwnerProfileModal,
    showSettingsModal,
    showSavedModal,
    showSearchModal,
    showThread,
  ]);

  const channel = useMemo(() => {
    return data?.channels.find((c) => c.id === activeChannelId) ?? data?.channels[0] ?? null;
  }, [activeChannelId, data]);

  useEffect(() => {
    if (!data || !window.location.hash.startsWith("#/message/")) return;
    const messageId = decodeURIComponent(window.location.hash.replace("#/message/", ""));
    const message = data.messages.find((item) => item.id === messageId || item.id.startsWith(messageId));
    if (!message) return;
    selectChannel(message.channel_id);
    revealThread(message.thread_root_id ?? message.id, message.channel_id);
    setFocusedMessageId(message.id);
    setActiveTab("chat");
    window.history.replaceState(window.history.state, "", `${window.location.pathname}${window.location.search}`);
  }, [data]);

  useEffect(() => {
    function onPopState(event: PopStateEvent) {
      if (!isAppHistoryState(event.state)) return;
      if (isMobileViewport()) {
        window.history.replaceState(null, "");
        appHistoryReadyRef.current = false;
        setAppHistoryPosition(0, 0);
        lastAppHistoryKeyRef.current = null;
        return;
      }
      restoringAppHistoryRef.current = true;
      appHistoryReadyRef.current = true;
      setAppHistoryPosition(event.state.index);
      const activeChannelIdFromState = event.state.activeChannelId ?? null;
      const activeThreadIdFromState = event.state.activeThreadId ?? null;
      const activeTabFromState = event.state.activeTab ?? "chat";
      lastAppHistoryKeyRef.current = appHistoryKey({
        ...event.state,
        activeChannelId: activeChannelIdFromState,
        activeThreadId: activeThreadIdFromState,
        activeTab: activeTabFromState,
      });
      if (activeChannelIdFromState) {
        setActiveChannelId(activeChannelIdFromState);
        setActiveThreadId(activeThreadIdFromState);
        rememberChannelThread(activeChannelIdFromState, activeThreadIdFromState);
      }
      setActiveTab(activeTabFromState);
      setShowThread(event.state.showThread);
      setShowMobileSidebar(event.state.showMobileSidebar);
      setMobileSidebarDragPx(0);
      setSelectedAgentId(event.state.selectedAgentId);
      setShowSearchModal(event.state.activeModal === "search");
      setShowActivityFeedModal(event.state.activeModal === "activity");
      setShowSavedModal(event.state.activeModal === "saved");
    }

    window.addEventListener("popstate", onPopState);
    return () => window.removeEventListener("popstate", onPopState);
  }, []);

  useEffect(() => {
    if (!data || !activeChannelId) return;
    if (isMobileViewport()) {
      if (isAppHistoryState(window.history.state)) {
        window.history.replaceState(null, "");
      }
      appHistoryReadyRef.current = false;
      replaceNextAppHistoryEntryRef.current = false;
      lastAppHistoryKeyRef.current = null;
      setAppHistoryPosition(0, 0);
      return;
    }

    const currentState = buildAppHistoryState(appHistoryIndexRef.current);
    const currentKey = appHistoryKey(currentState);

    if (restoringAppHistoryRef.current) {
      restoringAppHistoryRef.current = false;
      lastAppHistoryKeyRef.current = currentKey;
      return;
    }

    if (!appHistoryReadyRef.current) {
      const existingState = window.history.state;
      if (isAppHistoryState(existingState)) {
        appHistoryReadyRef.current = true;
        setAppHistoryPosition(existingState.index);
        const nextState = buildAppHistoryState(existingState.index);
        window.history.replaceState(nextState, "");
        lastAppHistoryKeyRef.current = appHistoryKey(nextState);
        return;
      }

      const baseState: AppHistoryState = {
        ...currentState,
        index: 0,
      };
      const baseKey = appHistoryKey(baseState);
      window.history.replaceState(baseState, "");
      appHistoryReadyRef.current = true;
      setAppHistoryPosition(0, 0);
      lastAppHistoryKeyRef.current = baseKey;

      if (baseKey === currentKey) return;
      const firstState = { ...currentState, index: 1 };
      window.history.pushState(firstState, "");
      setAppHistoryPosition(1, 1);
      lastAppHistoryKeyRef.current = appHistoryKey(firstState);
      return;
    }

    if (lastAppHistoryKeyRef.current === currentKey) return;

    if (replaceNextAppHistoryEntryRef.current) {
      replaceNextAppHistoryEntryRef.current = false;
      const nextState = { ...currentState, index: appHistoryIndexRef.current };
      window.history.replaceState(nextState, "");
      setAppHistoryPosition(nextState.index);
      lastAppHistoryKeyRef.current = appHistoryKey(nextState);
      return;
    }

    const nextState = { ...currentState, index: appHistoryIndexRef.current + 1 };
    window.history.pushState(nextState, "");
    setAppHistoryPosition(nextState.index, nextState.index);
    lastAppHistoryKeyRef.current = appHistoryKey(nextState);
  }, [
    data,
    activeChannelId,
    activeThreadId,
    activeTab,
    selectedAgentId,
    activeMobileModal,
    showMobileSidebar,
    showThread,
  ]);

  useEffect(() => {
    const hasOpenModal = showChannelAgentsModal ||
      showChannelSettingsModal ||
      showCreateAgentModal ||
      showCreateChannelModal ||
      showActivityFeedModal ||
      showOwnerProfileModal ||
      showSettingsModal ||
      showSavedModal ||
      showSearchModal;
    const isSidebarSwipe = !showThread && !selectedAgentId;
    if (!isMobileViewport() || showMobileSidebar || hasOpenModal) return;

    let startX: number | null = null;
    let startY: number | null = null;
    let lastX = 0;
    let lastTime = 0;
    let tracking = false;

    function mobileSidebarWidth() {
      return window.innerWidth;
    }

    function resetSwipe() {
      startX = null;
      startY = null;
      lastX = 0;
      lastTime = 0;
      tracking = false;
      setMobileSidebarDragPx(0);
      setMobileDragSurface(null);
    }

    function onTouchStart(event: TouchEvent) {
      if (event.touches.length !== 1 || isTextInput(event.target) || isActionControl(event.target)) {
        resetSwipe();
        return;
      }

      const touch = event.touches[0];
      if (touch.clientX > MOBILE_EDGE_SWIPE_START_PX) {
        resetSwipe();
        return;
      }
      startX = touch.clientX;
      startY = touch.clientY;
      lastX = touch.clientX;
      lastTime = event.timeStamp;
      tracking = true;
      setMobileDragSurface(isSidebarSwipe ? "sidebar" : "panel");
      setMobileSidebarDragPx(MOBILE_SIDEBAR_PEEK_PX);
    }

    function onTouchMove(event: TouchEvent) {
      if (!tracking || startX === null || startY === null || event.touches.length !== 1) return;
      const touch = event.touches[0];
      const deltaX = touch.clientX - startX;
      const deltaY = Math.abs(touch.clientY - startY);

      if (deltaY > MOBILE_EDGE_SWIPE_MAX_VERTICAL_PX && Math.abs(deltaX) < MOBILE_EDGE_SWIPE_OPEN_PX) {
        resetSwipe();
        return;
      }
      if (Math.abs(deltaX) > 10) {
        event.preventDefault();
      }

      const width = mobileSidebarWidth();
      setMobileSidebarDragPx(Math.max(MOBILE_SIDEBAR_PEEK_PX, Math.min(width, deltaX)));
      lastX = touch.clientX;
    }

    function onTouchEnd(event: TouchEvent) {
      if (!tracking || startX === null) {
        resetSwipe();
        return;
      }

      const width = mobileSidebarWidth();
      const currentPx = Math.max(0, Math.min(width, lastX - startX));
      const elapsed = Math.max(1, event.timeStamp - lastTime);
      const velocity = (lastX - startX) / elapsed;
      const shouldOpen = currentPx >= width * 0.28 || velocity > MOBILE_SIDEBAR_FLING_VELOCITY;

      if (shouldOpen) {
        if (selectedAgentId) {
          closeSelectedAgent();
        } else if (showThread) {
          closeThreadPanel();
        } else {
          openMobileSidebarFromContent();
        }
      }
      resetSwipe();
    }

    window.addEventListener("touchstart", onTouchStart, { passive: false, capture: true });
    window.addEventListener("touchmove", onTouchMove, { passive: false, capture: true });
    window.addEventListener("touchend", onTouchEnd, { passive: true, capture: true });
    window.addEventListener("touchcancel", resetSwipe, { passive: true, capture: true });
    return () => {
      window.removeEventListener("touchstart", onTouchStart, { capture: true });
      window.removeEventListener("touchmove", onTouchMove, { capture: true });
      window.removeEventListener("touchend", onTouchEnd, { capture: true });
      window.removeEventListener("touchcancel", resetSwipe, { capture: true });
    };
  }, [
    activeThreadId,
    selectedAgentId,
    showChannelAgentsModal,
    showChannelSettingsModal,
    showCreateAgentModal,
    showCreateChannelModal,
    showActivityFeedModal,
    showMobileSidebar,
    showOwnerProfileModal,
    showSettingsModal,
    showSavedModal,
    showSearchModal,
    showThread,
  ]);

  useEffect(() => {
    function onTouchStart(event: TouchEvent) {
      if (!isMobileViewport() || !isActionControl(event.target) || isTextInput(event.target)) return;
      if (event.target instanceof HTMLElement && event.target.closest(".composer, .reply-composer")) return;
      if (document.activeElement instanceof HTMLElement && isTextInput(document.activeElement)) {
        document.activeElement.blur();
      }
    }

    window.addEventListener("touchstart", onTouchStart, { passive: true, capture: true });
    return () => {
      window.removeEventListener("touchstart", onTouchStart, { capture: true });
    };
  }, []);

  useEffect(() => {
    function updateComposerFocus() {
      const activeElement = document.activeElement;
      const isFocused = Boolean(
        isMobileViewport()
          && activeElement instanceof HTMLElement
          && activeElement.closest(".composer, .reply-composer"),
      );
      setMobileComposerFocused((current) => current === isFocused ? current : isFocused);
    }

    function onFocusOut() {
      window.requestAnimationFrame(updateComposerFocus);
    }

    window.addEventListener("focusin", updateComposerFocus);
    window.addEventListener("focusout", onFocusOut);
    window.addEventListener("resize", updateComposerFocus);
    updateComposerFocus();
    return () => {
      window.removeEventListener("focusin", updateComposerFocus);
      window.removeEventListener("focusout", onFocusOut);
      window.removeEventListener("resize", updateComposerFocus);
    };
  }, []);

  const visibleMessages = useMemo(() => {
    return (data?.messages ?? []).filter((message) => !isProgressOnlyMessage(message));
  }, [data?.messages]);

  const rootMessages = useMemo(() => {
    if (!channel) return [];
    return visibleMessages.filter((m) => m.channel_id === channel.id && !m.thread_root_id);
  }, [visibleMessages, channel]);

  const activeRoot = activeThreadId ? rootMessages.find((m) => m.id === activeThreadId) ?? null : null;

  const replies = useMemo(() => {
    if (!activeRoot) return [];
    return visibleMessages.filter((m) => m.thread_root_id === activeRoot.id);
  }, [visibleMessages, activeRoot]);

  const threadReplySummaries = useMemo(() => {
    return visibleMessages.reduce<Record<string, ThreadReplySummary>>((summaries, message) => {
      if (!message.thread_root_id) return summaries;
      const current = summaries[message.thread_root_id] ?? { count: 0, latest: null, participants: [] };
      current.count += 1;
      if (!current.latest || new Date(message.created_at) > new Date(current.latest.created_at)) {
        current.latest = message;
      }
      if (
        message.sender_role !== "system" &&
        !current.participants.some((participant) =>
          participant.sender_role === message.sender_role &&
          participant.sender_name === message.sender_name &&
          participant.sender_agent_id === message.sender_agent_id)
      ) {
        current.participants.push(message);
      }
      summaries[message.thread_root_id] = current;
      return summaries;
    }, {});
  }, [visibleMessages]);

  const threadReplyCounts = useMemo(() => {
    return Object.fromEntries(
      Object.entries(threadReplySummaries).map(([rootId, summary]) => [rootId, summary.count]),
    );
  }, [threadReplySummaries]);

  const messagesById = useMemo(() => {
    return new Map(visibleMessages.map((message) => [message.id, message]));
  }, [visibleMessages]);

  const threadActivitiesByRoot = useMemo(() => {
    return new Map((data?.thread_activities ?? []).map((activity) => [activity.thread_root_id, activity]));
  }, [data?.thread_activities]);

  const allThreadRootMessages = useMemo(() => {
    const latestByRoot = new Map<string, number>();
    for (const message of visibleMessages) {
      if (!message.thread_root_id) continue;
      const timestamp = new Date(message.created_at).getTime();
      latestByRoot.set(message.thread_root_id, Math.max(latestByRoot.get(message.thread_root_id) ?? 0, timestamp));
    }
    for (const activity of data?.thread_activities ?? []) {
      const timestamp = new Date(activity.latest_activity_at).getTime();
      if (Number.isFinite(timestamp)) {
        latestByRoot.set(activity.thread_root_id, Math.max(latestByRoot.get(activity.thread_root_id) ?? 0, timestamp));
      }
    }
    return visibleMessages
      .filter((message) =>
        !message.thread_root_id &&
        latestByRoot.has(message.id) &&
        (message.thread_followed || (threadUnreadCounts[message.id] ?? 0) > 0) &&
        !locallyUnfollowedThreadIds.has(message.id))
      .sort((left, right) => (latestByRoot.get(right.id) ?? 0) - (latestByRoot.get(left.id) ?? 0));
  }, [data?.thread_activities, visibleMessages, locallyUnfollowedThreadIds, threadUnreadCounts]);

  const allActivityFeedItems = useMemo(() => {
    if (!data) return [];
    const channelsById = new Map(data.channels.map((item) => [item.id, item]));
    const agentsById = new Map(data.agents.map((item) => [item.id, item]));
    const latestByChannel = new Map<string, Message>();
    const repliesByRoot = new Map<string, Message[]>();

    for (const message of visibleMessages) {
      const currentChannelLatest = latestByChannel.get(message.channel_id);
      if (!currentChannelLatest || new Date(message.created_at) > new Date(currentChannelLatest.created_at)) {
        latestByChannel.set(message.channel_id, message);
      }
      if (message.thread_root_id) {
        const currentReplies = repliesByRoot.get(message.thread_root_id) ?? [];
        currentReplies.push(message);
        repliesByRoot.set(message.thread_root_id, currentReplies);
      }
    }
    for (const replies of repliesByRoot.values()) {
      replies.sort((left, right) => new Date(left.created_at).getTime() - new Date(right.created_at).getTime());
    }

    const channelLabel = (channelId: string | null) => {
      if (!channelId) return APP_DISPLAY_NAME;
      const target = channelsById.get(channelId);
      if (!target) return "Unknown";
      if (target.kind === "dm") {
        const agent = target.dm_agent_id ? agentsById.get(target.dm_agent_id) : null;
        return agent ? `@${agent.handle}` : "Direct message";
      }
      return `#${target.name}`;
    };
    const timestamp = (value: string | null | undefined) => value || new Date(0).toISOString();
    const items: ActivityFeedItem[] = [];
    const threadRootIdsForActivityFeed = new Set(allThreadRootMessages.map((message) => message.id));

    for (const channel of data.channels) {
      const unread = channel.unread_count > 0 || channelAlertIds.has(channel.id);
      if (!unread) continue;
      const latest = latestByChannel.get(channel.id);
      if (latest?.thread_root_id && threadRootIdsForActivityFeed.has(latest.thread_root_id)) continue;
      const dmAgent = channel.kind === "dm" && channel.dm_agent_id ? agentsById.get(channel.dm_agent_id) : null;
      items.push({
        id: `${channel.kind}:${channel.id}`,
        dismissId: `${channel.kind}:${channel.id}`,
        kind: channel.kind === "dm" ? "dm" : "channel",
        title: channel.kind === "dm" ? `DM with @${dmAgent?.handle ?? "agent"}` : `New activity in #${channel.name}`,
        excerpt: latest?.body ?? visibleChannelDescription(channel.description),
        surface: channel.kind === "dm" ? "Direct message" : `#${channel.name}`,
        actor: latest?.sender_name ?? "",
        timestamp: timestamp(latest?.created_at),
        unread: true,
        actorAgentId: latest?.sender_agent_id ?? dmAgent?.id ?? null,
        actorRole: latest?.sender_role ?? (channel.kind === "dm" ? "agent" : null),
        channelId: channel.id,
        threadId: latest?.thread_root_id ?? null,
        messageId: latest?.id ?? null,
        taskId: null,
        reminderId: null,
        replyCount: latest?.thread_root_id ? (threadReplyCounts[latest.thread_root_id] ?? 0) : 0,
        newCount: channel.unread_count,
      });
    }

    for (const root of allThreadRootMessages) {
      const replies = repliesByRoot.get(root.id) ?? [];
      const threadActivity = threadActivitiesByRoot.get(root.id);
      const unreadCount = threadUnreadCounts[root.id] ?? threadActivity?.unread_count ?? 0;
      const latestActivity = threadActivity
        ? messagesById.get(threadActivity.latest_message_id) ?? replies[replies.length - 1] ?? root
        : replies[replies.length - 1] ?? root;
      const unread = unreadCount > 0;
      items.push({
        id: `thread:${root.id}`,
        dismissId: `thread:${root.id}`,
        kind: "thread",
        title: firstLines(latestActivity.body, 1),
        excerpt: latestActivity.body,
        surface: channelLabel(root.channel_id),
        actor: latestActivity.sender_name,
        timestamp: timestamp(latestActivity.created_at),
        unread,
        actorAgentId: latestActivity.sender_agent_id,
        actorRole: latestActivity.sender_role,
        channelId: root.channel_id,
        threadId: root.id,
        messageId: latestActivity.id,
        taskId: null,
        reminderId: null,
        replyCount: threadReplyCounts[root.id] ?? 0,
        newCount: unreadCount,
      });
    }

    visibleMessages
      .filter((message) => message.sender_role !== "owner" && messageMentionsOwner(message))
      .sort((left, right) => new Date(right.created_at).getTime() - new Date(left.created_at).getTime())
      .forEach((message) => {
        const rootId = message.thread_root_id ?? message.id;
        items.push({
          id: `mention:${message.id}`,
          dismissId: `mention:${message.id}`,
          kind: "mention",
          title: firstLines(message.body, 1),
          excerpt: message.body,
          surface: channelLabel(message.channel_id),
          actor: message.sender_name,
          timestamp: message.created_at,
          unread: channelAlertIds.has(message.channel_id) || (message.thread_root_id ? (threadUnreadCounts[message.thread_root_id] ?? 0) > 0 : false),
          actorAgentId: message.sender_agent_id,
          actorRole: message.sender_role,
          channelId: message.channel_id,
          threadId: rootId,
          messageId: message.id,
          taskId: null,
          reminderId: null,
          replyCount: threadReplyCounts[rootId] ?? 0,
          newCount: message.thread_root_id ? (threadUnreadCounts[message.thread_root_id] ?? 0) : 0,
        });
      });

    data.tasks
      .filter((task) => task.status !== "done")
      .forEach((task) => {
        items.push({
          id: `task:${task.id}`,
          dismissId: `task:${task.id}`,
          kind: "task",
          title: `Task #${task.number}: ${task.title}`,
          excerpt: task.assignee_name ? `Assigned to ${task.assignee_name}` : "Unassigned",
          surface: `#${task.channel_name}`,
          actor: task.status.replace("_", " "),
          timestamp: task.updated_at,
          unread: task.status === "in_review",
          channelId: task.channel_id,
          threadId: task.message_id,
          messageId: task.message_id,
          taskId: task.id,
          reminderId: null,
          replyCount: threadReplyCounts[task.message_id] ?? 0,
          newCount: 0,
        });
      });

    data.reminders
      .filter((reminder) => reminder.status === "fired")
      .forEach((reminder) => {
        items.push({
          id: `reminder:${reminder.id}`,
          dismissId: `reminder:${reminder.id}`,
          kind: "reminder",
          title: reminder.title,
          excerpt: reminder.note,
          surface: reminder.channel_id ? channelLabel(reminder.channel_id) : "Reminder",
          actor: "Reminder due",
          timestamp: reminder.fired_at ?? reminder.due_at,
          unread: true,
          channelId: reminder.channel_id,
          threadId: reminder.thread_root_id,
          messageId: reminder.message_id,
          taskId: null,
          reminderId: reminder.id,
          replyCount: reminder.thread_root_id ? (threadReplyCounts[reminder.thread_root_id] ?? 0) : 0,
          newCount: 1,
        });
      });

    return items;
  }, [allThreadRootMessages, channelAlertIds, data, messagesById, threadActivitiesByRoot, threadReplyCounts, threadUnreadCounts, visibleMessages]);

  const activityFeedItems = useMemo(() => {
    return allActivityFeedItems
      .filter((item) => {
        const dismissedAt = dismissedActivityFeedItems[item.dismissId];
        if (!dismissedAt) return true;
        return new Date(item.timestamp).getTime() > new Date(dismissedAt).getTime();
      })
      .map((item) => {
        const readAt = readActivityFeedItems[item.id];
        if (!readAt || new Date(item.timestamp).getTime() > new Date(readAt).getTime()) {
          return item;
        }
        return { ...item, unread: false };
      })
      .sort(compareActivityFeedItems)
      .slice(0, 120);
  }, [allActivityFeedItems, dismissedActivityFeedItems, readActivityFeedItems]);

  const activityFeedUnreadCount = useMemo(() => {
    return activityFeedItems.filter((item) => item.unread).length;
  }, [activityFeedItems]);

  const savedMessageIds = useMemo(() => {
    return new Set(data?.saved_messages.map((item) => item.message_id) ?? []);
  }, [data?.saved_messages]);

  const savedUnreadCount = useMemo(() => {
    const items = data?.saved_messages ?? [];
    if (items.length === 0) return 0;
    const readUntil = dismissedActivityFeedItems[SAVED_MESSAGES_READ_DISMISS_ID];
    if (!readUntil) return items.length;
    const readUntilTime = new Date(readUntil).getTime();
    if (!Number.isFinite(readUntilTime)) return items.length;
    return items.filter((item) => new Date(item.created_at).getTime() > readUntilTime).length;
  }, [data?.saved_messages, dismissedActivityFeedItems]);

  const shareBaseUrl = useMemo(() => {
    if (!data) return window.location.origin;
    return isTauriRuntime() ? data.web_base_url ?? window.location.origin : window.location.origin;
  }, [data?.web_base_url]);

  const visibleTasks = useMemo(() => {
    if (!data || !channel) return [];
    if (channel.kind === "dm") return [];
    return data.tasks.filter((task) => task.channel_id === channel.id);
  }, [data, channel]);

  const channelMemberIds = useMemo(() => {
    if (!data || !channel) return new Set<string>();
    return new Set(data.channel_members.filter((member) => member.channel_id === channel.id).map((member) => member.agent_id));
  }, [data, channel]);

  const channelAgents = useMemo(() => {
    if (!data || !channel) return [];
    return data.agents.filter((agent) => channelMemberIds.has(agent.id));
  }, [data, channel, channelMemberIds]);

  const activeTask = useMemo(() => {
    if (!data || !activeRoot) return null;
    return data.tasks.find((task) => task.message_id === activeRoot.id) ?? null;
  }, [data, activeRoot]);

  const activeChannelMessageCount = useMemo(() => {
    if (!activeChannelId) return 0;
    return visibleMessages.filter((message) => message.channel_id === activeChannelId).length;
  }, [visibleMessages, activeChannelId]);

  const activeRunFor = useCallback((agentId: string) => {
    return data?.agent_runs.find((run) => run.agent_id === agentId && ACTIVE_RUN_STATUSES.has(run.status)) ?? null;
  }, [data?.agent_runs]);

  const selectedAgent = useMemo(() => {
    if (!data || !selectedAgentId) return null;
    return data.agents.find((agent) => agent.id === selectedAgentId) ?? null;
  }, [data, selectedAgentId]);

  const selectedAgentRun = useMemo(() => {
    if (!selectedAgent) return null;
    return activeRunFor(selectedAgent.id);
  }, [selectedAgent, activeRunFor]);

  const selectedAgentActivities = useMemo(() => {
    if (!data || !selectedAgent) return [];
    return data.agent_activities
      .filter((activity) => activity.agent_id === selectedAgent.id || activity.agent_handle === selectedAgent.handle)
      .slice(0, 80);
  }, [data, selectedAgent]);

  const selectedAgentRuns = useMemo(() => {
    if (!data || !selectedAgent) return [];
    return data.agent_runs.filter((run) => run.agent_id === selectedAgent.id);
  }, [data, selectedAgent]);

  const selectedAgentPerformance = useMemo(() => {
    return buildAgentPerformance(selectedAgentActivities, selectedAgentRuns);
  }, [selectedAgentActivities, selectedAgentRuns]);

  const selectedAgentLiveActivity = useMemo(() => {
    return selectedAgentActivities.find((activity) => activity.run_id === selectedAgentRun?.id)
      ?? selectedAgentActivities.find((activity) => activity.kind in ACTIVITY_PHASE_LABELS)
      ?? null;
  }, [selectedAgentActivities, selectedAgentRun]);

  const selectedAgentPhase = selectedAgent ? (selectedAgentRun
    ? {
        kind: selectedAgentLiveActivity?.phase ?? selectedAgentLiveActivity?.kind ?? "run",
        label: selectedAgentLiveActivity ? phaseForActivity(selectedAgentLiveActivity.phase || selectedAgentLiveActivity.kind) : "Running",
        detail: selectedAgentLiveActivity?.summary || selectedAgentLiveActivity?.detail || "Waiting for observable output from the agent.",
      }
    : {
        kind: selectedAgent.status,
        label: selectedAgent.status,
        detail: "No active run.",
      }) : null;

  const selectedAgentWorkItems = useMemo(() => {
    if (!data || !selectedAgent) return [];
    return data.agent_work_items
      .filter((item) => item.agent_id === selectedAgent.id && item.status !== "silent");
  }, [data, selectedAgent]);

  const searchResults = useMemo(() => {
    if (!data) return [];
    const query = searchQuery.trim().toLowerCase();
    if (!query) return [];
    const channelById = new Map(data.channels.map((item) => [item.id, item]));
    const agentById = new Map(data.agents.map((item) => [item.id, item]));
    const channelLabel = (channelId: string | null) => {
      if (!channelId) return "No channel";
      const target = channelById.get(channelId);
      if (!target) return "Unknown channel";
      if (target.kind === "dm") {
        const agent = target.dm_agent_id ? agentById.get(target.dm_agent_id) : null;
        return agent ? `DM with @${agent.handle}` : "Direct message";
      }
      return `#${target.name}`;
    };
    const includes = (value: string) => value.toLowerCase().includes(query);
    const results: SearchResult[] = [];

    if (searchScopeAllows(searchScope, "channels")) {
      results.push(...data.channels
        .filter((item) => {
        const dmAgent = item.kind === "dm" ? data.agents.find((agent) => agent.id === item.dm_agent_id) : null;
          return includes(`${item.name} ${visibleChannelDescription(item.description)} ${dmAgent?.handle ?? ""} ${dmAgent?.display_name ?? ""}`);
        })
        .map((item) => {
        const dmAgent = item.kind === "dm" ? data.agents.find((agent) => agent.id === item.dm_agent_id) : null;
        return {
          id: item.id,
          kind: item.kind === "dm" ? "dm" : "channel",
          title: item.kind === "dm" ? `@${dmAgent?.handle ?? "agent"}` : `#${item.name}`,
          detail: item.kind === "dm" ? dmAgent?.display_name ?? "direct message" : visibleChannelDescription(item.description) || "channel",
          excerpt: item.kind === "dm" ? dmAgent?.description ?? "" : visibleChannelDescription(item.description),
          createdAt: null,
          channelId: item.id,
          threadId: null,
          agentId: dmAgent?.id ?? null,
        };
        }).slice(0, 10));
    }

    if (searchScopeAllows(searchScope, "tasks")) {
      results.push(...data.tasks
        .filter((item) =>
          matchesSearchTime(item.updated_at, searchTimeRange) &&
          includes(`${item.title} ${item.status} ${item.channel_name} ${item.assignee_name ?? ""}`))
        .map((item) => ({
        id: item.id,
        kind: "task",
        title: `#${item.number} ${item.title}`,
        detail: `${item.channel_name} · ${item.status.replace("_", " ")}`,
        excerpt: item.assignee_name ? `Assigned to ${item.assignee_name}` : "Unassigned",
        createdAt: item.updated_at,
        channelId: item.channel_id,
        threadId: item.message_id,
        agentId: null,
      })).slice(0, 12));
    }

    if (searchScopeAllows(searchScope, "messages")) {
      results.push(...visibleMessages
        .filter((item) =>
          matchesSearchTime(item.created_at, searchTimeRange) &&
          includes(`${item.sender_name} ${item.body} ${channelLabel(item.channel_id)}`))
        .sort((a, b) => new Date(b.created_at).getTime() - new Date(a.created_at).getTime())
        .map((item) => ({
        id: item.id,
        kind: item.thread_root_id ? "reply" : "message",
        title: item.sender_name,
        detail: `${channelLabel(item.channel_id)} · ${item.thread_root_id ? "thread reply" : "message"} · ${formatTime(item.created_at)}`,
        excerpt: firstLines(item.body, 2),
        createdAt: item.created_at,
        channelId: item.channel_id,
        threadId: item.thread_root_id ?? item.id,
        agentId: item.sender_agent_id,
        senderRole: item.sender_role,
      })).slice(0, 40));
    }

    if (searchScopeAllows(searchScope, "artifacts")) {
      results.push(...data.artifacts
        .filter((item) =>
          matchesSearchTime(item.created_at, searchTimeRange) &&
          includes(`${item.title} ${item.summary} ${item.content} ${item.kind} ${channelLabel(item.channel_id)}`))
        .sort((a, b) => new Date(b.created_at).getTime() - new Date(a.created_at).getTime())
        .map((item) => ({
        id: item.id,
        kind: "artifact",
        title: item.title,
        detail: `${item.kind} · ${channelLabel(item.channel_id)} · ${formatTime(item.created_at)}`,
        excerpt: firstLines(item.summary || item.content, 2),
        createdAt: item.created_at,
        channelId: item.channel_id,
        threadId: item.thread_root_id ?? item.message_id,
        agentId: item.creator_agent_id,
      })).slice(0, 20));
    }

    if (searchScopeAllows(searchScope, "agents")) {
      results.push(...data.agents
        .filter((item) => includes(`${item.handle} ${item.display_name} ${item.runtime} ${item.model} ${item.description}`))
        .map((item) => ({
        id: item.id,
        kind: "agent",
        title: `@${item.handle}`,
        detail: `${item.display_name} · ${item.runtime} · ${item.status}`,
        excerpt: item.description,
        createdAt: null,
        channelId: null,
        threadId: null,
        agentId: item.id,
      })).slice(0, 10));
    }

    if (searchScopeAllows(searchScope, "activity")) {
      results.push(...data.agent_activities
        .filter((item) =>
          matchesSearchTime(item.created_at, searchTimeRange) &&
          includes(`${item.agent_handle} ${item.kind} ${item.title} ${item.detail}`))
        .sort((a, b) => new Date(b.created_at).getTime() - new Date(a.created_at).getTime())
        .map((item) => ({
        id: item.id,
        kind: "activity",
        title: item.title,
        detail: `${item.agent_handle || "unknown"} · ${formatTime(item.created_at)}`,
        excerpt: item.detail,
        createdAt: item.created_at,
        channelId: null,
        threadId: null,
        agentId: item.agent_id,
      })).slice(0, 16));

      results.push(...data.agent_work_items
        .filter((item) =>
          item.status !== "silent" &&
          matchesSearchTime(item.updated_at, searchTimeRange) &&
          includes(`${item.agent_handle} ${item.status} ${item.title} ${item.context}`))
        .sort((a, b) => new Date(b.updated_at).getTime() - new Date(a.updated_at).getTime())
        .map((item) => ({
        id: item.id,
        kind: "request",
        title: item.title,
        detail: `${agentRequestSourceLabel(item.source_kind, item.task_number)} · ${item.agent_handle} · ${item.status} · ${channelLabel(item.channel_id)}`,
        excerpt: firstLines(item.context, 2),
        createdAt: item.updated_at,
        channelId: item.channel_id,
        threadId: item.thread_root_id,
        agentId: item.agent_id,
      })).slice(0, 16));
    }

    return results.slice(0, 80);
  }, [data, searchQuery, searchScope, searchTimeRange, visibleMessages]);

  function taskForMessage(messageId: string) {
    return data?.tasks.find((task) => task.message_id === messageId) ?? null;
  }

  function normalizedChannelNameInput(value: string) {
    return value.trim().replace(/^#+/, "").toLowerCase().replace(/ /g, "-");
  }

  function channelNameExists(normalizedName: string, excludeChannelId?: string) {
    return Boolean(data?.channels.some((item) => (
      item.id !== excludeChannelId &&
      item.name === normalizedName
    )));
  }

  function duplicateChannelNameMessage(normalizedName: string) {
    return `Channel #${normalizedName} already exists`;
  }

  function isDuplicateChannelNameError(message: string) {
    return message.startsWith("channel #") && message.endsWith(" already exists");
  }

  const normalizedNewChannelName = normalizedChannelNameInput(newChannel);
  const newChannelDuplicateError = normalizedNewChannelName && channelNameExists(normalizedNewChannelName)
    ? duplicateChannelNameMessage(normalizedNewChannelName)
    : null;
  const newChannelNameError = newChannelDuplicateError || newChannelNameSubmitError;

  useEffect(() => {
    setChannelNameDraft(channel?.name ?? "");
    setChannelDescriptionDraft(channel ? visibleChannelDescription(channel.description) : "");
  }, [channel?.id, channel?.name, channel?.description]);

  useEffect(() => {
    if (channel?.kind !== "dm") return;
    setActiveTab("chat");
    setShowChannelSettingsModal(false);
    setShowChannelAgentsModal(false);
  }, [channel?.id, channel?.kind]);

  useEffect(() => {
    if (!activeChannelId) return;
    apiInvoke("mark_channel_read", { channelId: activeChannelId }).catch((err) => console.error(err));
  }, [activeChannelId, activeChannelMessageCount]);

  async function createChannel() {
    if (createChannelSubmittingRef.current) return;
    const name = normalizedChannelNameInput(newChannel);
    if (!name) return;
    if (channelNameExists(name)) {
      setNewChannelNameSubmitError(duplicateChannelNameMessage(name));
      return;
    }
    const agentIds = Array.from(newChannelAgentIds);
    let result: { channelId?: string };
    const shouldReturnToMobileHome = createChannelOpenedFromMobileHomeRef.current;
    createChannelSubmittingRef.current = true;
    setCreateChannelSubmitting(true);
    try {
      result = await apiInvoke<{ channelId?: string }>("create_channel", {
        name,
        agentIds: agentIds.length > 0 ? agentIds : undefined,
      });
      await refresh(true, shouldReturnToMobileHome ? undefined : result.channelId);
    } catch (err) {
      const message = errorMessage(err, "create_channel failed");
      if (isDuplicateChannelNameError(message)) {
        setNewChannelNameSubmitError(duplicateChannelNameMessage(name));
        setAppError(null);
      } else {
        setAppError(message);
        console.error(err);
      }
      return;
    } finally {
      createChannelSubmittingRef.current = false;
      setCreateChannelSubmitting(false);
    }
    setNewChannel("");
    setNewChannelNameSubmitError(null);
    setNewChannelAgentIds(new Set());
    setShowCreateChannelModal(false);
    createChannelOpenedFromMobileHomeRef.current = false;
    if (shouldReturnToMobileHome) {
      returnToMobileHome();
      return;
    }
    if (result.channelId) {
      setActiveChannelId(result.channelId);
      setShowMobileSidebar(false);
      openThread(null, result.channelId);
    }
  }

  async function saveChannel() {
    if (!channel) return;
    const name = normalizedChannelNameInput(channelNameDraft);
    if (!name) return;
    if (channel.kind === "dm") {
      setAppError("Direct message settings are managed by the agent profile");
      setShowChannelSettingsModal(false);
      return;
    }
    if (channelNameExists(name, channel.id)) {
      setAppError(`Channel #${name} already exists`);
      return;
    }
    await mutate("update_channel", {
      channelId: channel.id,
      name,
      description: channelDescriptionDraft,
    });
    setShowChannelSettingsModal(false);
  }

  async function saveOwnerProfile() {
    if (!ownerProfileDraft.displayName.trim()) return;
    await mutate("update_owner_profile", {
      displayName: ownerProfileDraft.displayName,
      avatar: ownerProfileDraft.avatar,
      description: ownerProfileDraft.description,
    });
    setShowOwnerProfileModal(false);
  }

  async function deleteChannel() {
    if (!channel) return;
    const channelToDelete = channel;
    const fallbackChannelId = data?.channels.find((item) => item.id !== channelToDelete.id)?.id ?? "";
    setConfirmRequest({
      title: `Delete #${channelToDelete.name}?`,
      body: "This removes the channel timeline, tasks, threads, agent memberships, schedules, and attachments for this channel. This cannot be undone.",
      confirmLabel: "Delete channel",
      onConfirm: async () => {
        await mutate("delete_channel", { channelId: channelToDelete.id });
        setShowChannelSettingsModal(false);
        forgetChannelThread(channelToDelete.id);
        if (activeChannelId === channelToDelete.id) {
          setActiveChannelId(fallbackChannelId);
          openThread(rememberedThreadForChannel(fallbackChannelId), fallbackChannelId);
          if (isMobileViewport()) {
            returnToMobileHome();
          }
        }
      },
    });
  }

  function updateRootComposerDraft(channelId: string | null | undefined, updater: (draft: ComposerDraftState) => ComposerDraftState) {
    setRootComposerDrafts((current) => updateComposerDraftRecord(current, channelId, updater));
  }

  function updateReplyComposerDraft(threadId: string | null | undefined, updater: (draft: ComposerDraftState) => ComposerDraftState) {
    setReplyComposerDrafts((current) => updateComposerDraftRecord(current, threadId, updater));
  }

  function setDraft(value: string) {
    updateRootComposerDraft(activeChannelId, (current) => ({ ...current, text: value }));
  }

  function setReplyDraft(value: string) {
    updateReplyComposerDraft(activeThreadId, (current) => ({ ...current, text: value }));
  }

  function defaultThreadForChannel(channelId: string) {
    const repliedRootIds = new Set(
      visibleMessages
        .filter((message) => message.channel_id === channelId && message.thread_root_id)
        .map((message) => message.thread_root_id),
    );
    return visibleMessages.find((m) => m.channel_id === channelId && !m.thread_root_id && repliedRootIds.has(m.id))?.id ?? null;
  }

  function rootThreadBelongsToChannel(channelId: string, threadId: string) {
    return visibleMessages.some((message) => message.channel_id === channelId && !message.thread_root_id && message.id === threadId);
  }

  function rememberedThreadForChannel(channelId: string) {
    if (!Object.prototype.hasOwnProperty.call(channelThreadMemory, channelId)) {
      return defaultThreadForChannel(channelId);
    }
    const rememberedThreadId = channelThreadMemory[channelId];
    if (!rememberedThreadId) return null;
    return rootThreadBelongsToChannel(channelId, rememberedThreadId)
      ? rememberedThreadId
      : defaultThreadForChannel(channelId);
  }

  function rememberChannelThread(channelId: string | null | undefined, threadId: string | null) {
    if (!channelId) return;
    setChannelThreadMemory((current) => {
      if (current[channelId] === threadId) return current;
      return { ...current, [channelId]: threadId };
    });
  }

  function latestThreadActivity(threadId: string): ThreadActivity | null {
    return threadActivitiesByRoot.get(threadId) ?? null;
  }

  function threadReadCutoff(threadId: string) {
    const activity = latestThreadActivity(threadId);
    const latestMessage = activity ? messagesById.get(activity.latest_message_id) : null;
    const latestReply = visibleMessages
      .filter((message) => message.thread_root_id === threadId)
      .reduce<Message | null>((latest, message) => {
        if (!latest || new Date(message.created_at) > new Date(latest.created_at)) return message;
        return latest;
      }, null);
    const latestAt = activity?.latest_activity_at ?? latestMessage?.created_at ?? latestReply?.created_at;
    const latestTime = latestAt ? new Date(latestAt).getTime() : 0;
    const cutoffTime = Math.max(Date.now(), Number.isFinite(latestTime) ? latestTime : 0);
    return new Date(cutoffTime).toISOString();
  }

  async function persistThreadReadMarkers(markers: { threadId: string; dismissedUntil: string }[]) {
    const cutoffByThreadId = new Map<string, string>();
    for (const marker of markers) {
      const existing = cutoffByThreadId.get(marker.threadId);
      if (!existing || new Date(marker.dismissedUntil).getTime() > new Date(existing).getTime()) {
        cutoffByThreadId.set(marker.threadId, marker.dismissedUntil);
      }
    }
    const items = Array.from(cutoffByThreadId, ([threadId, dismissedUntil]) => ({
      itemId: `thread:${threadId}`,
      dismissedUntil,
    }));
    if (items.length === 0) return;
    await apiInvoke("mark_inbox_items_read", { items });
  }

  function markThreadRead(threadId: string) {
    void persistThreadReadMarkers([
      { threadId, dismissedUntil: threadReadCutoff(threadId) },
    ]).catch((err) => console.error(err));
  }

  function forgetChannelThread(channelId: string | null | undefined) {
    if (!channelId) return;
    setChannelThreadMemory((current) => {
      if (!Object.prototype.hasOwnProperty.call(current, channelId)) return current;
      const next = { ...current };
      delete next[channelId];
      return next;
    });
  }

  function restoreRememberedThreadForChannel(channelId: string) {
    const nextThreadId = rememberedThreadForChannel(channelId);
    openThread(nextThreadId, channelId);
    if (!isMobileViewport()) {
      setShowThread(Boolean(nextThreadId));
    }
  }

  function selectChannel(channelId: string) {
    const nextChannel = data?.channels.find((item) => item.id === channelId) ?? null;
    setSelectedAgentId(null);
    setActiveChannelId(channelId);
    setShowMobileSidebar(false);
    if (nextChannel?.kind === "dm") {
      setActiveTab("chat");
    }
    restoreRememberedThreadForChannel(channelId);
  }

  function openThread(threadId: string | null, channelId = activeChannelId) {
    setActiveThreadId(threadId);
    rememberChannelThread(channelId, threadId);
    setFocusedMessageId(null);
    if (!threadId) return;
    setThreadUnreadCounts((current) => {
      if (!current[threadId]) return current;
      const next = { ...current };
      delete next[threadId];
      return next;
    });
    markThreadRead(threadId);
  }

  function revealThread(threadId: string | null, channelId = activeChannelId) {
    openThread(threadId, channelId);
    if (threadId) {
      setSelectedAgentId(null);
      setShowThread(true);
    }
  }

  function revealThreadRootInChannel(message: Message) {
    setSelectedAgentId(null);
    setActiveTab("chat");
    setActiveChannelId(message.channel_id);
    if (isMobileViewport()) {
      setShowThread(false);
    }
    setFocusedMessageId(null);
    window.requestAnimationFrame(() => {
      setFocusedMessageId(message.id);
    });
  }

  function canNavigateBack() {
    if (isMobileViewport()) return false;
    return appHistoryReadyRef.current && appHistoryIndexRef.current > 0;
  }

  function canNavigateForward() {
    if (isMobileViewport()) return false;
    return appHistoryReadyRef.current && appHistoryIndexRef.current < appHistoryMaxIndexRef.current;
  }

  function navigateBack(fallback: () => void) {
    if (isMobileViewport()) {
      fallback();
      return;
    }
    if (canNavigateBack()) {
      window.history.back();
      return;
    }
    fallback();
  }

  function navigateForward() {
    if (isMobileViewport()) return;
    if (canNavigateForward()) {
      window.history.forward();
    }
  }

  function openMobileSidebarFromContent() {
    setMobileSidebarFocus("home");
    if (isMobileViewport()) {
      setShowThread(false);
      setSelectedAgentId(null);
      setShowMobileSidebar(true);
      setMobileSidebarDragPx(0);
      return;
    }
    setShowMobileSidebar(true);
  }

  function openMobileHome() {
    returnToMobileHome();
  }

  function isMobileHomeOpen() {
    return isMobileViewport() && showMobileSidebar && mobileSidebarFocus === "home";
  }

  function returnToMobileHome() {
    setShowSearchModal(false);
    setShowActivityFeedModal(false);
    setShowSavedModal(false);
    setSelectedAgentId(null);
    setShowThread(false);
    setShowMobileSidebar(true);
    setMobileSidebarDragPx(0);
    setMobileSidebarFocus("home");
  }

  function openSearchModal() {
    setShowMobileSidebar(false);
    setMobileSidebarFocus("home");
    setShowActivityFeedModal(false);
    setShowSavedModal(false);
    setShowSearchModal(true);
  }

  function openActivityFeedModal() {
    setShowMobileSidebar(false);
    setMobileSidebarFocus("home");
    setShowSearchModal(false);
    setShowSavedModal(false);
    setShowActivityFeedModal(true);
  }

  function openSavedModal() {
    setShowMobileSidebar(false);
    setMobileSidebarFocus("home");
    setShowSearchModal(false);
    setShowActivityFeedModal(false);
    setShowSavedModal(true);
    void markSavedMessagesRead();
  }

  async function markSavedMessagesRead() {
    const items = data?.saved_messages ?? [];
    if (items.length === 0) return;
    const latestSavedAt = items.reduce((latest, item) => {
      const savedAt = new Date(item.created_at).getTime();
      return Number.isFinite(savedAt) ? Math.max(latest, savedAt) : latest;
    }, 0);
    const cutoff = new Date(Math.max(Date.now(), latestSavedAt)).toISOString();
    const current = dismissedActivityFeedItems[SAVED_MESSAGES_READ_DISMISS_ID];
    if (current && new Date(current).getTime() >= new Date(cutoff).getTime()) return;
    setDismissedActivityFeedItems((existing) => ({ ...existing, [SAVED_MESSAGES_READ_DISMISS_ID]: cutoff }));
    await apiInvoke("dismiss_inbox_items", {
      items: [{ itemId: SAVED_MESSAGES_READ_DISMISS_ID, dismissedUntil: cutoff }],
    });
  }

  function closeAppModal(fallback: () => void) {
    if (isMobileViewport()) {
      fallback();
      return;
    }
    if (activeMobileModal && canNavigateBack()) {
      window.history.back();
      return;
    }
    fallback();
  }

  function closeSelectedAgent() {
    if (selectedAgentId && searchResultAgentIdRef.current === selectedAgentId) {
      searchResultAgentIdRef.current = null;
      replaceNextAppHistoryEntryRef.current = true;
      setSelectedAgentId(null);
      return;
    }
    navigateBack(() => setSelectedAgentId(null));
  }

  function closeThreadPanel() {
    if (activeThreadId && searchResultThreadIdRef.current === activeThreadId) {
      searchResultThreadIdRef.current = null;
    }
    replaceNextAppHistoryEntryRef.current = true;
    openThread(null);
    setShowThread(false);
  }

  function optimisticMessageAttachments(messageId: string, attachments: DraftAttachment[]): MessageAttachment[] {
    const objectUrls: string[] = [];
    const messageAttachments = attachments.map((attachment) => {
      const localUrl = URL.createObjectURL(attachment.file);
      objectUrls.push(localUrl);
      return {
        id: `local-${attachment.id}`,
        message_id: messageId,
        original_name: attachment.original_name,
        mime_type: attachment.mime_type,
        size_bytes: attachment.size_bytes,
        storage_path: "",
        local_url: localUrl,
        created_at: new Date().toISOString(),
      };
    });
    if (objectUrls.length > 0) {
      optimisticAttachmentUrlsRef.current.set(messageId, objectUrls);
    }
    return messageAttachments;
  }

  function releaseOptimisticAttachmentUrls(messageId: string) {
    const objectUrls = optimisticAttachmentUrlsRef.current.get(messageId) ?? [];
    objectUrls.forEach((url) => URL.revokeObjectURL(url));
    optimisticAttachmentUrlsRef.current.delete(messageId);
  }

  function addOptimisticOwnerMessage(
    channelId: string,
    threadRootId: string | null,
    body: string,
    asTask: boolean,
    attachments: DraftAttachment[] = [],
  ) {
    const id = `local-${clientId()}`;
    const createdAt = new Date().toISOString();
    const optimisticMessage: Message = {
      id,
      channel_id: channelId,
      thread_root_id: threadRootId,
      sender_agent_id: null,
      sender_name: data?.owner_profile.display_name || DEFAULT_OWNER_DISPLAY_NAME,
      sender_role: "owner",
      body,
      is_task: asTask,
      thread_followed: true,
      delivery_state: attachments.length > 0 ? "sending" : "complete",
      stream_key: "",
      task_number: null,
      task_status: null,
      attachments: optimisticMessageAttachments(id, attachments),
      artifacts: [],
      created_at: createdAt,
      updated_at: createdAt,
    };
    optimisticMessagesRef.current.set(id, optimisticMessage);
    knownMessageIdsRef.current?.add(id);
    invalidatePendingRefreshResult();
    setData((current) => current ? {
      ...current,
      messages: [...current.messages, optimisticMessage],
    } : current);
    return id;
  }

  function removeOptimisticMessage(messageId: string) {
    optimisticMessagesRef.current.delete(messageId);
    releaseOptimisticAttachmentUrls(messageId);
    knownMessageIdsRef.current?.delete(messageId);
    invalidatePendingRefreshResult();
    setData((current) => current ? {
      ...current,
      messages: current.messages.filter((message) => message.id !== messageId),
    } : current);
  }

  function settleOptimisticMessage(messageId: string, persistedMessage: Message) {
    optimisticMessagesRef.current.delete(messageId);
    releaseOptimisticAttachmentUrls(messageId);
    knownMessageIdsRef.current?.delete(messageId);
    knownMessageIdsRef.current?.add(persistedMessage.id);
    invalidatePendingRefreshResult();
    setData((current) => {
      if (!current) return current;
      const messages = current.messages
        .filter((message) => message.id !== messageId && message.id !== persistedMessage.id)
        .concat(persistedMessage);
      return { ...current, messages: sortedMessages(messages) };
    });
  }

  function appendDraftAttachments(files: FileList | File[], target: "root" | "reply") {
    const nextAttachments = Array.from(files)
      .filter((file) => {
        if (file.size <= MAX_ATTACHMENT_BYTES) return true;
        setAppError(`${file.name} is larger than 25MB`);
        return false;
      })
      .map(draftAttachmentFromFile);
    if (nextAttachments.length === 0) return;
    if (target === "root") {
      updateRootComposerDraft(activeChannelId, (current) => ({
        ...current,
        attachments: [...current.attachments, ...nextAttachments],
      }));
    } else {
      updateReplyComposerDraft(activeThreadId, (current) => ({
        ...current,
        attachments: [...current.attachments, ...nextAttachments],
      }));
    }
  }

  async function setChannelMember(agentId: string, member: boolean) {
    if (!channel) return;
    if (channel.kind === "dm") {
      setAppError("Direct message membership is fixed");
      return;
    }
    const channelId = channel.id;
    const agent = data?.agents.find((entry) => entry.id === agentId);
    // Optimistic update so ChannelAgentsModal's controlled checkbox flips
    // immediately on tap, matching the snappy feel of CreateChannelModal's
    // local Set-based picker. Without this, the controlled checkbox stays
    // pinned to the old `isMember` value until the backend round-trip and
    // refresh complete, and on mobile that lag reads as "tap didn't hit".
    setData((current) => {
      if (!current) return current;
      const without = current.channel_members.filter(
        (entry) => !(entry.channel_id === channelId && entry.agent_id === agentId),
      );
      if (!member) {
        return { ...current, channel_members: without };
      }
      if (!agent) return current;
      const optimistic: ChannelMember = {
        channel_id: channelId,
        agent_id: agentId,
        agent_handle: agent.handle,
        agent_display_name: agent.display_name,
        created_at: new Date().toISOString(),
      };
      return { ...current, channel_members: [...without, optimistic] };
    });
    try {
      // Bypass `mutate`'s blocking `await refresh()` — the backend emits a
      // targeted `channel_member_upsert` / `channel_member_remove` event
      // which `handleBackendEvent` patches into `channel_members` locally,
      // so the full bootstrap round-trip is no longer needed to confirm
      // the state. Avoiding it removes the ~hundred-ms tail that made the
      // ChannelAgentsModal Add button feel laggy on mobile after the
      // optimistic flip.
      await apiInvoke("set_channel_agent_membership", {
        channelId,
        agentId,
        member,
      });
    } catch (err) {
      setAppError(errorMessage(err, "set_channel_agent_membership failed"));
      console.error(err);
      // Refresh in the background to revert the optimistic flip back to
      // whatever the backend actually persisted; we deliberately do not
      // await it so the error toast renders immediately.
      refresh().catch(() => undefined);
    }
  }

  async function createAgent() {
    const preferredHandle = agentDraft.handle.trim() || agentDraft.displayName.trim();
    if (!preferredHandle) return;
    const shouldReturnToCreateChannel = returnToCreateChannelAfterAgent;
    const shouldReturnToMobileHome = createAgentOpenedFromMobileHomeRef.current && !shouldReturnToCreateChannel;
    const handle = availableAgentHandle(preferredHandle, data?.agents ?? []);
    const displayName = agentDraft.displayName.trim() || handle;
    const nextForm = {
      ...agentDraft,
      handle,
      displayName,
      launchCommand: buildPresetCommand({ ...agentDraft, handle, displayName }),
      workingDirectory: agentDraft.workingDirectory.trim() || defaultAgentWorkspace(handle),
    };
    await apiInvoke<string>("create_agent", {
      handle,
      displayName: nextForm.displayName,
      role: nextForm.role,
      runtime: nextForm.runtime,
      model: nextForm.model,
      reasoningEffort: nextForm.reasoningEffort,
      serviceTier: nextForm.serviceTier,
      avatar: nextForm.avatar,
      description: nextForm.description,
      launchCommand: nextForm.launchCommand,
      environmentVariables: nextForm.environmentVariables,
      workingDirectory: nextForm.workingDirectory,
      dailyBudgetMicros: budgetMicrosFromForm(nextForm.dailyBudgetUsd),
    });
    await refresh();
    setAgentDraft(newAgentDraft());
    setShowCreateAgentModal(false);
    setReturnToCreateChannelAfterAgent(false);
    createAgentOpenedFromMobileHomeRef.current = false;
    if (shouldReturnToCreateChannel) {
      setShowCreateChannelModal(true);
      return;
    }
    if (shouldReturnToMobileHome) {
      returnToMobileHome();
    }
  }

  function updateDraftRuntime(runtime: string) {
    const preset = RUNTIME_PRESETS[runtime];
    const currentPreset = RUNTIME_PRESETS[agentDraft.runtime];
    const shouldReplaceModel =
      !agentDraft.model.trim() ||
      !preset?.models.includes(agentDraft.model) ||
      (currentPreset && agentDraft.model === currentPreset.defaultModel);
    setAgentDraft({
      ...agentDraft,
      runtime,
      model: preset && shouldReplaceModel ? preset.defaultModel : agentDraft.model,
      reasoningEffort: runtime === "codex" ? agentDraft.reasoningEffort || "medium" : "",
      serviceTier: runtime === "codex" ? agentDraft.serviceTier : "",
    });
  }

  function updateEditRuntime(runtime: string) {
    const preset = RUNTIME_PRESETS[runtime];
    const currentPreset = RUNTIME_PRESETS[agentEdit.runtime];
    const shouldReplaceModel =
      !agentEdit.model.trim() ||
      !preset?.models.includes(agentEdit.model) ||
      (currentPreset && agentEdit.model === currentPreset.defaultModel);
    setAgentEdit({
      ...agentEdit,
      runtime,
      model: preset && shouldReplaceModel ? preset.defaultModel : agentEdit.model,
      reasoningEffort: runtime === "codex" ? agentEdit.reasoningEffort || "medium" : "",
      serviceTier: runtime === "codex" ? agentEdit.serviceTier : "",
    });
  }

  function startEditAgent(agent: Agent) {
    setEditingAgentId(agent.id);
    setAgentEdit({
      handle: agent.handle,
      displayName: agent.display_name,
      role: agent.role || "agent",
      avatar: agent.avatar || "",
      runtime: agent.runtime,
      model: agent.model,
      reasoningEffort: agent.reasoning_effort || (agent.runtime === "codex" ? "medium" : ""),
      serviceTier: agent.service_tier || "",
      description: agent.description,
      launchCommand: agent.launch_command,
      environmentVariables: agent.environment_variables,
      workingDirectory: agent.working_directory,
      dailyBudgetUsd: budgetUsdFromMicros(agent.daily_budget_micros),
    });
  }

  async function saveAgent() {
    if (!editingAgentId || !(agentEdit.handle.trim() || agentEdit.displayName.trim())) return;
    const handle = availableAgentHandle(
      agentEdit.handle.trim() || agentEdit.displayName.trim(),
      data?.agents ?? [],
      editingAgentId,
    );
    const displayName = agentEdit.displayName.trim() || handle;
    const nextForm = {
      ...agentEdit,
      handle,
      displayName,
      launchCommand: buildPresetCommand({ ...agentEdit, handle, displayName }),
      workingDirectory: agentEdit.workingDirectory.trim(),
    };
    await mutate("update_agent", {
      agentId: editingAgentId,
      handle: nextForm.handle,
      displayName: nextForm.displayName || nextForm.handle,
      role: nextForm.role,
      runtime: nextForm.runtime,
      model: nextForm.model,
      reasoningEffort: nextForm.reasoningEffort,
      serviceTier: nextForm.serviceTier,
      avatar: nextForm.avatar,
      description: nextForm.description,
      launchCommand: nextForm.launchCommand,
      environmentVariables: nextForm.environmentVariables,
      workingDirectory: nextForm.workingDirectory,
      dailyBudgetMicros: budgetMicrosFromForm(nextForm.dailyBudgetUsd),
    });
    setEditingAgentId(null);
    setAgentEdit(EMPTY_AGENT_FORM);
  }

  function cancelEditAgent() {
    setEditingAgentId(null);
    setAgentEdit(EMPTY_AGENT_FORM);
  }

  async function deleteAgent(agent: Agent) {
    const agentDm = data?.channels.find((item) => item.kind === "dm" && item.dm_agent_id === agent.id) ?? null;
    const fallbackChannelId = data?.channels.find((item) => item.id !== agentDm?.id)?.id ?? null;
    setConfirmRequest({
      title: `Delete @${agent.handle}?`,
      body: "This removes the agent profile, DM, schedules, runtime sessions, runs, and pending requests. Existing channel messages keep their sender name.",
      confirmLabel: "Delete agent",
      onConfirm: async () => {
        await mutate("delete_agent", { agentId: agent.id });
        if (editingAgentId === agent.id) setEditingAgentId(null);
        if (selectedAgentId === agent.id) setSelectedAgentId(null);
        if (agentDm) forgetChannelThread(agentDm.id);
        if (agentDm && activeChannelId === agentDm.id) {
          setActiveChannelId(fallbackChannelId ?? "");
          openThread(rememberedThreadForChannel(fallbackChannelId ?? ""), fallbackChannelId ?? "");
        }
      },
    });
  }

  async function restartAgent(agent: Agent) {
    await mutate("start_agent", { agentId: agent.id });
  }

  async function sendRootMessage(asTask = false, bodyOverride?: string, attachmentsOverride?: DraftAttachment[]) {
    const rawBody = bodyOverride ?? draft;
    const attachments = attachmentsOverride ?? draftAttachments;
    if (!channel || (!rawBody.trim() && attachments.length === 0)) return;
    const body = rawBody.trim();
    const sendAsTask = channel.kind === "dm" ? false : asTask;
    const optimisticId = addOptimisticOwnerMessage(channel.id, null, body, sendAsTask, attachments);
    updateRootComposerDraft(channel.id, () => EMPTY_COMPOSER_DRAFT);
    try {
      const persistedMessage = await apiInvoke<Message>("send_message", {
        channelId: channel.id,
        threadRootId: null,
        body,
        asTask: sendAsTask,
        attachments: await attachmentUploads(attachments),
      });
      settleOptimisticMessage(optimisticId, persistedMessage);
    } catch (err) {
      removeOptimisticMessage(optimisticId);
      updateRootComposerDraft(channel.id, () => ({ text: body, attachments }));
      const message = errorMessage(err, "Failed to send message");
      setAppError(message);
      console.error(err);
    }
  }

  async function openDmWithAgent(agent: Agent) {
    try {
      const channelId = await apiInvoke<string>("open_dm_with_agent", { agentId: agent.id });
      await refresh();
      setSelectedAgentId(null);
      setActiveChannelId(channelId);
      restoreRememberedThreadForChannel(channelId);
      setActiveTab("chat");
    } catch (err) {
      const message = errorMessage(err, "Failed to open direct message");
      setAppError(message);
      console.error(err);
    }
  }

  async function sendReply(bodyOverride?: string, attachmentsOverride?: DraftAttachment[]) {
    const rawBody = bodyOverride ?? replyDraft;
    const attachments = attachmentsOverride ?? replyAttachments;
    if (!channel || !activeRoot || (!rawBody.trim() && attachments.length === 0)) return;
    const body = rawBody.trim();
    const optimisticId = addOptimisticOwnerMessage(channel.id, activeRoot.id, body, false, attachments);
    updateReplyComposerDraft(activeRoot.id, () => EMPTY_COMPOSER_DRAFT);
    try {
      const persistedMessage = await apiInvoke<Message>("send_message", {
        channelId: channel.id,
        threadRootId: activeRoot.id,
        body,
        asTask: false,
        attachments: await attachmentUploads(attachments),
      });
      settleOptimisticMessage(optimisticId, persistedMessage);
    } catch (err) {
      removeOptimisticMessage(optimisticId);
      updateReplyComposerDraft(activeRoot.id, () => ({ text: body, attachments }));
      const message = errorMessage(err, "Failed to send reply");
      setAppError(message);
      console.error(err);
    }
  }

  async function updateTaskStatus(task: Task, status: string) {
    await mutate("update_task_status", { taskId: task.id, status });
  }

  async function saveTaskTitle(task: Task) {
    const title = (taskTitleDrafts[task.id] ?? task.title).trim();
    if (!title || title === task.title) return;
    await mutate("update_task_title", { taskId: task.id, title });
    setTaskTitleDrafts((current) => {
      const next = { ...current };
      delete next[task.id];
      return next;
    });
  }

  function setTaskTitleDraft(task: Task, title: string) {
    setTaskTitleDrafts((current) => ({ ...current, [task.id]: title }));
  }

  async function claimTask(task: Task, agentId: string) {
    if (task.status === "done") return;
    await mutate("claim_task", { taskId: task.id, agentId: agentId || null });
  }

  function openTask(task: Task) {
    setActiveChannelId(task.channel_id);
    revealThread(task.message_id, task.channel_id);
    setActiveTab("chat");
    setFocusedMessageId(null);
    window.requestAnimationFrame(() => {
      setFocusedMessageId(task.message_id);
    });
  }

  function openWorkItem(item: AgentWorkItem, focusedMessageIdOverride?: string | null) {
    if (item.channel_id) setActiveChannelId(item.channel_id);
    if (item.thread_root_id) {
      revealThread(item.thread_root_id, item.channel_id ?? activeChannelId);
      setActiveTab("chat");
    }
    const agent = data?.agents.find((candidate) => candidate.id === item.agent_id);
    if (agent) setSelectedAgentId(agent.id);
    const focusId = focusedMessageIdOverride === undefined
      ? item.source_message_id
      : focusedMessageIdOverride;
    if (focusId) {
      const messageExists = Boolean(data?.messages.some((message) => message.id === focusId));
      if (messageExists) {
        setFocusedMessageId(focusId);
      } else if (item.channel_id) {
        setAppError("Original message no longer exists");
      }
    }
  }

  function openSearchResult(result: SearchResult) {
    const openedFromSearch = showSearchModal;
    let openedAgentId: string | null = null;
    if (result.agentId) {
      const agent = data?.agents.find((item) => item.id === result.agentId);
      if (agent) {
        openedAgentId = agent.id;
        setSelectedAgentId(agent.id);
      }
    }
    if (result.channelId) selectChannel(result.channelId);
    if (result.threadId) {
      revealThread(result.threadId, result.channelId ?? activeChannelId);
      setActiveTab("chat");
    }
    if (openedFromSearch) {
      replaceNextAppHistoryEntryRef.current = true;
      searchResultThreadIdRef.current = result.threadId ?? null;
      searchResultAgentIdRef.current = result.threadId ? null : openedAgentId;
    }
    setShowSearchModal(false);
  }

  function openSavedMessage(item: SavedMessage) {
    void markSavedMessagesRead();
    selectChannel(item.channel_id);
    revealThread(item.thread_root_id ?? item.message_id, item.channel_id);
    setFocusedMessageId(item.message_id);
    setActiveTab("chat");
    setShowSavedModal(false);
  }

  async function persistReadActivityFeedItems(items: ActivityFeedItem[], readUntil?: string | ((item: ActivityFeedItem) => string)) {
    const reads = items.map((item) => ({
      itemId: item.id,
      dismissedUntil: typeof readUntil === "function" ? readUntil(item) : (readUntil ?? activityFeedItemCutoff(item)),
    }));
    if (reads.length === 0) return;
    await apiInvoke("mark_inbox_items_read", { items: reads });
  }

  async function persistDismissedActivityFeedItems(
    items: ActivityFeedItem[],
    dismissedUntil?: string | ((item: ActivityFeedItem) => string),
  ) {
    const dismissals = items.map((item) => ({
      itemId: item.dismissId,
      dismissedUntil: typeof dismissedUntil === "function" ? dismissedUntil(item) : (dismissedUntil ?? activityFeedItemCutoff(item)),
    }));
    if (dismissals.length === 0) return;
    await apiInvoke("dismiss_inbox_items", { items: dismissals });
  }

  function activityFeedItemCutoff(item: ActivityFeedItem) {
    const itemTime = new Date(item.timestamp).getTime();
    const cutoffTime = Math.max(Date.now(), Number.isFinite(itemTime) ? itemTime : 0);
    return new Date(cutoffTime).toISOString();
  }

  function openActivityFeedItem(item: ActivityFeedItem) {
    if (item.unread) {
      void markActivityFeedItemRead(item);
    }
    const targetThreadId = item.threadId ?? item.messageId;
    if (item.channelId) selectChannel(item.channelId);
    setSelectedAgentId(null);
    setActiveTab("chat");
    if (targetThreadId) {
      revealThread(targetThreadId, item.channelId ?? activeChannelId);
    } else {
      openThread(null, item.channelId ?? activeChannelId);
      setShowThread(false);
    }
    if (item.messageId) {
      setFocusedMessageId(item.messageId);
    }
    setShowActivityFeedModal(false);
  }

  async function markActivityFeedItemRead(item: ActivityFeedItem) {
    if (!item.unread) return;
    const dismissedUntil = activityFeedItemCutoff(item);
    setReadActivityFeedItems((current) => ({ ...current, [item.id]: dismissedUntil }));
    const operations: Promise<unknown>[] = [persistReadActivityFeedItems([item], dismissedUntil)];
    if (item.threadId) {
      setThreadUnreadCounts((current) => {
        if (!current[item.threadId!]) return current;
        const next = { ...current };
        delete next[item.threadId!];
        return next;
      });
      operations.push(persistThreadReadMarkers([{ threadId: item.threadId, dismissedUntil }]));
    }
    if (item.channelId) {
      setChannelAlertIds((current) => {
        if (!current.has(item.channelId!)) return current;
        const next = new Set(current);
        next.delete(item.channelId!);
        return next;
      });
      operations.push(apiInvoke("mark_channel_read", { channelId: item.channelId }));
    }
    await Promise.all(operations);
    await refresh();
  }

  async function dismissActivityFeedItem(item: ActivityFeedItem) {
    const dismissedUntil = activityFeedItemCutoff(item);
    setDismissedActivityFeedItems((current) => ({ ...current, [item.dismissId]: dismissedUntil }));
    await persistDismissedActivityFeedItems([item], dismissedUntil);
    await refresh();
  }

  async function dismissActivityFeedItems(items: ActivityFeedItem[]) {
    if (items.length === 0) return;
    const cutoffByDismissId = new Map<string, string>();
    for (const item of items) {
      const cutoff = activityFeedItemCutoff(item);
      const existing = cutoffByDismissId.get(item.dismissId);
      if (!existing || new Date(cutoff).getTime() > new Date(existing).getTime()) {
        cutoffByDismissId.set(item.dismissId, cutoff);
      }
    }
    setDismissedActivityFeedItems((current) => {
      const next = { ...current };
      for (const [dismissId, dismissedUntil] of cutoffByDismissId) {
        next[dismissId] = dismissedUntil;
      }
      return next;
    });
    await persistDismissedActivityFeedItems(items, (item) => cutoffByDismissId.get(item.dismissId) ?? item.timestamp);
    await refresh();
  }

  async function markAllActivityFeedRead(items: ActivityFeedItem[]) {
    const markReadItems = items.filter((item) => item.unread);
    if (markReadItems.length === 0) return;
    const cutoffByItemId = new Map(markReadItems.map((item) => [item.id, activityFeedItemCutoff(item)]));
    setReadActivityFeedItems((current) => {
      const next = { ...current };
      for (const item of markReadItems) {
        next[item.id] = cutoffByItemId.get(item.id) ?? item.timestamp;
      }
      return next;
    });
    setChannelAlertIds((current) => {
      const channelIds = new Set(markReadItems.map((item) => item.channelId).filter((id): id is string => Boolean(id)));
      if (channelIds.size === 0) return current;
      const next = new Set(current);
      for (const channelId of channelIds) {
        next.delete(channelId);
      }
      return next;
    });
    setThreadUnreadCounts((current) => {
      const threadIds = new Set(markReadItems.map((item) => item.threadId).filter((id): id is string => Boolean(id)));
      if (threadIds.size === 0) return current;
      const next = { ...current };
      for (const threadId of threadIds) {
        delete next[threadId];
      }
      return next;
    });
    await Promise.all([
      persistReadActivityFeedItems(markReadItems, (item) => cutoffByItemId.get(item.id) ?? item.timestamp),
      persistThreadReadMarkers(
        markReadItems
          .filter((item): item is ActivityFeedItem & { threadId: string } => Boolean(item.threadId))
          .map((item) => ({
            threadId: item.threadId,
            dismissedUntil: cutoffByItemId.get(item.id) ?? item.timestamp,
          })),
      ),
      ...Array.from(
        new Set(markReadItems.map((item) => item.channelId).filter((id): id is string => Boolean(id))),
        (channelId) => apiInvoke("mark_channel_read", { channelId }),
      ),
    ]);
    await refresh();
  }

  function startSidebarResize(event: ReactPointerEvent<HTMLButtonElement>) {
    event.preventDefault();
    const startX = event.clientX;
    const startWidth = sidebarWidth;

    function onPointerMove(moveEvent: PointerEvent) {
      const delta = moveEvent.clientX - startX;
      const rightPanelMinWidth = selectedAgent
        ? MIN_AGENT_DRAWER_WIDTH
        : showThread
          ? MIN_THREAD_PANEL_WIDTH
          : 0;
      const reservedWidth = isCompactDesktopViewport()
        ? MIN_COMPACT_CONTENT_WIDTH
        : MIN_CONVERSATION_WIDTH + rightPanelMinWidth;
      const maxWidth = Math.min(
        MAX_SIDEBAR_WIDTH,
        Math.max(MIN_SIDEBAR_WIDTH, window.innerWidth - reservedWidth),
      );
      const next = Math.min(maxWidth, Math.max(MIN_SIDEBAR_WIDTH, startWidth + delta));
      setSidebarWidth(next);
    }

    function onPointerUp() {
      window.removeEventListener("pointermove", onPointerMove);
      window.removeEventListener("pointerup", onPointerUp);
      document.body.classList.remove("resizing-column");
    }

    document.body.classList.add("resizing-column");
    window.addEventListener("pointermove", onPointerMove);
    window.addEventListener("pointerup", onPointerUp);
  }

  function startThreadResize(event: ReactPointerEvent<HTMLButtonElement>) {
    event.preventDefault();
    const startX = event.clientX;
    const startWidth = threadPanelWidth;

    function onPointerMove(moveEvent: PointerEvent) {
      const delta = startX - moveEvent.clientX;
      const maxWidth = maxThreadPanelWidth(sidebarWidth);
      const next = Math.min(maxWidth, Math.max(MIN_THREAD_PANEL_WIDTH, startWidth + delta));
      setThreadPanelWidth(next);
    }

    function onPointerUp() {
      window.removeEventListener("pointermove", onPointerMove);
      window.removeEventListener("pointerup", onPointerUp);
      document.body.classList.remove("resizing-column");
    }

    document.body.classList.add("resizing-column");
    window.addEventListener("pointermove", onPointerMove);
    window.addEventListener("pointerup", onPointerUp);
  }

  function startAgentDrawerResize(event: ReactPointerEvent<HTMLButtonElement>) {
    event.preventDefault();
    const startX = event.clientX;
    const startWidth = agentDrawerWidth;

    function onPointerMove(moveEvent: PointerEvent) {
      const delta = startX - moveEvent.clientX;
      const maxWidth = maxAgentDrawerWidth(sidebarWidth);
      const next = Math.min(maxWidth, Math.max(MIN_AGENT_DRAWER_WIDTH, startWidth + delta));
      setAgentDrawerWidth(next);
    }

    function onPointerUp() {
      window.removeEventListener("pointermove", onPointerMove);
      window.removeEventListener("pointerup", onPointerUp);
      document.body.classList.remove("resizing-column");
    }

    document.body.classList.add("resizing-column");
    window.addEventListener("pointermove", onPointerMove);
    window.addEventListener("pointerup", onPointerUp);
  }

  // Optimistically toggle saved state so the bookmark button responds instantly,
  // instead of waiting for the backend command + a full bootstrap refresh. The
  // backend still emits `saved_message_updated`, which triggers a later refresh
  // that reconciles the optimistic entry with the authoritative row.
  function toggleMessageSaved(messageId: string, saved: boolean, optimisticEntry?: SavedMessage) {
    // Mark this as the latest intent for this message. A request that fails
    // after a newer toggle landed must NOT roll back, or rapid save→unsave
    // would be clobbered back to the stale state.
    const seq = (savedToggleSeqRef.current.get(messageId) ?? 0) + 1;
    savedToggleSeqRef.current.set(messageId, seq);

    // Optimistically patch only this message's entry. Capture its prior entry
    // (if any) so a failure can restore exactly this one message rather than
    // overwriting the whole list (which could drop other in-flight toggles).
    let priorEntry: SavedMessage | undefined;
    setData((current) => {
      if (!current) return current;
      priorEntry = current.saved_messages.find((item) => item.message_id === messageId);
      const next = savedMessagesWithState(current.saved_messages, messageId, saved, optimisticEntry ?? priorEntry);
      if (next === current.saved_messages) return current;
      return { ...current, saved_messages: next };
    });
    pendingSavedToggleOverridesRef.current.set(messageId, {
      saved,
      entry: saved ? optimisticEntry ?? priorEntry : undefined,
      seq,
    });

    const previousRequest = savedToggleRequestChainsRef.current.get(messageId) ?? Promise.resolve();
    const request = previousRequest
      .catch(() => undefined)
      .then(async () => {
        try {
          await apiInvoke("set_message_saved", { messageId, saved });
        } catch (err) {
          // Only roll back if no newer toggle for this message superseded us.
          if (savedToggleSeqRef.current.get(messageId) === seq) {
            setData((current) => current
              ? { ...current, saved_messages: savedMessagesWithState(current.saved_messages, messageId, Boolean(priorEntry), priorEntry) }
              : current);
            setAppError(errorMessage(err, "set_message_saved failed"));
          }
          console.error(err);
        } finally {
          if (pendingSavedToggleOverridesRef.current.get(messageId)?.seq === seq) {
            pendingSavedToggleOverridesRef.current.delete(messageId);
          }
        }
      })
      .finally(() => {
        if (savedToggleRequestChainsRef.current.get(messageId) === request) {
          savedToggleRequestChainsRef.current.delete(messageId);
        }
      });
    savedToggleRequestChainsRef.current.set(messageId, request);
  }

  async function setMessageSaved(message: Message, saved: boolean) {
    const optimisticEntry: SavedMessage | undefined = saved
      ? {
          id: `optimistic-saved:${message.id}`,
          message_id: message.id,
          channel_id: message.channel_id,
          channel_name: data?.channels.find((channel) => channel.id === message.channel_id)?.name ?? "",
          thread_root_id: message.thread_root_id,
          sender_name: message.sender_name,
          sender_role: message.sender_role,
          body: message.body,
          message_created_at: message.created_at,
          created_at: new Date().toISOString(),
        }
      : undefined;
    await toggleMessageSaved(message.id, saved, optimisticEntry);
  }

  async function unsaveSavedMessage(item: SavedMessage) {
    await toggleMessageSaved(item.message_id, false);
  }

  async function installSupervisorService() {
    await mutate("install_supervisor_service");
  }

  async function uninstallSupervisorService() {
    await mutate("uninstall_supervisor_service");
  }

  if (!data || !bootReady) {
    return (
      <BootSplash
        appError={appError}
        onRetry={() => refreshWithError(`Failed to load ${APP_DISPLAY_NAME} state`)}
      />
    );
  }

  return (
    <main
      className={`app theme-liquid ${selectedAgent || showThread ? "" : "thread-hidden"} ${selectedAgent || activeThreadId ? "right-panel-active" : ""} ${showMobileSidebar ? "mobile-sidebar-open" : ""} ${mobileDragSurface === "sidebar" ? "mobile-sidebar-dragging" : ""} ${mobileDragSurface === "panel" ? "mobile-panel-dragging" : ""} ${mobileComposerFocused ? "mobile-composer-focused" : ""}`}
      style={{
        "--sidebar-width": `${sidebarWidth}px`,
        "--thread-width": `${selectedAgent ? agentDrawerWidth : threadPanelWidth}px`,
        "--mobile-sidebar-drag": `${mobileSidebarDragPx}px`,
        ...UI_TYPE_SCALE[chatTextSize],
      } as CSSProperties}
    >
      <Sidebar
        data={data}
        channel={channel}
        activityFeedUnreadCount={activityFeedUnreadCount}
        savedUnreadCount={savedUnreadCount}
        openSearch={openSearchModal}
        openActivityFeed={openActivityFeedModal}
        openSaved={openSavedModal}
        mobileFocus={mobileSidebarFocus}
        openCreateChannelModal={() => {
          const openedFromMobileHome = isMobileHomeOpen();
          createChannelOpenedFromMobileHomeRef.current = openedFromMobileHome;
          if (!openedFromMobileHome) {
            setShowMobileSidebar(false);
          }
          setReturnToCreateChannelAfterAgent(false);
          setShowCreateChannelModal(true);
        }}
        selectChannel={(channelId) => {
          setShowMobileSidebar(false);
          setMobileSidebarFocus("home");
          setMobileSidebarDragPx(0);
          selectChannel(channelId);
        }}
        openCreateAgentModal={() => {
          const openedFromMobileHome = isMobileHomeOpen();
          setAgentDraft(newAgentDraft());
          setReturnToCreateChannelAfterAgent(false);
          createAgentOpenedFromMobileHomeRef.current = openedFromMobileHome;
          if (!openedFromMobileHome) {
            setShowMobileSidebar(false);
          }
          setShowCreateAgentModal(true);
        }}
        openDmWithAgent={(agent) => {
          setShowMobileSidebar(false);
          openDmWithAgent(agent);
        }}
        openAgentDetail={(agent) => {
          setShowMobileSidebar(false);
          setMobileSidebarDragPx(0);
          setSelectedAgentId(agent.id);
        }}
        openOwnerProfileModal={() => {
          setOwnerProfileDraft(ownerProfileToForm(data.owner_profile));
          if (!isMobileHomeOpen()) {
            setShowMobileSidebar(false);
          }
          setShowOwnerProfileModal(true);
        }}
        openSettingsModal={() => {
          if (!isMobileHomeOpen()) {
            setShowMobileSidebar(false);
          }
          setShowSettingsModal(true);
        }}
        onResizeStart={startSidebarResize}
      />
      <SearchModal
        open={showSearchModal}
        query={searchQuery}
        scope={searchScope}
        timeRange={searchTimeRange}
        results={searchResults}
        agents={data.agents}
        ownerProfile={data.owner_profile}
        onQueryChange={setSearchQuery}
        onScopeChange={setSearchScope}
        onTimeRangeChange={setSearchTimeRange}
        onOpenResult={openSearchResult}
        onClear={() => setSearchQuery("")}
        onClose={() => closeAppModal(() => setShowSearchModal(false))}
      />

      <ActivityFeedModal
        open={showActivityFeedModal}
        items={activityFeedItems}
        agents={data.agents}
        ownerProfile={data.owner_profile}
        onOpenItem={openActivityFeedItem}
        onMarkItemRead={markActivityFeedItemRead}
        onDismissItem={dismissActivityFeedItem}
        onDismissItems={dismissActivityFeedItems}
        onMarkAllRead={markAllActivityFeedRead}
        onClose={() => closeAppModal(() => setShowActivityFeedModal(false))}
      />

      <SavedMessagesModal
        open={showSavedModal}
        items={data.saved_messages}
        agents={data.agents}
        ownerProfile={data.owner_profile}
        onOpenItem={openSavedMessage}
        onUnsaveItem={unsaveSavedMessage}
        onClose={() => closeAppModal(() => setShowSavedModal(false))}
      />

      <OwnerProfileModal
        open={showOwnerProfileModal}
        form={ownerProfileDraft}
        onChange={setOwnerProfileDraft}
        onCancel={() => {
          setOwnerProfileDraft(ownerProfileToForm(data.owner_profile));
          setShowOwnerProfileModal(false);
        }}
        onSubmit={saveOwnerProfile}
      />

      <SettingsModal
        open={showSettingsModal}
        themePreference={themePreference}
        chatTextSize={chatTextSize}
        showImageThumbnails={showImageThumbnails}
        onThemePreferenceChange={setThemePreference}
        onChatTextSizeChange={setChatTextSize}
        onShowImageThumbnailsChange={setShowImageThumbnails}
        onClose={() => setShowSettingsModal(false)}
      />

      <Conversation
        channel={channel}
        channels={data.channels}
        agents={data.agents}
        ownerProfile={data.owner_profile}
        agentActivities={data.agent_activities}
        agentRuns={data.agent_runs}
        agentWorkItems={data.agent_work_items}
        channelAgents={channelAgents}
        activeTab={activeTab}
        activeRoot={activeRoot}
        rootMessages={rootMessages}
        threadReplyCounts={threadReplyCounts}
        threadUnreadCounts={threadUnreadCounts}
        threadReplySummaries={threadReplySummaries}
        visibleTasks={visibleTasks}
        draft={draft}
        draftAttachments={draftAttachments}
        taskTitleDrafts={taskTitleDrafts}
        setActiveTab={setActiveTab}
        setActiveThreadId={revealThread}
        openMobileSidebar={openMobileSidebarFromContent}
        canNavigateBack={appHistoryIndex > 0}
        canNavigateForward={appHistoryIndex < appHistoryMaxIndex}
        navigateBack={() => navigateBack(() => {})}
        navigateForward={navigateForward}
        openChannelSettingsModal={() => setShowChannelSettingsModal(true)}
        deleteChannel={deleteChannel}
        openChannelAgentsModal={() => setShowChannelAgentsModal(true)}
        taskForMessage={taskForMessage}
        setTaskTitleDraft={setTaskTitleDraft}
        saveTaskTitle={saveTaskTitle}
        claimTask={claimTask}
        updateTaskStatus={updateTaskStatus}
        openTask={openTask}
        setDraft={setDraft}
        addDraftAttachments={(files) => appendDraftAttachments(files, "root")}
        removeDraftAttachment={(id) => updateRootComposerDraft(activeChannelId, (current) => ({
          ...current,
          attachments: current.attachments.filter((item) => item.id !== id),
        }))}
        sendRootMessage={sendRootMessage}
        openAgentDetail={(agent) => setSelectedAgentId(agent.id)}
        openArtifact={openArtifact}
        openWorkItem={openWorkItem}
        shareBaseUrl={shareBaseUrl}
        savedMessageIds={savedMessageIds}
        focusedMessageId={focusedMessageId}
        showImageThumbnails={showImageThumbnails}
        onToggleMessageSaved={setMessageSaved}
      />

      {selectedAgent ? (
        <AgentDetailDrawer
          agent={selectedAgent}
          activeRun={selectedAgentRun}
          phase={selectedAgentPhase}
          activities={selectedAgentActivities}
          performance={selectedAgentPerformance}
          workItems={selectedAgentWorkItems}
          reminders={data.reminders}
          onClose={closeSelectedAgent}
          onDelete={deleteAgent}
          onEdit={(agent) => {
            startEditAgent(agent);
          }}
          onRestart={restartAgent}
          channels={data.channels}
          messages={data.messages}
          onOpenWorkItem={(item, focusedMessageIdOverride) => {
            openWorkItem(item, focusedMessageIdOverride);
            setSelectedAgentId(null);
          }}
          onResizeStart={startAgentDrawerResize}
        />
      ) : showThread && (
        <ThreadPanel
          channel={channel}
          channels={data.channels}
          agents={data.agents}
          channelAgents={channelAgents}
          ownerProfile={data.owner_profile}
          agentActivities={data.agent_activities}
          agentRuns={data.agent_runs}
          agentWorkItems={data.agent_work_items}
          activeRoot={activeRoot}
          activeTask={activeTask}
          replies={replies}
          unreadCount={activeThreadId ? threadUnreadCounts[activeThreadId] ?? 0 : 0}
          taskTitleDrafts={taskTitleDrafts}
          replyDraft={replyDraft}
          replyAttachments={replyAttachments}
          onClose={closeThreadPanel}
          setTaskTitleDraft={setTaskTitleDraft}
          saveTaskTitle={saveTaskTitle}
          claimTask={claimTask}
          updateTaskStatus={updateTaskStatus}
          setReplyDraft={setReplyDraft}
          addReplyAttachments={(files) => appendDraftAttachments(files, "reply")}
          removeReplyAttachment={(id) => updateReplyComposerDraft(activeThreadId, (current) => ({
            ...current,
            attachments: current.attachments.filter((item) => item.id !== id),
          }))}
          sendReply={sendReply}
          openAgentDetail={(agent) => setSelectedAgentId(agent.id)}
          openArtifact={openArtifact}
          openWorkItem={openWorkItem}
          onLocateRoot={revealThreadRootInChannel}
          shareBaseUrl={shareBaseUrl}
          savedMessageIds={savedMessageIds}
          focusedMessageId={focusedMessageId}
          showImageThumbnails={showImageThumbnails}
          onToggleMessageSaved={setMessageSaved}
          onResizeStart={startThreadResize}
        />
      )}

      <nav className="mobile-bottom-nav" aria-label="Primary mobile navigation">
        <button
          type="button"
          className={showMobileSidebar && mobileSidebarFocus === "home" ? "active" : ""}
          onClick={openMobileHome}
        >
          <Home size={20} />
          <span>Home</span>
        </button>
        <button
          type="button"
          className={`${showActivityFeedModal ? "active" : ""} ${activityFeedUnreadCount ? "has-unread" : ""}`}
          onClick={openActivityFeedModal}
        >
          <span className="mobile-bottom-nav-icon">
            <Inbox size={20} />
            {activityFeedUnreadCount > 0 && <UnreadBadge value={activityFeedUnreadCount} />}
          </span>
          <span>Activity</span>
        </button>
        <button
          type="button"
          className={`${showSavedModal ? "active" : ""} ${savedUnreadCount ? "has-unread" : ""}`}
          onClick={openSavedModal}
        >
          <span className="mobile-bottom-nav-icon">
            <Bookmark size={20} />
            {savedUnreadCount > 0 && <UnreadBadge value={savedUnreadCount} />}
          </span>
          <span>Saved</span>
        </button>
        <button
          type="button"
          className={showSearchModal ? "active" : ""}
          onClick={openSearchModal}
        >
          <Search size={20} />
          <span>Search</span>
        </button>
      </nav>

      {appError && (
        <div className="app-toast error" role="alert">
          <span>{appError}</span>
          <button onClick={() => setAppError(null)} aria-label="Dismiss error">Dismiss</button>
        </div>
      )}

      <CreateChannelModal
        open={showCreateChannelModal}
        channelName={newChannel}
        nameError={newChannelNameError}
        agents={data.agents}
        selectedAgentIds={newChannelAgentIds}
        submitting={createChannelSubmitting}
        onChange={(value) => {
          setNewChannel(value);
          setNewChannelNameSubmitError(null);
        }}
        onToggleAgent={(agentId, member) => {
          setNewChannelAgentIds((current) => {
            const next = new Set(current);
            if (member) next.add(agentId);
            else next.delete(agentId);
            return next;
          });
        }}
        onCreateAgent={() => {
          setAgentDraft(newAgentDraft());
          setReturnToCreateChannelAfterAgent(true);
          createAgentOpenedFromMobileHomeRef.current = false;
          setNewChannelNameSubmitError(null);
          setShowCreateChannelModal(false);
          setShowCreateAgentModal(true);
        }}
        onCancel={() => {
          setShowCreateChannelModal(false);
          createChannelOpenedFromMobileHomeRef.current = false;
          setReturnToCreateChannelAfterAgent(false);
          setNewChannelNameSubmitError(null);
          setNewChannelAgentIds(new Set());
        }}
        onSubmit={createChannel}
      />

      <ChannelSettingsModal
        open={showChannelSettingsModal}
        channel={channel}
        nameDraft={channelNameDraft}
        descriptionDraft={channelDescriptionDraft}
        onNameChange={setChannelNameDraft}
        onDescriptionChange={setChannelDescriptionDraft}
        onCancel={() => setShowChannelSettingsModal(false)}
        onSave={saveChannel}
      />

      <ConfirmModal
        open={Boolean(confirmRequest)}
        title={confirmRequest?.title ?? ""}
        body={confirmRequest?.body ?? ""}
        confirmLabel={confirmRequest?.confirmLabel ?? "Confirm"}
        onCancel={() => setConfirmRequest(null)}
        onConfirm={confirmRequest?.onConfirm ?? (() => {})}
      />

      <ChannelAgentsModal
        open={showChannelAgentsModal}
        channel={channel}
        agents={data.agents}
        channelMemberIds={channelMemberIds}
        onSetMember={setChannelMember}
        onCreateAgent={() => {
          setAgentDraft(newAgentDraft());
          setReturnToCreateChannelAfterAgent(false);
          createAgentOpenedFromMobileHomeRef.current = false;
          setShowChannelAgentsModal(false);
          setShowCreateAgentModal(true);
        }}
        onClose={() => setShowChannelAgentsModal(false)}
      />

      <AgentFormModal
        open={showCreateAgentModal}
        title="Add Agent"
        form={agentDraft}
        runtimeChecks={runtimeChecks}
        submitLabel="Add agent"
        createMode
        onChange={setAgentDraft}
        onRuntimeChange={updateDraftRuntime}
        onCancel={() => {
          const shouldReturnToCreateChannel = returnToCreateChannelAfterAgent;
          setAgentDraft(newAgentDraft());
          setShowCreateAgentModal(false);
          setReturnToCreateChannelAfterAgent(false);
          createAgentOpenedFromMobileHomeRef.current = false;
          if (shouldReturnToCreateChannel) {
            setShowCreateChannelModal(true);
          }
        }}
        onSubmit={createAgent}
      />

      <AgentFormModal
        open={Boolean(editingAgentId)}
        title="Edit Agent"
        form={agentEdit}
        runtimeChecks={runtimeChecks}
        submitLabel="Save"
        showNotes
        onChange={setAgentEdit}
        onRuntimeChange={updateEditRuntime}
        onCancel={cancelEditAgent}
        onSubmit={saveAgent}
      />

    </main>
  );
}

const app = (
  <AppErrorBoundary>
    <App />
  </AppErrorBoundary>
);

createRoot(document.getElementById("root")!).render(
  shouldEnableBenchProfiler()
    ? <Profiler id="LantorApp" onRender={recordBenchCommit}>{app}</Profiler>
    : app,
);
