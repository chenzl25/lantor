import {
  ArrowLeft,
  ArrowRight,
  CheckCircle2,
  ChevronRight,
  Flag,
  Hash,
  Bookmark,
  LayoutList,
  MessageSquare,
  Paperclip,
  Send,
  Settings,
  Trash2,
  UserPlus,
} from "lucide-react";
import { Fragment, useEffect, useLayoutEffect, useMemo, useRef, useState, type ClipboardEvent, type DragEvent, type FocusEvent, type KeyboardEvent, type MouseEvent as ReactMouseEvent, type PointerEvent as ReactPointerEvent, type WheelEvent as ReactWheelEvent } from "react";
import { useAutoGrowTextarea } from "../hooks/useAutoGrowTextarea";
import { useMentionPicker } from "../hooks/useMentionPicker";
import { useMobileViewport } from "../hooks/useMobileViewport";
import { isImeComposing } from "../input-utils";
import { mentionableAgentsForChannel } from "../mentions";
import { copyText } from "../clipboard";
import { APP_DISPLAY_NAME } from "../branding";
import { isCompactFollowupMessage, wasEdited } from "../message-grouping";
import { DESKTOP_MESSAGE_PREVIEW_CHARS, DESKTOP_MESSAGE_PREVIEW_LINES } from "../message-preview";
import { messageShareLink, messageToMarkdown } from "../message-share";
import { Agent, AgentActivity, AgentRun, AgentWorkItem, Artifact, Channel, DraftAttachment, Message, OwnerProfile, TASK_STATUSES, Task, ThreadReplySummary } from "../types";
import { agentForMessageSender, deletedAgentForMessageSender, formatClockTime, formatDateDivider, formatTime, isSameCalendarDay, ownerAsAvatarAgent, visibleAgentDescription, visibleChannelDescription } from "../ui-utils";
import { ActivityProgressDock, activeProgressByAgent } from "./ActivityProgressDock";
import { AgentAvatar, AgentAvatarWithProfile } from "./AgentAvatar";
import { DraftAttachmentsPreview } from "./DraftAttachmentsPreview";
import { MessageActionMenu } from "./MessageActionMenu";
import { MessageAttachments } from "./MessageAttachments";
import { MessageArtifacts } from "./MessageArtifacts";
import { MessageMarkdown } from "./MessageMarkdown";
import { TaskAssigneePicker } from "./TaskAssigneePicker";

type ConversationProps = {
  channel: Channel | null;
  channels: Channel[];
  agents: Agent[];
  ownerProfile: OwnerProfile;
  agentActivities: AgentActivity[];
  agentRuns: AgentRun[];
  agentWorkItems: AgentWorkItem[];
  channelAgents: Agent[];
  activeTab: "chat" | "tasks";
  activeRoot: Message | null;
  rootMessages: Message[];
  threadReplyCounts: Record<string, number>;
  threadUnreadCounts: Record<string, number>;
  threadReplySummaries: Record<string, ThreadReplySummary>;
  visibleTasks: Task[];
  draft: string;
  draftAttachments: DraftAttachment[];
  taskTitleDrafts: Record<string, string>;
  setActiveTab: (tab: "chat" | "tasks") => void;
  setActiveThreadId: (threadId: string | null) => void;
  openMobileSidebar: () => void;
  canNavigateBack: boolean;
  canNavigateForward: boolean;
  navigateBack: () => void;
  navigateForward: () => void;
  openChannelSettingsModal: () => void;
  deleteChannel: () => void;
  openChannelAgentsModal: () => void;
  taskForMessage: (messageId: string) => Task | null;
  setTaskTitleDraft: (task: Task, title: string) => void;
  saveTaskTitle: (task: Task) => void;
  claimTask: (task: Task, agentId: string) => void;
  updateTaskStatus: (task: Task, status: string) => void;
  openTask: (task: Task) => void;
  setDraft: (value: string) => void;
  addDraftAttachments: (files: FileList | File[]) => void;
  removeDraftAttachment: (id: string) => void;
  sendRootMessage: (asTask?: boolean, bodyOverride?: string, attachmentsOverride?: DraftAttachment[]) => void;
  openAgentDetail: (agent: Agent) => void;
  openArtifact: (artifact: Artifact) => void;
  openWorkItem?: (item: AgentWorkItem, focusedMessageIdOverride?: string | null) => void;
  shareBaseUrl: string | null;
  savedMessageIds: Set<string>;
  focusedMessageId: string | null;
  showImageThumbnails: boolean;
  onToggleMessageSaved: (message: Message, saved: boolean) => void;
};

type MessageMenuState = {
  x: number;
  y: number;
  message: Message;
} | null;

const MESSAGE_CARD_INTERACTIVE_TARGET_SELECTOR = [
  "a",
  "button",
  "input",
  "select",
  "textarea",
  "summary",
  "[contenteditable='true']",
  "[role='button']",
  "[role='link']",
  ".message-artifacts",
  ".message-attachments",
].join(",");

function taskStatusLabel(status: string) {
  return status.replace("_", " ");
}

type ReplyProgress = ReturnType<typeof activeProgressByAgent>[number];
type ActiveReplyMenuPlacement = "above" | "below";

function compactReplyProgressText(value: string, limit: number) {
  const normalized = value.replace(/\s+/g, " ").trim();
  if (!normalized) return "";
  if (normalized.length <= limit) return normalized;
  return `${normalized.slice(0, Math.max(0, limit - 1)).trim()}...`;
}

function userFacingReplyProgressTitle(value: string) {
  const title = value.trim() || "Working";
  const lowered = title.toLowerCase();
  if (lowered.includes("warm app-server ready") || lowered.includes("warm stream-json ready")) return "Runtime ready";
  if (lowered === "started working" || lowered === "run started" || lowered === "run created") return "Working";
  return title;
}

function userFacingReplyProgressDetail(value: string) {
  const detail = value.trim();
  if (!detail || detail.startsWith("{") || detail.startsWith("[")) return "";
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
        .map(([key, item]) => `${key.replace(/_/g, " ")} ${item}`)
        .join(", ");
    }
  }
  if (detail === "pid unavailable") return "";
  return detail;
}

function replyProgressSummary(progress: ReplyProgress) {
  if (progress.latestActivity) {
    const title = userFacingReplyProgressTitle(progress.latestActivity.summary || progress.latestActivity.title || "Working");
    const detail = compactReplyProgressText(userFacingReplyProgressDetail(progress.latestActivity.detail), 72);
    return {
      title,
      detail: detail && detail !== title ? detail : "",
    };
  }
  if (progress.state === "queued" && progress.queuedItems.length > 0) {
    return {
      title: progress.queuedItems.length === 1 ? "Queued" : `${progress.queuedItems.length} queued`,
      detail: "Waiting to start",
    };
  }
  return {
    title: "Working",
    detail: "",
  };
}

function ActiveReplyIndicator({ label }: { label: string }) {
  return (
    <span className="thread-reply-status-text" aria-label={label}>
      <span className="thread-reply-status-dot" aria-hidden="true" />
      <span className="thread-reply-status-dot" aria-hidden="true" />
      <span className="thread-reply-status-dot" aria-hidden="true" />
    </span>
  );
}

function shouldCollapseChannelMessage(body: string) {
  const text = body.trim();
  if (!text) return false;
  return text.split("\n").length > DESKTOP_MESSAGE_PREVIEW_LINES || text.length > DESKTOP_MESSAGE_PREVIEW_CHARS;
}

function closeUnbalancedCodeFence(body: string) {
  const fenceMatches = body.match(/(^|\n)```/g);
  if (!fenceMatches || fenceMatches.length % 2 === 0) return body;
  return `${body.replace(/\s+$/, "")}\n\`\`\``;
}

function channelMessagePreview(body: string) {
  const text = body.trim();
  const lines = text.split("\n");
  const linePreview = lines.slice(0, DESKTOP_MESSAGE_PREVIEW_LINES).join("\n");
  const preview = linePreview.length > DESKTOP_MESSAGE_PREVIEW_CHARS
    ? `${linePreview.slice(0, DESKTOP_MESSAGE_PREVIEW_CHARS).replace(/\s+\S*$/, "")}`
    : linePreview;
  return closeUnbalancedCodeFence(preview);
}

function isInteractiveMessageClick(event: ReactMouseEvent<HTMLElement>) {
  if (event.nativeEvent.composedPath().some((node) => (
    node instanceof Element && node.matches(MESSAGE_CARD_INTERACTIVE_TARGET_SELECTOR)
  ))) {
    return true;
  }
  return event.target instanceof Element
    && Boolean(event.target.closest(MESSAGE_CARD_INTERACTIVE_TARGET_SELECTOR));
}

export function Conversation({
  channel,
  channels,
  agents,
  ownerProfile,
  agentActivities,
  agentRuns,
  agentWorkItems,
  channelAgents,
  activeTab,
  activeRoot,
  rootMessages,
  threadReplyCounts,
  threadUnreadCounts,
  threadReplySummaries,
  visibleTasks,
  draft,
  draftAttachments,
  taskTitleDrafts,
  setActiveTab,
  setActiveThreadId,
  openMobileSidebar,
  canNavigateBack,
  canNavigateForward,
  navigateBack,
  navigateForward,
  openChannelSettingsModal,
  deleteChannel,
  openChannelAgentsModal,
  taskForMessage,
  setTaskTitleDraft,
  saveTaskTitle,
  claimTask,
  updateTaskStatus,
  openTask,
  setDraft,
  addDraftAttachments,
  removeDraftAttachment,
  sendRootMessage,
  openAgentDetail,
  openArtifact,
  openWorkItem,
  shareBaseUrl,
  savedMessageIds,
  focusedMessageId,
  showImageThumbnails,
  onToggleMessageSaved,
}: ConversationProps) {
  const [showChannelActions, setShowChannelActions] = useState(false);
  const [messageMenu, setMessageMenu] = useState<MessageMenuState>(null);
  const [expandedChannelMessageIds, setExpandedChannelMessageIds] = useState<Set<string>>(() => new Set());
  const [activeReplyMenuPlacementByMessageId, setActiveReplyMenuPlacementByMessageId] = useState<Record<string, ActiveReplyMenuPlacement>>({});
  const messageListRef = useRef<HTMLDivElement | null>(null);
  const messageListContentRef = useRef<HTMLDivElement | null>(null);
  const messageListBottomAnchorRef = useRef<HTMLDivElement | null>(null);
  const bottomScrollFrameRef = useRef<number | null>(null);
  const bottomScrollTimeoutRef = useRef<number | null>(null);
  const shouldFollowMessagesRef = useRef(true);
  const userMessageScrollUntilRef = useRef(0);
  const messageListMetricsRef = useRef({ scrollHeight: 0, scrollTop: 0, clientHeight: 0 });
  const channelActionsRef = useRef<HTMLDivElement | null>(null);
  const isDm = channel?.kind === "dm";
  const dmAgent = isDm ? agents.find((agent) => agent.id === channel?.dm_agent_id) ?? null : null;
  function openLinkedAgentDetail(handle: string) {
    const agent = agents.find((candidate) => candidate.handle.toLowerCase() === handle.toLowerCase());
    if (agent) openAgentDetail(agent);
  }
  const activeReplyProgressByRoot = useMemo<Record<string, ReturnType<typeof activeProgressByAgent>>>(() => {
    if (!channel) return {};
    return Object.fromEntries(
      rootMessages
        .map((message) => [
          message.id,
          activeProgressByAgent(
            [],
            agentActivities,
            agentRuns,
            agentWorkItems,
            agents,
            channel.id,
            message.id,
          ),
        ] as const)
        .filter(([, progress]) => progress.length > 0),
    );
  }, [agentActivities, agentRuns, agentWorkItems, agents, channel, rootMessages]);
  const messageListProgressVersion = useMemo(() => {
    if (!channel) return "";
    const rootMessageIds = new Set(rootMessages.map((message) => message.id));
    const relevantWorkItems = agentWorkItems.filter((workItem) => (
      workItem.channel_id === channel.id
      && ((workItem.thread_root_id ?? null) === null || rootMessageIds.has(workItem.thread_root_id ?? ""))
    ));
    const relevantRunIds = new Set(relevantWorkItems.map((workItem) => workItem.run_id).filter(Boolean));
    const relevantRuns = agentRuns.filter((run) => relevantRunIds.has(run.id));
    const relevantActivities = agentActivities.filter((activity) => (
      activity.run_id ? relevantRunIds.has(activity.run_id) : false
    ));
    return [
      ...relevantWorkItems.map((workItem) => (
        `work:${workItem.id}:${workItem.status}:${workItem.updated_at}:${workItem.run_id ?? ""}:${workItem.thread_root_id ?? ""}`
      )),
      ...relevantRuns.map((run) => `run:${run.id}:${run.status}:${run.started_at ?? ""}:${run.stopped_at ?? ""}`),
      ...relevantActivities.map((activity) => (
        `activity:${activity.id}:${activity.status}:${activity.created_at}`
      )),
    ].join("|");
  }, [agentActivities, agentRuns, agentWorkItems, channel, rootMessages]);
  const lastRootMessage = rootMessages[rootMessages.length - 1] ?? null;
  const activeTasks = visibleTasks.filter((task) => task.status !== "done");
  const reviewTasks = visibleTasks.filter((task) => task.status === "in_review");
  const unassignedTasks = visibleTasks.filter((task) => task.status !== "done" && !task.assignee_id);
  const assignedTasks = visibleTasks.filter((task) => task.assignee_id || task.status === "done");
  const taskAssigneeOptions = channelAgents.length > 0 ? channelAgents : agents;
  const mentionAgents = useMemo(
    () => mentionableAgentsForChannel(channel, agents, channelAgents),
    [agents, channel, channelAgents],
  );
  const channelAgentPreview = channelAgents.slice(0, 3);
  const surfaceLabel = channel
    ? isDm
      ? `DM with @${dmAgent?.handle || "agent"}`
      : `#${channel.name}`
    : APP_DISPLAY_NAME;
  function isMessageListAtBottom(element: HTMLDivElement) {
    return messageListDistanceFromBottom(element) < 32;
  }

  function wasMessageListPreviouslyAtBottom() {
    const metrics = messageListMetricsRef.current;
    if (metrics.scrollHeight === 0 && metrics.clientHeight === 0) return true;
    return metrics.scrollHeight - metrics.scrollTop - metrics.clientHeight < 32;
  }

  function messageListDistanceFromBottom(element: HTMLDivElement) {
    return element.scrollHeight - element.scrollTop - element.clientHeight;
  }

  function rememberMessageListMetrics(element: HTMLDivElement) {
    messageListMetricsRef.current = {
      scrollHeight: element.scrollHeight,
      scrollTop: element.scrollTop,
      clientHeight: element.clientHeight,
    };
  }

  function isMessageListViewportOnlyResize(element: HTMLDivElement) {
    const previous = messageListMetricsRef.current;
    return previous.scrollHeight > 0 &&
      previous.scrollHeight === element.scrollHeight &&
      previous.clientHeight !== element.clientHeight;
  }

  function cancelPendingMessageBottomScroll() {
    if (bottomScrollFrameRef.current !== null) {
      window.cancelAnimationFrame(bottomScrollFrameRef.current);
      bottomScrollFrameRef.current = null;
    }
    if (bottomScrollTimeoutRef.current !== null) {
      window.clearTimeout(bottomScrollTimeoutRef.current);
      bottomScrollTimeoutRef.current = null;
    }
  }

  function isUserScrollingMessages() {
    return Date.now() < userMessageScrollUntilRef.current;
  }

  function stopFollowingMessages(element = messageListRef.current) {
    userMessageScrollUntilRef.current = Date.now() + 650;
    shouldFollowMessagesRef.current = false;
    cancelPendingMessageBottomScroll();
    if (element) rememberMessageListMetrics(element);
  }

  function isPointerOnMessageListScrollbar(event: ReactPointerEvent<HTMLDivElement>) {
    const element = event.currentTarget;
    const scrollbarWidth = element.offsetWidth - element.clientWidth;
    if (scrollbarWidth <= 0) return false;
    return event.clientX >= element.getBoundingClientRect().right - scrollbarWidth - 2;
  }

  function scrollMessagesToBottomNow(behavior: ScrollBehavior = "auto") {
    const element = messageListRef.current;
    if (!element) return;
    userMessageScrollUntilRef.current = 0;
    element.scrollTo({ top: element.scrollHeight, behavior });
    if (behavior === "auto") {
      shouldFollowMessagesRef.current = true;
      rememberMessageListMetrics(element);
    }
  }

  function scrollMessagesToBottom(behavior: ScrollBehavior = "auto") {
    scrollMessagesToBottomNow(behavior);
    if (behavior !== "auto") return;
    cancelPendingMessageBottomScroll();
    bottomScrollFrameRef.current = window.requestAnimationFrame(() => {
      bottomScrollFrameRef.current = null;
      if (shouldFollowMessagesRef.current) scrollMessagesToBottomNow();
    });
    bottomScrollTimeoutRef.current = window.setTimeout(() => {
      bottomScrollTimeoutRef.current = null;
      if (shouldFollowMessagesRef.current) scrollMessagesToBottomNow();
    }, 50);
  }

  function handleMessageListScroll() {
    const element = messageListRef.current;
    if (!element) return;
    const atBottom = isMessageListAtBottom(element);
    const layoutChanged =
      messageListMetricsRef.current.scrollHeight !== element.scrollHeight
      || messageListMetricsRef.current.clientHeight !== element.clientHeight;
    const userScrolling = isUserScrollingMessages();
    if (atBottom && !userScrolling) {
      shouldFollowMessagesRef.current = true;
    } else if (!userScrolling && shouldFollowMessagesRef.current && layoutChanged && wasMessageListPreviouslyAtBottom()) {
      scrollMessagesToBottom();
    }
    rememberMessageListMetrics(element);
  }

  function handleMessageListWheel(event: ReactWheelEvent<HTMLDivElement>) {
    if (event.deltaY >= 0) return;
    stopFollowingMessages();
  }

  function handleMessageListPointerDown(event: ReactPointerEvent<HTMLDivElement>) {
    if (!isPointerOnMessageListScrollbar(event)) return;
    stopFollowingMessages(event.currentTarget);
  }

  function handleMessageListTouchMove() {
    stopFollowingMessages();
  }

  function updateActiveReplyMenuPlacement(messageId: string, summaryElement: HTMLElement) {
    const menu = summaryElement.querySelector<HTMLElement>(".thread-reply-active-menu");
    if (!menu) return;

    const boundaryRect = messageListRef.current?.getBoundingClientRect();
    const summaryRect = summaryElement.getBoundingClientRect();
    const menuHeight = menu.offsetHeight || menu.getBoundingClientRect().height;
    const gap = 6;
    const boundaryTop = boundaryRect?.top ?? 0;
    const boundaryBottom = boundaryRect?.bottom ?? window.innerHeight;
    const spaceBelow = boundaryBottom - summaryRect.bottom - gap;
    const spaceAbove = summaryRect.top - boundaryTop - gap;
    const placement: ActiveReplyMenuPlacement = spaceBelow < menuHeight && spaceAbove > spaceBelow
      ? "above"
      : "below";

    setActiveReplyMenuPlacementByMessageId((current) => (
      current[messageId] === placement ? current : { ...current, [messageId]: placement }
    ));
  }

  function handleMessageListContentLoad() {
    if (!shouldFollowMessagesRef.current) return;
    scrollMessagesToBottom();
  }

  function hasSelectedText() {
    return Boolean(window.getSelection()?.toString().trim());
  }

  function isPrimaryUnmodifiedClick(event: ReactMouseEvent<HTMLElement>) {
    return event.button === 0 && !event.ctrlKey && !event.metaKey && !event.altKey && !event.shiftKey;
  }

  function shouldOpenThreadFromMessageClick() {
    return window.matchMedia("(max-width: 760px)").matches;
  }

  function renderReplyParticipantAvatar(message: Message) {
    const agent = agentForMessageSender(message, agents);
    if (agent) return <AgentAvatar agent={agent} size="sm" title={`@${agent.handle}`} showStatus={false} />;
    const deletedAgent = deletedAgentForMessageSender(message);
    if (deletedAgent) return <AgentAvatar agent={deletedAgent} size="sm" title={`@${deletedAgent.handle} has been deleted`} showStatus={false} />;
    if (message.sender_role === "owner") {
      return <AgentAvatar agent={ownerAsAvatarAgent(ownerProfile)} size="sm" showStatus={false} />;
    }
    return <span className="thread-reply-fallback-avatar">{message.sender_name.slice(0, 1)}</span>;
  }

  useEffect(() => {
    if (!isDm) return;
    if (activeTab === "tasks") setActiveTab("chat");
  }, [activeTab, isDm, setActiveTab]);

  useEffect(() => {
    setShowChannelActions(false);
    setMessageMenu(null);
  }, [channel?.id]);

  useEffect(() => {
    if (!showChannelActions) return;
    function handlePointerDown(event: PointerEvent) {
      const root = channelActionsRef.current;
      if (!root) return;
      const target = event.target as Node | null;
      if (target && root.contains(target)) return;
      setShowChannelActions(false);
    }
    function handleKeyDown(event: globalThis.KeyboardEvent) {
      if (event.key !== "Escape") return;
      event.stopPropagation();
      setShowChannelActions(false);
    }
    document.addEventListener("pointerdown", handlePointerDown, true);
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("pointerdown", handlePointerDown, true);
      document.removeEventListener("keydown", handleKeyDown);
    };
  }, [showChannelActions]);

  function handleChannelActionsBlur(event: FocusEvent<HTMLDivElement>) {
    if (event.currentTarget.contains(event.relatedTarget)) return;
    setShowChannelActions(false);
  }

  function shouldUseNativeMessageSelection() {
    return window.matchMedia("(hover: none)").matches;
  }

  async function copyMessageMarkdown(message: Message) {
    await copyText(messageToMarkdown(message, surfaceLabel));
    setMessageMenu(null);
  }

  async function copyMessageLink(message: Message) {
    await copyText(messageShareLink(message, shareBaseUrl));
    setMessageMenu(null);
  }

  function toggleChannelMessageExpanded(messageId: string) {
    setExpandedChannelMessageIds((current) => {
      const next = new Set(current);
      if (next.has(messageId)) {
        next.delete(messageId);
      } else {
        next.add(messageId);
      }
      return next;
    });
  }

  useLayoutEffect(() => {
    shouldFollowMessagesRef.current = true;
    scrollMessagesToBottom();
  }, [channel?.id]);

  useEffect(() => () => {
    if (bottomScrollFrameRef.current !== null) window.cancelAnimationFrame(bottomScrollFrameRef.current);
    if (bottomScrollTimeoutRef.current !== null) window.clearTimeout(bottomScrollTimeoutRef.current);
  }, []);

  useEffect(() => {
    if (activeTab !== "chat") return;
    const root = messageListRef.current;
    const content = messageListContentRef.current;
    const bottomAnchor = messageListBottomAnchorRef.current;
    if (!root || !content || !bottomAnchor) return;
    function keepBottomVisible(source: "viewport" | "content") {
      const list = messageListRef.current;
      if (!list) return;
      if (source === "viewport" && isMessageListViewportOnlyResize(list)) {
        rememberMessageListMetrics(list);
        return;
      }
      if (shouldFollowMessagesRef.current && !isUserScrollingMessages()) {
        scrollMessagesToBottom();
      } else {
        rememberMessageListMetrics(list);
      }
    }
    const observer = typeof ResizeObserver === "undefined"
      ? null
      : new ResizeObserver((entries) => {
        const hasContentResize = entries.some((entry) => entry.target === content);
        keepBottomVisible(hasContentResize ? "content" : "viewport");
      });
    const intersectionObserver = typeof IntersectionObserver === "undefined"
      ? null
      : new IntersectionObserver((entries) => {
        if (!shouldFollowMessagesRef.current) return;
        if (entries.some((entry) => !entry.isIntersecting || entry.intersectionRatio < 1)) {
          keepBottomVisible("viewport");
        }
      }, { root, threshold: 1 });
    const mutationObserver = typeof MutationObserver === "undefined"
      ? null
      : new MutationObserver(() => keepBottomVisible("content"));
    observer?.observe(root);
    observer?.observe(content);
    intersectionObserver?.observe(bottomAnchor);
    mutationObserver?.observe(content, { childList: true, characterData: true, subtree: true });
    const handleWindowResize = () => keepBottomVisible("viewport");
    window.addEventListener("resize", handleWindowResize);
    return () => {
      observer?.disconnect();
      intersectionObserver?.disconnect();
      mutationObserver?.disconnect();
      window.removeEventListener("resize", handleWindowResize);
    };
  }, [activeTab, channel?.id]);

  useEffect(() => {
    setExpandedChannelMessageIds(new Set());
  }, [channel?.id]);

  useLayoutEffect(() => {
    if (!shouldFollowMessagesRef.current) return;
    scrollMessagesToBottom();
  }, [
    activeTab,
    channel?.id,
    messageListProgressVersion,
    rootMessages.length,
    lastRootMessage?.id,
    lastRootMessage?.updated_at,
    lastRootMessage?.delivery_state,
  ]);

  useLayoutEffect(() => {
    if (!focusedMessageId) return;
    let frameId = 0;
    let settleFrameId = 0;
    let attemptsRemaining = 6;
    function scrollFocusedMessage() {
      const list = messageListRef.current;
      const element = list?.querySelector<HTMLElement>(`[data-message-id="${focusedMessageId}"]`);
      if (element) {
        stopFollowingMessages(list);
        element.scrollIntoView({ block: "center" });
        settleFrameId = window.requestAnimationFrame(() => {
          const currentList = messageListRef.current;
          if (currentList) rememberMessageListMetrics(currentList);
        });
        return;
      }
      if (attemptsRemaining <= 0) return;
      attemptsRemaining -= 1;
      frameId = window.requestAnimationFrame(scrollFocusedMessage);
    }
    scrollFocusedMessage();
    return () => {
      if (frameId) window.cancelAnimationFrame(frameId);
      if (settleFrameId) window.cancelAnimationFrame(settleFrameId);
    };
  }, [
    channel?.id,
    focusedMessageId,
    messageListProgressVersion,
    rootMessages.length,
    lastRootMessage?.id,
    lastRootMessage?.updated_at,
    lastRootMessage?.delivery_state,
  ]);

  return (
    <section className="conversation">
      <header className="topbar">
        <button
          type="button"
          className="mobile-nav-button"
          aria-label="Back to navigation"
          onClick={openMobileSidebar}
        >
          <ArrowLeft size={18} />
        </button>
        <div className="desktop-history-controls" aria-label="Navigation history">
          <button
            type="button"
            className="desktop-history-button"
            aria-label="Go back"
            title="Back"
            disabled={!canNavigateBack}
            onClick={navigateBack}
          >
            <ArrowLeft size={17} />
          </button>
          <button
            type="button"
            className="desktop-history-button"
            aria-label="Go forward"
            title="Forward"
            disabled={!canNavigateForward}
            onClick={navigateForward}
          >
            <ArrowRight size={17} />
          </button>
        </div>
        <div className="channel-title">
          {isDm && dmAgent ? (
            <button
              type="button"
              className="hash-card dm-card dm-agent-detail-trigger"
              aria-label={`View @${dmAgent.handle} details`}
              onClick={() => openAgentDetail(dmAgent)}
            >
              <AgentAvatarWithProfile agent={dmAgent} />
            </button>
          ) : (
            <span className="hash-card">
              <Hash />
            </span>
          )}
          <div>
            <h1>{isDm ? dmAgent?.display_name || "Direct Message" : channel?.name || "No channel"}</h1>
            {isDm ? (
              <p title={dmAgent ? `@${dmAgent.handle} · ${dmAgent.runtime} · ${dmAgent.status}` : undefined}>
                {dmAgent ? `@${dmAgent.handle} · ${dmAgent.runtime}` : "Agent no longer exists"}
              </p>
            ) : (
              <p>{channel ? visibleChannelDescription(channel.description) : "Create a channel from the sidebar"}</p>
            )}
          </div>
        </div>
        {channel && !isDm && (
          <div className="channel-header-actions" ref={channelActionsRef} onBlur={handleChannelActionsBlur}>
            <button
              type="button"
              className={`channel-agent-count-trigger ${channelAgents.length === 0 ? "empty" : ""}`}
              title={channelAgents.length === 0 ? "Add agents to this channel" : "Manage channel agents"}
              aria-label={channelAgents.length === 0 ? "Add agents to this channel" : "Manage channel agents"}
              onClick={() => {
                setShowChannelActions(false);
                openChannelAgentsModal();
              }}
            >
              {channelAgentPreview.length > 0 ? (
                <span className="channel-agent-preview" aria-hidden="true">
                  {channelAgentPreview.map((agent) => (
                    <span key={agent.id}>
                      <AgentAvatar agent={agent} size="sm" showStatus={false} title={`@${agent.handle}`} />
                    </span>
                  ))}
                </span>
              ) : (
                <UserPlus size={16} />
              )}
              <span>{channelAgents.length > 0 ? channelAgents.length : "Add agent"}</span>
            </button>
            <button
              type="button"
              className={`channel-action-trigger ${showChannelActions ? "active" : ""}`}
              title="Channel actions"
              aria-label="Channel actions"
              aria-expanded={showChannelActions}
              onClick={() => setShowChannelActions((current) => !current)}
            >
              <Settings size={18} />
            </button>
            {showChannelActions && (
              <div className="channel-actions-menu">
                <button
                  type="button"
                  onClick={() => {
                    setShowChannelActions(false);
                    openChannelSettingsModal();
                  }}
                >
                  <Settings size={15} />
                  <span>Channel settings</span>
                </button>
                <button
                  type="button"
                  className="danger"
                  onClick={() => {
                    setShowChannelActions(false);
                    deleteChannel();
                  }}
                >
                  <Trash2 size={15} />
                  <span>Delete channel</span>
                </button>
              </div>
            )}
          </div>
        )}
      </header>

      <div className="tabs">
        <button className={activeTab === "chat" ? "active" : ""} onClick={() => setActiveTab("chat")}>
          <MessageSquare size={16} /> Chat
        </button>
        {!isDm && (
          <button className={activeTab === "tasks" ? "active" : ""} onClick={() => setActiveTab("tasks")}>
            <LayoutList size={16} /> Tasks
          </button>
        )}
      </div>

      {activeTab === "chat" ? (
        <div className="message-list-shell">
          <div className="message-progress-layer">
            <ActivityProgressDock
              messages={rootMessages}
              activities={agentActivities}
              runs={agentRuns}
              workItems={agentWorkItems}
              agents={agents}
              channelId={channel?.id ?? null}
              threadRootId={null}
              onOpenWorkItem={openWorkItem}
            />
          </div>
          <div
            ref={messageListRef}
            className="message-list"
            onScroll={handleMessageListScroll}
            onWheelCapture={handleMessageListWheel}
            onPointerDownCapture={handleMessageListPointerDown}
            onTouchMoveCapture={handleMessageListTouchMove}
            onLoadCapture={handleMessageListContentLoad}
          >
            <div ref={messageListContentRef} className="message-list-content">
              {channel ? (
                rootMessages.length > 0 ? (
                  <div className="beginning">
                    {isDm ? `Beginning of your DM with @${dmAgent?.handle || "agent"}` : `Beginning of #${channel.name}`}
                  </div>
                ) : (
                  <div className="empty-state">
                    <MessageSquare size={34} />
                    <h2>{isDm ? "No DM messages yet" : "No messages yet"}</h2>
                    <p>
                      {isDm
                        ? "Send a message here to talk directly with this agent."
                        : channelAgents.length === 0
                          ? "Add agents to this channel or send the first message."
                          : "Send a root message from the composer. Replies belong in the right thread pane."}
                    </p>
                    {!isDm && channelAgents.length === 0 && (
                      <button type="button" className="empty-state-action" onClick={openChannelAgentsModal}>
                        <UserPlus size={16} /> Add agent
                      </button>
                    )}
                  </div>
                )
              ) : (
                <div className="empty-state">
                  <Hash size={34} />
                  <h2>No channels yet</h2>
                  <p>Create a channel in the left sidebar, then send messages or tasks.</p>
                </div>
              )}
            {rootMessages.map((message, index) => {
            const linkedTask = taskForMessage(message.id);
            const replyCount = threadReplyCounts[message.id] ?? 0;
            const unreadReplyCount = threadUnreadCounts[message.id] ?? 0;
            const replySummary = threadReplySummaries[message.id] ?? null;
            const activeReplyProgress = activeReplyProgressByRoot[message.id] ?? [];
            const hasActiveReplyProgress = activeReplyProgress.length > 0;
            const activeReplyStatus = hasActiveReplyProgress
              ? replyProgressSummary(activeReplyProgress[0]).title
              : "";
            const activeReplyMenuPlacement = activeReplyMenuPlacementByMessageId[message.id] ?? "above";
            const replyingAgents = activeReplyProgress.map((progress) => progress.agent.display_name).join(", ");
            const activeReplyAgentIds = new Set(activeReplyProgress.map((progress) => progress.agent.id).filter(Boolean));
            const replySummaryClassName = [
              "thread-reply-summary",
              hasActiveReplyProgress ? "active-reply" : "",
              unreadReplyCount > 0 ? "unread-replies" : "",
            ].filter(Boolean).join(" ");
            const replyParticipants = (replySummary?.participants ?? [])
              .filter((participant) => !participant.sender_agent_id || !activeReplyAgentIds.has(participant.sender_agent_id))
              .slice(0, Math.max(0, 3 - activeReplyProgress.length));
            const messageAgent = isDm ? null : agentForMessageSender(message, agents);
            const deletedMessageAgent = isDm || messageAgent ? null : deletedAgentForMessageSender(message);
            const isSaved = savedMessageIds.has(message.id);
            const isCompact = isCompactFollowupMessage(message, rootMessages[index - 1]);
            const showDateDivider = index === 0 || !isSameCalendarDay(message.created_at, rootMessages[index - 1]?.created_at ?? "");
            const isLongChannelMessage = shouldCollapseChannelMessage(message.body);
            const isChannelMessageExpanded = expandedChannelMessageIds.has(message.id);
            const visibleMessageBody = isLongChannelMessage && !isChannelMessageExpanded
              ? channelMessagePreview(message.body)
              : message.body;
            if (message.sender_role === "system") {
              return (
                <Fragment key={message.id}>
                  {showDateDivider && (
                    <div className="message-date-divider" role="separator">
                      <span />
                      <time dateTime={message.created_at}>{formatDateDivider(message.created_at)}</time>
                      <span />
                    </div>
                  )}
                  <article className="system-message">
                    <div className="system-message-line">
                      <MessageMarkdown body={message.body} onLocalAgentLink={openLinkedAgentDetail} scrollKey={`message:${message.id}`} />
                      <time>{formatTime(message.created_at)}</time>
                    </div>
                  </article>
                </Fragment>
              );
            }
            return (
              <Fragment key={message.id}>
                {showDateDivider && (
                  <div className="message-date-divider" role="separator">
                    <span />
                    <time dateTime={message.created_at}>{formatDateDivider(message.created_at)}</time>
                    <span />
                  </div>
                )}
                <article
                  data-message-id={message.id}
                  className={`message-card ${isCompact ? "compact" : ""} ${message.id === activeRoot?.id ? "focused" : ""} ${isSaved ? "saved" : ""}`}
                  data-jump-focused={focusedMessageId === message.id ? "true" : "false"}
                  onClick={(event) => {
                    if (!isPrimaryUnmodifiedClick(event)) return;
                    if (isInteractiveMessageClick(event)) return;
                    if (hasSelectedText()) return;
                    if (shouldOpenThreadFromMessageClick()) setActiveThreadId(message.id);
                  }}
                  onContextMenu={(event) => {
                    if (shouldUseNativeMessageSelection()) return;
                    event.preventDefault();
                    event.stopPropagation();
                    setMessageMenu({ x: event.clientX, y: event.clientY, message });
                  }}
                >
                  {isCompact ? (
                    <time className="message-compact-time" dateTime={message.created_at}>
                      {formatClockTime(message.created_at)}
                    </time>
                  ) : messageAgent ? (
                    <button
                      type="button"
                      className="message-agent-avatar-trigger"
                      aria-label={`View @${messageAgent.handle} details`}
                      onClick={(event) => {
                        event.stopPropagation();
                        openAgentDetail(messageAgent);
                      }}
                    >
                      <AgentAvatarWithProfile agent={messageAgent} />
                    </button>
                  ) : deletedMessageAgent ? (
                    <AgentAvatar
                      agent={deletedMessageAgent}
                      size="md"
                      title={`@${deletedMessageAgent.handle} has been deleted`}
                    />
                  ) : message.sender_role === "owner" ? (
                    <AgentAvatar agent={ownerAsAvatarAgent(ownerProfile)} size="md" showStatus={false} />
                  ) : (
                    <div className="avatar">{message.sender_name.slice(0, 1)}</div>
                  )}
                  <div className="message-body">
                    {!isCompact && (
                      <div className="meta">
                        <strong>{message.sender_name}</strong>
                        <span>{message.sender_role}</span>
                        <time>{formatTime(message.created_at)}</time>
                        {wasEdited(message) && <span className="edited-indicator">edited</span>}
                        {linkedTask && (
                          <mark>
                            <CheckCircle2 size={14} /> #{linkedTask.number} · {linkedTask.status.replace("_", " ")}
                          </mark>
                        )}
                        <button
                          type="button"
                          className={`message-save-button mobile-message-save-tag ${isSaved ? "saved" : ""}`}
                          title={isSaved ? "Unsave message" : "Save message"}
                          aria-label={isSaved ? "Unsave message" : "Save message"}
                          aria-pressed={isSaved}
                          onPointerDown={(event) => event.stopPropagation()}
                          onClick={(event) => {
                            event.stopPropagation();
                            onToggleMessageSaved(message, !isSaved);
                          }}
                        >
                          <Bookmark size={14} />
                        </button>
                      </div>
                    )}
                    <div className="message-hover-actions" aria-label="Message actions">
                      <button
                        type="button"
                        data-tooltip={replyCount > 0 ? "View thread" : "Reply in thread"}
                        title={replyCount > 0 ? "View thread replies" : "Reply in thread"}
                        aria-label={replyCount > 0 ? "View thread replies" : "Reply in thread"}
                        onPointerDown={(event) => event.stopPropagation()}
                        onClick={(event) => {
                          event.stopPropagation();
                          if (!isPrimaryUnmodifiedClick(event)) return;
                          setActiveThreadId(message.id);
                        }}
                      >
                        <MessageSquare size={14} />
                      </button>
                      <button
                        type="button"
                        className={isSaved ? "saved" : ""}
                        data-tooltip={isSaved ? "Unsave" : "Save"}
                        title={isSaved ? "Unsave message" : "Save message"}
                        aria-label={isSaved ? "Unsave message" : "Save message"}
                        onPointerDown={(event) => event.stopPropagation()}
                        onClick={(event) => {
                          event.stopPropagation();
                          onToggleMessageSaved(message, !isSaved);
                        }}
                      >
                        <Bookmark size={14} />
                      </button>
                    </div>
                    {message.delivery_state !== "streaming" && (
                      <>
                        <div className={isLongChannelMessage && !isChannelMessageExpanded ? "message-long-preview collapsed" : "message-long-preview"}>
                          <MessageMarkdown body={visibleMessageBody} onLocalAgentLink={openLinkedAgentDetail} scrollKey={`message:${message.id}`} />
                        </div>
                        {isLongChannelMessage && (
                          <button
                            type="button"
                            className="message-expand-button"
                            aria-expanded={isChannelMessageExpanded}
                            onPointerDown={(event) => event.stopPropagation()}
                            onClick={(event) => {
                              event.stopPropagation();
                              toggleChannelMessageExpanded(message.id);
                            }}
                          >
                            {isChannelMessageExpanded ? "Show less" : "Show more"}
                          </button>
                        )}
                      </>
                    )}
                    <MessageAttachments attachments={message.attachments} showImageThumbnails={showImageThumbnails} />
                    <MessageArtifacts artifacts={message.artifacts} onOpenArtifact={openArtifact} />
                    {message.delivery_state === "sending" && (
                      <div className="message-stream-state sending">Sending...</div>
                    )}
                    {message.delivery_state === "error" && (
                      <div className="message-stream-state error">Response interrupted</div>
                    )}
                    {(hasActiveReplyProgress || (replyCount > 0 && replySummary)) && (
                      <button
                        type="button"
                        className={replySummaryClassName}
                        data-active-menu-placement={hasActiveReplyProgress ? activeReplyMenuPlacement : undefined}
                        title="View thread replies"
                        aria-label={hasActiveReplyProgress
                          ? `${activeReplyStatus}${replyingAgents ? `: ${replyingAgents}` : ""}. View thread`
                          : `View ${replyCount} ${replyCount === 1 ? "reply" : "replies"} in thread`}
                        onPointerEnter={(event) => {
                          if (hasActiveReplyProgress) updateActiveReplyMenuPlacement(message.id, event.currentTarget);
                        }}
                        onPointerDown={(event) => event.stopPropagation()}
                        onFocus={(event) => {
                          if (hasActiveReplyProgress) updateActiveReplyMenuPlacement(message.id, event.currentTarget);
                        }}
                        onClick={(event) => {
                          event.stopPropagation();
                          if (!isPrimaryUnmodifiedClick(event)) return;
                          setActiveThreadId(message.id);
                        }}
                      >
                        {(activeReplyProgress.length > 0 || replyParticipants.length > 0) && (
                          <div className="thread-reply-avatars">
                            {activeReplyProgress.slice(0, 3).map((progress) => (
                              <span key={`active:${progress.key}`}>
                                <AgentAvatar agent={progress.agent} size="sm" showStatus={false} />
                              </span>
                            ))}
                            {replyParticipants.map((participant) => (
                              <span key={`${participant.sender_role}:${participant.sender_agent_id ?? participant.sender_name}`}>
                                {renderReplyParticipantAvatar(participant)}
                              </span>
                            ))}
                          </div>
                        )}
                        {hasActiveReplyProgress && (
                          <span className="thread-reply-progress-dots" aria-hidden="true">...</span>
                        )}
                        {replyCount > 0 && (
                          <strong>{`${replyCount} ${replyCount === 1 ? "reply" : "replies"}`}</strong>
                        )}
                        {hasActiveReplyProgress ? (
                          <span className="thread-reply-summary-spacer" aria-hidden="true">
                            <span className="thread-reply-active-menu" aria-hidden="true">
                              {activeReplyProgress.map((progress) => {
                                const summary = replyProgressSummary(progress);
                                return (
                                  <span key={`menu:${progress.key}`} className="thread-reply-active-agent">
                                    <AgentAvatar agent={progress.agent} size="sm" showStatus={false} />
                                    <span className="thread-reply-active-agent-copy">
                                      <span className="thread-reply-active-agent-name">{progress.agent.display_name}</span>
                                      <span className="thread-reply-active-agent-status">
                                        <span>{summary.title}</span>
                                        {summary.detail && <em>{summary.detail}</em>}
                                      </span>
                                    </span>
                                  </span>
                                );
                              })}
                            </span>
                          </span>
                        ) : replySummary?.latest ? (
                          <span className="thread-reply-summary-action">
                            <time dateTime={replySummary.latest.created_at}>Last reply {formatTime(replySummary.latest.created_at)}</time>
                            <span className="thread-reply-summary-open">View thread</span>
                          </span>
                        ) : null}
                        <ChevronRight className="thread-reply-summary-icon" size={18} aria-hidden="true" />
                      </button>
                    )}
                  </div>
                </article>
              </Fragment>
            );
            })}
            <div ref={messageListBottomAnchorRef} className="message-list-bottom-anchor" aria-hidden="true" />
          </div>
          </div>
          {messageMenu && (
            <MessageActionMenu
              x={messageMenu.x}
              y={messageMenu.y}
              isSaved={savedMessageIds.has(messageMenu.message.id)}
              onCopyLink={() => copyMessageLink(messageMenu.message)}
              onCopyMarkdown={() => copyMessageMarkdown(messageMenu.message)}
              onToggleSaved={() => {
                onToggleMessageSaved(messageMenu.message, !savedMessageIds.has(messageMenu.message.id));
                setMessageMenu(null);
              }}
              onClose={() => setMessageMenu(null)}
            />
          )}
        </div>
      ) : (
        <div className="task-board">
          <section className="task-board-summary" aria-label="Task summary">
            <div>
              <strong>{visibleTasks.length}</strong>
              <span>Total</span>
            </div>
            <div>
              <strong>{activeTasks.length}</strong>
              <span>Active</span>
            </div>
            <div>
              <strong>{reviewTasks.length}</strong>
              <span>Review</span>
            </div>
            <div>
              <strong>{unassignedTasks.length}</strong>
              <span>Unassigned</span>
            </div>
          </section>
          {visibleTasks.length === 0 && (
            <div className="empty-state">
              <LayoutList size={34} />
              <h2>No tasks in this channel</h2>
              <p>Create tracked work from chat by sending a message in Task mode.</p>
            </div>
          )}
          {visibleTasks.length > 0 && (
            <div className="task-sections">
              {unassignedTasks.length > 0 && (
                <section className="task-queue-section unassigned" aria-label="Unassigned task queue">
                  <div className="task-queue-heading">
                    <div>
                      <span>Queue</span>
                      <strong>Unassigned</strong>
                    </div>
                    <mark>{unassignedTasks.length}</mark>
                  </div>
                  <div className="task-list">
                    {unassignedTasks.map((task) => renderTaskCard(task))}
                  </div>
                </section>
              )}
              {assignedTasks.length > 0 && (
                <section className="task-queue-section" aria-label="Assigned tasks">
                  <div className="task-queue-heading">
                    <div>
                      <span>Work</span>
                      <strong>Assigned</strong>
                    </div>
                    <mark>{assignedTasks.length}</mark>
                  </div>
                  <div className="task-list">
                    {assignedTasks.map((task) => renderTaskCard(task))}
                  </div>
                </section>
              )}
            </div>
          )}
        </div>
      )}

      {activeTab === "chat" && (
        <ConversationComposer
          channel={channel}
          isDm={isDm}
          dmAgent={dmAgent}
          mentionAgents={mentionAgents}
          channels={channels}
          draft={draft}
          draftAttachments={draftAttachments}
          setDraft={setDraft}
          addDraftAttachments={addDraftAttachments}
          removeDraftAttachment={removeDraftAttachment}
          sendRootMessage={sendRootMessage}
        />
      )}
    </section>
  );

  function renderTaskCard(task: Task) {
    const assignee = agents.find((agent) => agent.id === task.assignee_id) ?? null;
    return (
      <article className={`task-card ${task.assignee_id ? "" : "unassigned"}`} key={task.id}>
        <div className="task-card-main">
          <div className="task-card-head" onClick={() => openTask(task)}>
            <span>Task #{task.number}</span>
            <button type="button" className="task-open-thread" aria-label={`Open task #${task.number} thread`}>
              <MessageSquare size={14} />
            </button>
          </div>
          <input
            value={taskTitleDrafts[task.id] ?? task.title}
            onChange={(event) => setTaskTitleDraft(task, event.target.value)}
            onBlur={() => saveTaskTitle(task)}
            onKeyDown={(event) => {
              if (isImeComposing(event)) return;
              if (event.key === "Enter") saveTaskTitle(task);
            }}
          />
          <p>Updated {formatTime(task.updated_at)}</p>
        </div>
        <div className="task-controls">
          <TaskAssigneePicker
            agents={taskAssigneeOptions}
            assignee={assignee}
            disabled={task.status === "done"}
            done={task.status === "done"}
            onChange={(agentId) => claimTask(task, agentId)}
            taskNumber={task.number}
          />
          <div className="status-row" aria-label={`Task #${task.number} status`}>
            {TASK_STATUSES.map((status) => (
              <button
                type="button"
                key={status}
                className={task.status === status ? "active" : ""}
                data-state={status}
                onClick={() => updateTaskStatus(task, status)}
              >
                {taskStatusLabel(status)}
              </button>
            ))}
          </div>
        </div>
      </article>
    );
  }
}

type ConversationComposerProps = {
  channel: Channel | null;
  isDm: boolean;
  dmAgent: Agent | null;
  mentionAgents: Agent[];
  channels: Channel[];
  draft: string;
  draftAttachments: DraftAttachment[];
  setDraft: (value: string) => void;
  addDraftAttachments: (files: FileList | File[]) => void;
  removeDraftAttachment: (id: string) => void;
  sendRootMessage: (asTask?: boolean, bodyOverride?: string, attachmentsOverride?: DraftAttachment[]) => void;
};

function hasDraggedFiles(event: DragEvent<HTMLElement>) {
  return Array.from(event.dataTransfer.types).includes("Files");
}

function useBufferedComposerText(draft: string, resetKey: string | null | undefined, setDraft: (value: string) => void) {
  const [text, setText] = useState(draft);
  const textRef = useRef(draft);
  const committedRef = useRef(draft);
  const setDraftRef = useRef(setDraft);

  useEffect(() => {
    setDraftRef.current = setDraft;
  }, [setDraft]);

  useEffect(() => {
    textRef.current = draft;
    committedRef.current = draft;
    setText(draft);
  }, [draft, resetKey]);

  useEffect(() => {
    return () => {
      if (textRef.current === committedRef.current) return;
      committedRef.current = textRef.current;
      setDraftRef.current(textRef.current);
    };
  }, [resetKey]);

  function updateText(value: string) {
    textRef.current = value;
    setText(value);
  }

  function commitText(value = textRef.current) {
    if (value === committedRef.current) return;
    committedRef.current = value;
    setDraftRef.current(value);
  }

  function markCommitted(value: string) {
    textRef.current = value;
    committedRef.current = value;
    setText(value);
  }

  return { text, updateText, commitText, markCommitted };
}

function ConversationComposer({
  channel,
  isDm,
  dmAgent,
  mentionAgents,
  channels,
  draft,
  draftAttachments,
  setDraft,
  addDraftAttachments,
  removeDraftAttachment,
  sendRootMessage,
}: ConversationComposerProps) {
  const [sendAsTask, setSendAsTask] = useState(false);
  const [isComposerDragOver, setIsComposerDragOver] = useState(false);
  const composerDragDepthRef = useRef(0);
  const taskToggleHandledAtRef = useRef(0);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const shouldUseShortPlaceholder = useMobileViewport();
  const { text, updateText, commitText, markCommitted } = useBufferedComposerText(draft, channel?.id, setDraft);
  const {
    mentionState,
    mentionIndex,
    mentionCandidates,
    refreshMentionState,
    chooseMention,
    handleMentionKeyDown,
    closeMentionPicker,
    focusComposer,
  } = useMentionPicker({ agents: mentionAgents, channels, value: text, setValue: updateText, textareaRef });
  useAutoGrowTextarea(textareaRef, text);

  useEffect(() => {
    if (isDm) setSendAsTask(false);
  }, [isDm]);

  useEffect(() => {
    composerDragDepthRef.current = 0;
    setIsComposerDragOver(false);
    closeMentionPicker();
  }, [channel?.id]);

  // Mobile WebViews dismiss the soft keyboard when a tap blurs the focused
  // textarea, and the first tap is consumed by the dismissal.
  function preserveComposerFocus(event: ReactMouseEvent<HTMLElement>) {
    if (textareaRef.current && document.activeElement === textareaRef.current) {
      event.preventDefault();
    }
  }

  function handleTaskToggleMouseDown(event: ReactMouseEvent<HTMLElement>) {
    if (Date.now() - taskToggleHandledAtRef.current < 600) return;
    preserveComposerFocus(event);
    if (!channel) return;
    taskToggleHandledAtRef.current = Date.now();
    setSendAsTask((current) => !current);
  }

  function handleTaskTogglePointerDown(event: ReactPointerEvent<HTMLButtonElement>) {
    if (event.pointerType === "mouse") return;
    if (!channel) return;
    event.preventDefault();
    event.stopPropagation();
    taskToggleHandledAtRef.current = Date.now();
    setSendAsTask((current) => !current);
  }

  function handleTaskToggleClick() {
    if (!channel) return;
    if (Date.now() - taskToggleHandledAtRef.current < 600) return;
    setSendAsTask((current) => !current);
  }

  function submitComposer() {
    if (!channel || (!text.trim() && draftAttachments.length === 0)) return;
    const body = text;
    markCommitted("");
    sendRootMessage(isDm ? false : sendAsTask, body, draftAttachments);
    closeMentionPicker();
    focusComposer();
  }

  function handleComposerKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (isImeComposing(event)) return;
    if (handleMentionKeyDown(event)) return;
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      submitComposer();
    }
  }

  function handleComposerDragEnter(event: DragEvent<HTMLElement>) {
    if (!hasDraggedFiles(event)) return;
    event.preventDefault();
    event.stopPropagation();
    composerDragDepthRef.current += 1;
    event.dataTransfer.dropEffect = channel ? "copy" : "none";
    if (channel) setIsComposerDragOver(true);
  }

  function handleComposerDragOver(event: DragEvent<HTMLElement>) {
    if (!hasDraggedFiles(event)) return;
    event.preventDefault();
    event.stopPropagation();
    event.dataTransfer.dropEffect = channel ? "copy" : "none";
    if (channel) setIsComposerDragOver(true);
  }

  function handleComposerDragLeave(event: DragEvent<HTMLElement>) {
    if (!hasDraggedFiles(event)) return;
    event.preventDefault();
    event.stopPropagation();
    composerDragDepthRef.current = Math.max(0, composerDragDepthRef.current - 1);
    if (composerDragDepthRef.current === 0) setIsComposerDragOver(false);
  }

  function handleComposerDrop(event: DragEvent<HTMLElement>) {
    if (!hasDraggedFiles(event)) return;
    event.preventDefault();
    event.stopPropagation();
    composerDragDepthRef.current = 0;
    setIsComposerDragOver(false);
    if (!channel || event.dataTransfer.files.length === 0) return;
    addDraftAttachments(event.dataTransfer.files);
    focusComposer();
  }

  function handleComposerPaste(event: ClipboardEvent<HTMLTextAreaElement>) {
    const imageFiles = Array.from(event.clipboardData.files).filter((file) => file.type.startsWith("image/"));
    if (imageFiles.length === 0) return;
    event.preventDefault();
    if (!channel) return;
    addDraftAttachments(imageFiles);
    focusComposer();
  }

  const fullPlaceholder = channel
    ? isDm
      ? `Message @${dmAgent?.handle || "agent"}`
      : `Message #${channel.name}`
    : "Create a channel before messaging";
  const placeholder = shouldUseShortPlaceholder
    ? channel ? "Message" : "No channel"
    : fullPlaceholder;

  return (
    <footer
      className={`composer ${isComposerDragOver ? "drag-over" : ""}`}
      onDragEnter={handleComposerDragEnter}
      onDragOver={handleComposerDragOver}
      onDragLeave={handleComposerDragLeave}
      onDrop={handleComposerDrop}
    >
      {mentionState && mentionCandidates.length > 0 && (
        <div className="mention-picker">
          {mentionCandidates.map((candidate, index) => (
            <button
              key={`${candidate.kind}:${candidate.id}`}
              className={index === mentionIndex ? "active" : ""}
              onMouseDown={(event) => {
                event.preventDefault();
                chooseMention(candidate);
              }}
            >
              {candidate.kind === "agent" ? (
                <>
                  <AgentAvatar agent={candidate.agent} size="sm" title={`@${candidate.agent.handle}`} />
                  <span className="mention-picker-copy">
                    <strong>{candidate.agent.display_name}</strong>
                    <small>@{candidate.agent.handle}</small>
                    {visibleAgentDescription(candidate.agent.description) && <em>{visibleAgentDescription(candidate.agent.description)}</em>}
                  </span>
                </>
              ) : (
                <>
                  <span className="mention-picker-channel-icon" aria-hidden="true">
                    <Hash size={16} />
                  </span>
                  <span className="mention-picker-copy">
                    <strong>#{candidate.channel.name}</strong>
                    <small>Channel</small>
                    {visibleChannelDescription(candidate.channel.description) && <em>{visibleChannelDescription(candidate.channel.description)}</em>}
                  </span>
                </>
              )}
            </button>
          ))}
        </div>
      )}
      <input
        ref={fileInputRef}
        type="file"
        multiple
        className="file-input-hidden"
        onChange={(event) => {
          if (event.target.files) addDraftAttachments(event.target.files);
          event.target.value = "";
        }}
      />
      <DraftAttachmentsPreview attachments={draftAttachments} onRemove={removeDraftAttachment} />
      <textarea
        ref={textareaRef}
        rows={1}
        value={text}
        autoCapitalize="none"
        autoComplete="off"
        autoCorrect="off"
        spellCheck={false}
        onChange={(event) => {
          updateText(event.target.value);
          refreshMentionState(event.target.value, event.target.selectionStart);
        }}
        onBlur={() => commitText()}
        onSelect={(event) => refreshMentionState(text, event.currentTarget.selectionStart)}
        onKeyDown={handleComposerKeyDown}
        onPaste={handleComposerPaste}
        disabled={!channel}
        placeholder={placeholder}
        aria-label={fullPlaceholder}
      />
      <div className="composer-actions">
        <button
          type="button"
          className="attach-button"
          disabled={!channel}
          onMouseDown={preserveComposerFocus}
          onClick={() => fileInputRef.current?.click()}
        >
          <Paperclip size={16} />
        </button>
        {!isDm && (
          <button
            type="button"
            className={`task-toggle ${sendAsTask ? "active" : ""}`}
            title={sendAsTask ? "Send next message as a normal message" : "Send next message as a task"}
            aria-label={sendAsTask ? "Send next message as a normal message" : "Send next message as a task"}
            aria-pressed={sendAsTask}
            disabled={!channel}
            onPointerDown={handleTaskTogglePointerDown}
            onMouseDown={handleTaskToggleMouseDown}
            onClick={handleTaskToggleClick}
          >
            <Flag size={15} />
            <span>Task</span>
          </button>
        )}
        <button
          className="send"
          title={sendAsTask && !isDm ? "Create task" : "Send message"}
          aria-label={sendAsTask && !isDm ? "Create task" : "Send message"}
          disabled={!channel || (!text.trim() && draftAttachments.length === 0)}
          onMouseDown={preserveComposerFocus}
          onClick={submitComposer}
        >
          <Send size={17} />
        </button>
      </div>
    </footer>
  );
}
