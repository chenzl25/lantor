import { ArrowDown, ArrowLeft, CheckCircle2, Crosshair, FileImage, Hash, Maximize2, MessageSquare, Minimize2, MoreHorizontal, Paperclip, Quote, RotateCcw, Send, X } from "lucide-react";
import { useEffect, useLayoutEffect, useMemo, useRef, useState, type ClipboardEvent, type DragEvent, type KeyboardEvent, type PointerEvent as ReactPointerEvent, type TextareaHTMLAttributes, type WheelEvent as ReactWheelEvent } from "react";
import { useEventCallback } from "../hooks/useEventCallback";
import { useMessageRows } from "../hooks/useMessageRows";
import { useRetainedValue } from "../hooks/useRetainedValue";
import { useAutoGrowTextarea } from "../hooks/useAutoGrowTextarea";
import { useCoarsePointer } from "../hooks/useCoarsePointer";
import { useMentionPicker } from "../hooks/useMentionPicker";
import { useMobileViewport } from "../hooks/useMobileViewport";
import { APP_DISPLAY_NAME } from "../branding";
import { isImeComposing, isInputComposing } from "../input-utils";
import { mentionableAgentsForChannel } from "../mentions";
import { copyText } from "../clipboard";
import { observeScrollGeometry } from "../scroll-geometry";
import { isCompactFollowupMessage } from "../message-grouping";
import { shouldCollapseMessage as shouldCollapseThreadMessage } from "../message-preview";
import { messageShareLink, messageToMarkdown } from "../message-share";
import { appendMessageReferenceToken, messageReferenceToken, parseMessageReferences, removeMessageReferenceToken, withoutMessageReferenceTokens, type MessageReferenceKind, type ResolvedMessageReference } from "../message-references";
import { downloadThreadPanelSvg } from "../thread-svg-export";
import { Agent, AgentActivity, AgentRun, AgentWorkItem, Artifact, Channel, DraftAttachment, Message, OwnerProfile, TASK_STATUSES, Task } from "../types";
import { formatClockTime, formatTime, isSameCalendarDay, visibleAgentDescription, visibleChannelDescription } from "../ui-utils";
import { ActivityProgressDock, activeProgressByAgent, indexProgress } from "./ActivityProgressDock";
import { AgentAvatar } from "./AgentAvatar";
import { ComposerReferenceTextarea } from "./ComposerReferenceTextarea";
import { DraftAttachmentsPreview } from "./DraftAttachmentsPreview";
import { MessageActionMenu } from "./MessageActionMenu";
import { MessageRow, type MessageRowActions, type MessageRowAction } from "./MessageRow";
import { MessageReferencePreview, type MessageReferencePreviewItem } from "./MessageReferencePreview";
import { TaskAssigneePicker } from "./TaskAssigneePicker";

type WritingSuggestionsTextareaAttrs = TextareaHTMLAttributes<HTMLTextAreaElement> & { "writingsuggestions": "false" };

const disableWritingSuggestionsAttrs: WritingSuggestionsTextareaAttrs = { writingsuggestions: "false" };

function taskStatusLabel(status: string) {
  return status.replace("_", " ");
}

function metadataString(metadata: Record<string, unknown>, key: string) {
  const value = metadata[key];
  return typeof value === "string" ? value : typeof value === "number" ? String(value) : "";
}

function activityDetailText(activity: AgentActivity) {
  const title = metadataString(activity.metadata, "title");
  if (title) return title;
  const detail = activity.detail.trim();
  if (!detail || detail.startsWith("{")) return "";
  return detail;
}

function taskActivityLabel(activity: AgentActivity) {
  return activity.title || activity.summary || activity.kind.replace("_", " ");
}

function isNoisyTaskActivity(activity: AgentActivity) {
  const title = taskActivityLabel(activity).toLowerCase();
  if (activity.kind === "task" && title.startsWith("task claim opportunity")) return true;
  if (activity.kind === "dispatch" && (title === "request started" || title === "request queued")) return true;
  if (activity.kind === "run" && (title === "started working" || title === "run started" || title === "run created")) return true;
  return false;
}

function compactReferencePreview(body: string) {
  const text = withoutMessageReferenceTokens(body).replace(/\s+/g, " ").trim();
  if (!text) return "No text preview";
  return text.length > 140 ? `${text.slice(0, 139).trimEnd()}...` : text;
}

function waitForNextFrame() {
  return new Promise<void>((resolve) => {
    window.requestAnimationFrame(() => resolve());
  });
}

type ThreadPanelProps = {
  channel: Channel | null;
  channels: Channel[];
  agents: Agent[];
  channelAgents: Agent[];
  ownerProfile: OwnerProfile;
  agentActivities: AgentActivity[];
  agentRuns: AgentRun[];
  agentWorkItems: AgentWorkItem[];
  activeRoot: Message | null;
  activeTask: Task | null;
  replies: Message[];
  unreadCount: number;
  taskTitleDrafts: Record<string, string>;
  replyDraft: string;
  replyAttachments: DraftAttachment[];
  onClose: () => void;
  setTaskTitleDraft: (task: Task, title: string) => void;
  saveTaskTitle: (task: Task) => void;
  claimTask: (task: Task, agentId: string) => void;
  updateTaskStatus: (task: Task, status: string) => void;
  setReplyDraft: (value: string) => void;
  addReplyAttachments: (files: FileList | File[]) => void;
  removeReplyAttachment: (id: string) => void;
  sendReply: (bodyOverride?: string, attachmentsOverride?: DraftAttachment[]) => void;
  openAgentDetail: (agent: Agent) => void;
  openArtifact: (artifact: Artifact) => void;
  openWorkItem?: (item: AgentWorkItem, focusedMessageIdOverride?: string | null) => void;
  onReferenceMessageJump: (originMessageId: string, targetMessageId: string) => void;
  onReferenceThreadJump: (originMessageId: string, threadId: string) => void;
  messages: Message[];
  onLocateRoot: (message: Message) => void;
  shareBaseUrl: string | null;
  savedMessageIds: Set<string>;
  focusedMessageId: string | null;
  showImageThumbnails: boolean;
  onToggleMessageSaved: (message: Message, saved: boolean) => void;
  onResizeStart: (event: ReactPointerEvent<HTMLButtonElement>) => void;
};

type MessageMenuState = {
  x: number;
  y: number;
  message: Message;
} | null;

type ThreadMessageExpansionState = {
  messageIds: Set<string>;
  lastTouchedAt: number;
};

type ThreadScrollState = {
  scrollHeight: number;
  scrollTop: number;
  clientHeight: number;
  shouldFollow: boolean;
  anchor: ThreadScrollAnchor | null;
  lastTouchedAt: number;
};

type ThreadScrollAnchor = {
  messageId: string;
  topOffset: number;
};

const THREAD_MESSAGE_EXPANSION_TTL_MS = 24 * 60 * 60 * 1000;
const THREAD_MESSAGE_EXPANSION_MAX_ENTRIES = 50;
const THREAD_SCROLL_STATE_TTL_MS = 24 * 60 * 60 * 1000;
const THREAD_SCROLL_STATE_MAX_ENTRIES = 100;

function pruneThreadMessageExpansionState(
  entries: Map<string, ThreadMessageExpansionState>,
  now: number,
) {
  const freshEntries = Array.from(entries.entries())
    .filter(([, entry]) => now - entry.lastTouchedAt <= THREAD_MESSAGE_EXPANSION_TTL_MS)
    .sort((left, right) => right[1].lastTouchedAt - left[1].lastTouchedAt)
    .slice(0, THREAD_MESSAGE_EXPANSION_MAX_ENTRIES);
  return new Map(freshEntries);
}

function pruneThreadScrollState(entries: Map<string, ThreadScrollState>, now: number) {
  const freshEntries = Array.from(entries.entries())
    .filter(([, entry]) => now - entry.lastTouchedAt <= THREAD_SCROLL_STATE_TTL_MS)
    .sort((left, right) => right[1].lastTouchedAt - left[1].lastTouchedAt)
    .slice(0, THREAD_SCROLL_STATE_MAX_ENTRIES);
  return new Map(freshEntries);
}

export function ThreadPanel({
  channel,
  channels,
  agents,
  channelAgents,
  ownerProfile,
  agentActivities,
  agentRuns,
  agentWorkItems,
  activeRoot,
  activeTask,
  replies,
  taskTitleDrafts,
  replyDraft,
  replyAttachments,
  onClose,
  setTaskTitleDraft,
  saveTaskTitle,
  claimTask,
  updateTaskStatus,
  setReplyDraft,
  addReplyAttachments,
  removeReplyAttachment,
  sendReply,
  openAgentDetail,
  openArtifact,
  openWorkItem,
  onReferenceMessageJump,
  onReferenceThreadJump,
  messages,
  onLocateRoot,
  shareBaseUrl,
  savedMessageIds,
  focusedMessageId,
  showImageThumbnails,
  onToggleMessageSaved,
  onResizeStart,
}: ThreadPanelProps) {
  const [showBackToBottom, setShowBackToBottom] = useState(false);
  const [showMobileThreadActions, setShowMobileThreadActions] = useState(false);
  const [messageMenu, setMessageMenu] = useState<MessageMenuState>(null);
  const [tapFocusedMessageId, setTapFocusedMessageId] = useState<string | null>(null);
  const [expandedThreadMessageStateByThread, setExpandedThreadMessageStateByThread] = useState<Map<string, ThreadMessageExpansionState>>(() => new Map());
  const [pendingCollapsedThreadMessageId, setPendingCollapsedThreadMessageId] = useState<string | null>(null);
  const threadPanelRef = useRef<HTMLElement | null>(null);
  const threadScrollRef = useRef<HTMLDivElement | null>(null);
  const threadScrollContentRef = useRef<HTMLDivElement | null>(null);
  const threadMessageRefs = useRef(new Map<string, HTMLElement>());
  const threadScrollFrameRef = useRef<number | null>(null);
  const threadGeometryRef = useRef({ scrollHeight: 0, clientHeight: 0 });
  const shouldFollowThreadRef = useRef(true);
  const userThreadScrollUntilRef = useRef(0);
  const threadScrollMetricsRef = useRef({ scrollHeight: 0, scrollTop: 0, clientHeight: 0 });
  const threadScrollStateByThreadRef = useRef<Map<string, ThreadScrollState>>(new Map());
  const threadScrollAnchorRef = useRef<ThreadScrollAnchor | null>(null);
  const focusedThreadMessageScrollKeyRef = useRef<string | null>(null);
  const openLinkedAgentDetail = useEventCallback((handle: string) => {
    const agent = agents.find((candidate) => candidate.handle.toLowerCase() === handle.toLowerCase());
    if (agent) openAgentDetail(agent);
  });
  const isDm = channel?.kind === "dm";
  const agentsById = useMemo(() => new Map(agents.map((agent) => [agent.id, agent])), [agents]);
  const dmAgent = isDm ? agentsById.get(channel?.dm_agent_id ?? "") ?? null : null;
  const activeThreadExpansionKey = activeRoot?.id ?? null;
  const expandedThreadMessageIds = useMemo(() => {
    if (!activeThreadExpansionKey) return new Set<string>();
    const entry = expandedThreadMessageStateByThread.get(activeThreadExpansionKey);
    if (!entry || Date.now() - entry.lastTouchedAt > THREAD_MESSAGE_EXPANSION_TTL_MS) return new Set<string>();
    return entry.messageIds;
  }, [activeThreadExpansionKey, expandedThreadMessageStateByThread]);

  useEffect(() => {
    setShowMobileThreadActions(false);
  }, [activeThreadExpansionKey]);
  const surfaceLabel = channel
    ? isDm
      ? `Thread in DM with @${dmAgent?.handle || "agent"}`
      : `Thread in #${channel.name}`
    : `${APP_DISPLAY_NAME} thread`;
  const threadMessages = useMemo(() => activeRoot ? [activeRoot, ...replies] : replies, [activeRoot, replies]);
  const { rows, referenceStore } = useMessageRows(threadMessages, messages, channels, agents, ownerProfile);
  const progressIndex = useMemo(() => indexProgress(agentActivities, agentRuns, agentWorkItems, agents),
    [agentActivities, agentRuns, agentWorkItems, agents]);
  const progressChannelId = activeRoot ? channel?.id ?? null : null;
  const progressThreadId = activeRoot?.id ?? null;
  const progress = useRetainedValue(useMemo(() => activeProgressByAgent(replies, progressIndex, progressChannelId, progressThreadId),
    [replies, progressIndex, progressChannelId, progressThreadId]));
  const threadMessageById = useMemo(() => new Map(threadMessages.map((message) => [message.id, message])), [threadMessages]);
  const channelNameById = useMemo(() => new Map(channels.map((value) => [value.id, value.name])), [channels]);

  function messageReferencePreviewItem(kind: MessageReferenceKind, id: string, token?: string): MessageReferencePreviewItem {
    const message = threadMessageById.get(id);
    if (!message) {
      return {
        key: `${kind}:${id}:${token ?? ""}`,
        kind,
        id,
        token,
        channelName: channel?.name ?? "unknown",
        senderName: "Missing reference",
        preview: id,
        meta: "not loaded",
        missing: true,
      };
    }
    const replyCount = kind === "thread" ? replies.filter((reply) => reply.thread_root_id === id).length : 0;
    return {
      key: `${kind}:${id}:${token ?? ""}`,
      kind,
      id,
      token,
      channelName: channelNameById.get(message.channel_id) ?? channel?.name ?? "unknown",
      senderName: message.sender_name,
      preview: compactReferencePreview(message.body),
      meta: kind === "thread"
        ? `${replyCount} ${replyCount === 1 ? "reply" : "replies"} · ${formatTime(message.created_at)}`
        : formatTime(message.created_at),
    };
  }

  function referencePreviewItemsForText(text: string) {
    if (!text.includes("[[")) return [];
    return parseMessageReferences(text).map((reference) => (
      messageReferencePreviewItem(reference.kind, reference.id, reference.token)
    ));
  }

  const handleReferenceOpen = useEventCallback((sourceMessageId: string, reference: ResolvedMessageReference) => {
    if (reference.kind === "thread") {
      onReferenceThreadJump(sourceMessageId, reference.id);
      return;
    }
    onReferenceMessageJump(sourceMessageId, reference.id);
    targetMessageIntoView(reference.id);
  });

  const onRowAction = useEventCallback((message: Message, action: MessageRowAction) => {
    switch (action) {
      case "reference": insertMessageReference(message, "message"); break;
      case "save": onToggleMessageSaved(message, !savedMessageIds.has(message.id)); break;
      case "focus": setTapFocusedMessageId(message.id); break;
      case "expand": toggleThreadMessageExpanded(message.id); break;
    }
  });
  const onRowMenu = useEventCallback((message: Message, x: number, y: number) => setMessageMenu({ message, x, y }));
  const onRowArtifact = useEventCallback(openArtifact);
  const onRowMount = useEventCallback((messageId: string, node: HTMLElement | null) => {
    if (node) threadMessageRefs.current.set(messageId, node);
    else threadMessageRefs.current.delete(messageId);
  });
  const rowActions = useMemo<MessageRowActions>(() => ({
    referenceStore,
    onAction: onRowAction, onMenu: onRowMenu, onArtifact: onRowArtifact, onMount: onRowMount,
    onAgent: openLinkedAgentDetail, onReference: handleReferenceOpen,
  }), [onRowAction, onRowMenu, onRowArtifact, onRowMount, openLinkedAgentDetail, handleReferenceOpen, referenceStore]);

  function insertMessageReference(message: Message, kind: MessageReferenceKind) {
    const referenceId = kind === "thread" ? (message.thread_root_id ?? message.id) : message.id;
    setReplyDraft(appendMessageReferenceToken(replyDraft, kind, referenceId));
    setMessageMenu(null);
  }

  async function copyMessageReference(message: Message, kind: MessageReferenceKind) {
    const referenceId = kind === "thread" ? (message.thread_root_id ?? message.id) : message.id;
    await copyText(messageReferenceToken(kind, referenceId));
    setMessageMenu(null);
  }

  function removeDraftReference(token: string) {
    setReplyDraft(removeMessageReferenceToken(replyDraft, token));
  }

  function targetMessageIntoView(messageId: string) {
    const element = threadMessageRefs.current.get(messageId);
    if (!element) return;
    stopFollowingThread();
    element.scrollIntoView({ block: "center" });
    setTapFocusedMessageId(messageId);
    window.requestAnimationFrame(() => {
      const scrollRoot = threadScrollRef.current;
      if (scrollRoot) rememberThreadScrollMetrics(scrollRoot);
    });
  }
  const lastReply = replies[replies.length - 1] ?? null;
  const collapsibleThreadMessageIds = useMemo(() => {
    const messages = activeRoot ? [activeRoot, ...replies] : replies;
    return messages
      .filter((message) => message.delivery_state !== "streaming" && shouldCollapseThreadMessage(message.body))
      .map((message) => message.id);
  }, [activeRoot, replies]);
  const hasCollapsibleThreadMessages = collapsibleThreadMessageIds.length > 0;
  const areAllThreadMessagesExpanded = hasCollapsibleThreadMessages
    && collapsibleThreadMessageIds.every((messageId) => expandedThreadMessageIds.has(messageId));
  const areAllThreadMessagesFolded = hasCollapsibleThreadMessages
    && collapsibleThreadMessageIds.every((messageId) => !expandedThreadMessageIds.has(messageId));

  function isThreadScrollAtBottom(element: HTMLDivElement) {
    return threadScrollDistanceFromBottom(element) < 32;
  }

  function threadScrollDistanceFromBottom(element: HTMLDivElement) {
    return element.scrollHeight - element.scrollTop - element.clientHeight;
  }

  function rememberThreadScrollMetrics(element: HTMLDivElement) {
    const anchor = shouldFollowThreadRef.current ? null : captureThreadScrollAnchor(element);
    threadScrollMetricsRef.current = {
      scrollHeight: element.scrollHeight,
      scrollTop: element.scrollTop,
      clientHeight: element.clientHeight,
    };
    threadScrollAnchorRef.current = anchor;
    rememberThreadScrollState(activeRoot?.id ?? null, element, anchor);
  }

  function captureThreadScrollAnchor(element: HTMLDivElement): ThreadScrollAnchor | null {
    const containerTop = element.getBoundingClientRect().top;
    const candidates = Array.from(element.querySelectorAll<HTMLElement>("[data-message-id]"));
    let closest: ThreadScrollAnchor | null = null;
    let closestDistance = Number.POSITIVE_INFINITY;
    for (const candidate of candidates) {
      const rect = candidate.getBoundingClientRect();
      if (rect.bottom < containerTop) continue;
      const messageId = candidate.dataset.messageId;
      if (!messageId) continue;
      const topOffset = rect.top - containerTop;
      const distance = Math.abs(topOffset);
      if (distance < closestDistance) {
        closestDistance = distance;
        closest = { messageId, topOffset };
      }
      if (topOffset >= 0) break;
    }
    return closest;
  }

  function restoreThreadScrollAnchor(remember = true) {
    const element = threadScrollRef.current;
    const anchor = threadScrollAnchorRef.current;
    if (!element || !anchor) return false;
    const anchorElement = threadMessageRefs.current.get(anchor.messageId);
    if (!anchorElement) return false;
    const nextTopOffset = anchorElement.getBoundingClientRect().top - element.getBoundingClientRect().top;
    const delta = nextTopOffset - anchor.topOffset;
    if (Math.abs(delta) > 0.5) {
      element.scrollTop += delta;
    }
    if (remember) rememberThreadScrollMetrics(element);
    return true;
  }

  function rememberThreadScrollState(threadId: string | null, element: HTMLDivElement | null, anchor: ThreadScrollAnchor | null) {
    if (!threadId || !element) return;
    const now = Date.now();
    const next = pruneThreadScrollState(threadScrollStateByThreadRef.current, now);
    next.set(threadId, {
      scrollHeight: element.scrollHeight,
      scrollTop: element.scrollTop,
      clientHeight: element.clientHeight,
      shouldFollow: shouldFollowThreadRef.current,
      anchor,
      lastTouchedAt: now,
    });
    threadScrollStateByThreadRef.current = next;
  }

  function restoreThreadScrollState(threadId: string) {
    const element = threadScrollRef.current;
    if (!element) return false;
    const state = threadScrollStateByThreadRef.current.get(threadId);
    if (!state) return false;
    shouldFollowThreadRef.current = state.shouldFollow;
    if (state.shouldFollow) {
      scrollThreadToBottom();
      setShowBackToBottom(false);
      return true;
    }
    cancelPendingThreadBottomScroll();
    userThreadScrollUntilRef.current = 0;
    const maxScrollTop = Math.max(0, element.scrollHeight - element.clientHeight);
    element.scrollTop = Math.min(state.scrollTop, maxScrollTop);
    threadScrollAnchorRef.current = state.anchor;
    if (!restoreThreadScrollAnchor()) rememberThreadScrollMetrics(element);
    setShowBackToBottom(Boolean(activeRoot) && !isThreadScrollAtBottom(element));
    return true;
  }

  function preserveThreadViewport() {
    if (shouldFollowThreadRef.current) {
      scrollThreadToBottom();
      return;
    }
    const element = threadScrollRef.current;
    if (!element) return;
    if (!restoreThreadScrollAnchor()) rememberThreadScrollMetrics(element);
  }

  function cancelPendingThreadBottomScroll() {
    if (threadScrollFrameRef.current !== null) {
      window.cancelAnimationFrame(threadScrollFrameRef.current);
      threadScrollFrameRef.current = null;
    }
  }

  function isUserScrollingThread() {
    return Date.now() < userThreadScrollUntilRef.current;
  }

  function stopFollowingThread(element = threadScrollRef.current) {
    userThreadScrollUntilRef.current = Date.now() + 650;
    shouldFollowThreadRef.current = false;
    cancelPendingThreadBottomScroll();
    if (element) rememberThreadScrollMetrics(element);
  }

  function isPointerOnThreadScrollbar(event: ReactPointerEvent<HTMLDivElement>) {
    const element = event.currentTarget;
    const scrollbarWidth = element.offsetWidth - element.clientWidth;
    if (scrollbarWidth <= 0) return false;
    return event.clientX >= element.getBoundingClientRect().right - scrollbarWidth - 2;
  }

  function scrollThreadToBottom() {
    if (threadScrollFrameRef.current !== null) return;
    threadScrollFrameRef.current = window.requestAnimationFrame(() => {
      threadScrollFrameRef.current = null;
      const element = threadScrollRef.current;
      if (!element || !shouldFollowThreadRef.current) return;
      element.scrollTop = threadGeometryRef.current.scrollHeight || element.scrollHeight;
    });
  }

  function handleThreadScroll() {
    const element = threadScrollRef.current;
    if (!element) return;
    const atBottom = isThreadScrollAtBottom(element);
    const layoutChanged =
      threadScrollMetricsRef.current.scrollHeight !== element.scrollHeight
      || threadScrollMetricsRef.current.clientHeight !== element.clientHeight;
    const userScrolling = isUserScrollingThread();
    const reachedScrollEnd = Math.abs(threadScrollDistanceFromBottom(element)) <= 1;
    let shouldShowBackToBottom = Boolean(activeRoot) && !atBottom && !shouldFollowThreadRef.current;
    if (atBottom && (!userScrolling || reachedScrollEnd)) {
      userThreadScrollUntilRef.current = 0;
      shouldFollowThreadRef.current = true;
      shouldShowBackToBottom = false;
    } else if (!atBottom && shouldFollowThreadRef.current && !layoutChanged) {
      shouldFollowThreadRef.current = false;
      cancelPendingThreadBottomScroll();
      shouldShowBackToBottom = Boolean(activeRoot);
    } else if (!atBottom && layoutChanged) {
      shouldShowBackToBottom = Boolean(activeRoot);
    }
    setShowBackToBottom((current) => current === shouldShowBackToBottom ? current : shouldShowBackToBottom);
    rememberThreadScrollMetrics(element);
  }

  function handleThreadWheel(event: ReactWheelEvent<HTMLDivElement>) {
    if (event.deltaY >= 0) return;
    stopFollowingThread();
  }

  function handleThreadPointerDown(event: ReactPointerEvent<HTMLDivElement>) {
    if (!isPointerOnThreadScrollbar(event)) return;
    stopFollowingThread(event.currentTarget);
  }

  function handleThreadTouchMove() {
    stopFollowingThread();
  }

  function handleThreadContentLoad() {
    preserveThreadViewport();
  }

  function returnThreadToBottom() {
    shouldFollowThreadRef.current = true;
    setShowBackToBottom(false);
    scrollThreadToBottom();
  }

  useEffect(() => {
    setMessageMenu(null);
    setTapFocusedMessageId(null);
    setPendingCollapsedThreadMessageId(null);
  }, [activeRoot?.id]);

  useEffect(() => {
    const now = Date.now();
    setExpandedThreadMessageStateByThread((current) => {
      const touched = new Map(current);
      if (activeThreadExpansionKey) {
        const entry = touched.get(activeThreadExpansionKey);
        if (entry && now - entry.lastTouchedAt <= THREAD_MESSAGE_EXPANSION_TTL_MS) {
          touched.set(activeThreadExpansionKey, { messageIds: entry.messageIds, lastTouchedAt: now });
        }
      }
      return pruneThreadMessageExpansionState(touched, now);
    });
  }, [activeThreadExpansionKey]);

  useEffect(() => {
    if (!pendingCollapsedThreadMessageId) return;
    const frameId = window.requestAnimationFrame(() => {
      threadMessageRefs.current.get(pendingCollapsedThreadMessageId)?.scrollIntoView({
        block: "start",
        behavior: "smooth",
      });
      const element = threadScrollRef.current;
      if (element) rememberThreadScrollMetrics(element);
      setPendingCollapsedThreadMessageId(null);
    });
    return () => window.cancelAnimationFrame(frameId);
  }, [expandedThreadMessageIds, pendingCollapsedThreadMessageId]);

  useEffect(() => () => {
    if (threadScrollFrameRef.current !== null) window.cancelAnimationFrame(threadScrollFrameRef.current);
  }, []);

  useEffect(() => {
    const root = threadScrollRef.current;
    const content = threadScrollContentRef.current;
    if (!root || !content) return;
    return observeScrollGeometry(root, content, (geometry, viewportOnly) => {
      threadGeometryRef.current = geometry;
      if (viewportOnly) return;
      if (shouldFollowThreadRef.current) scrollThreadToBottom();
      else restoreThreadScrollAnchor(false);
    });
  }, [activeRoot?.id]);

  useLayoutEffect(() => {
    threadGeometryRef.current = { scrollHeight: 0, clientHeight: 0 };
    if (!activeRoot?.id) {
      shouldFollowThreadRef.current = true;
      setShowBackToBottom(false);
      return;
    }
    if (restoreThreadScrollState(activeRoot.id)) return;
    shouldFollowThreadRef.current = true;
    setShowBackToBottom(false);
    scrollThreadToBottom();
  }, [activeRoot?.id]);

  useLayoutEffect(() => {
    // While the user has jumped to a referenced message, don't auto-follow to
    // bottom — otherwise each reply/agent-activity update yanks them back down.
    if (focusedMessageId) return;
    preserveThreadViewport();
  }, [activeRoot?.id, focusedMessageId, activeRoot?.updated_at, replies.length, lastReply?.id, lastReply?.updated_at, lastReply?.delivery_state]);

  useLayoutEffect(() => {
    if (!focusedMessageId) {
      focusedThreadMessageScrollKeyRef.current = null;
      return;
    }
    const focusedScrollKey = `${activeRoot?.id ?? "none"}:${focusedMessageId}`;
    if (focusedThreadMessageScrollKeyRef.current === focusedScrollKey) return;
    let frameId = 0;
    let settleFrameId = 0;
    let attemptsRemaining = 6;
    function scrollFocusedThreadMessage() {
      const scrollRoot = threadScrollRef.current;
      const element = scrollRoot?.querySelector<HTMLElement>(`[data-message-id="${focusedMessageId}"]`);
      if (element) {
        focusedThreadMessageScrollKeyRef.current = focusedScrollKey;
        // Detach from bottom-follow so the jump target stays put once focus clears.
        stopFollowingThread();
        element.scrollIntoView({ block: "center" });
        settleFrameId = window.requestAnimationFrame(() => {
          const currentScrollRoot = threadScrollRef.current;
          if (currentScrollRoot) rememberThreadScrollMetrics(currentScrollRoot);
        });
        return;
      }
      // The target reply may render a few frames later (channel hydration,
      // freshly merged historical messages) — retry before giving up.
      if (attemptsRemaining <= 0) return;
      attemptsRemaining -= 1;
      frameId = window.requestAnimationFrame(scrollFocusedThreadMessage);
    }
    scrollFocusedThreadMessage();
    return () => {
      if (frameId) window.cancelAnimationFrame(frameId);
      if (settleFrameId) window.cancelAnimationFrame(settleFrameId);
    };
  }, [activeRoot?.id, focusedMessageId, replies.length, lastReply?.id, lastReply?.updated_at, lastReply?.delivery_state]);

  async function copyMessageMarkdown(message: Message) {
    await copyText(messageToMarkdown(message, surfaceLabel));
    setMessageMenu(null);
  }

  async function copyMessageLink(message: Message) {
    await copyText(messageShareLink(message, shareBaseUrl));
    setMessageMenu(null);
  }

  function setActiveThreadExpandedMessageIds(nextMessageIds: Set<string>) {
    if (!activeThreadExpansionKey) return;
    setExpandedThreadMessageStateByThread((current) => {
      const now = Date.now();
      const next = pruneThreadMessageExpansionState(current, now);
      next.set(activeThreadExpansionKey, { messageIds: nextMessageIds, lastTouchedAt: now });
      return pruneThreadMessageExpansionState(next, now);
    });
  }

  function updateActiveThreadExpandedMessageIds(updater: (current: Set<string>) => Set<string>) {
    if (!activeThreadExpansionKey) return;
    setExpandedThreadMessageStateByThread((current) => {
      const now = Date.now();
      const currentEntry = current.get(activeThreadExpansionKey);
      const currentMessageIds = currentEntry && now - currentEntry.lastTouchedAt <= THREAD_MESSAGE_EXPANSION_TTL_MS
        ? currentEntry.messageIds
        : new Set<string>();
      const nextMessageIds = updater(currentMessageIds);
      const next = pruneThreadMessageExpansionState(current, now);
      next.set(activeThreadExpansionKey, { messageIds: nextMessageIds, lastTouchedAt: now });
      return pruneThreadMessageExpansionState(next, now);
    });
  }

  function toggleThreadMessageExpanded(messageId: string) {
    if (expandedThreadMessageIds.has(messageId)) {
      stopFollowingThread();
      setPendingCollapsedThreadMessageId(messageId);
    }
    updateActiveThreadExpandedMessageIds((current) => {
      const next = new Set(current);
      if (next.has(messageId)) next.delete(messageId);
      else next.add(messageId);
      return next;
    });
  }

  function expandAllThreadMessages() {
    if (!hasCollapsibleThreadMessages || areAllThreadMessagesExpanded) return;
    stopFollowingThread();
    setPendingCollapsedThreadMessageId(null);
    setActiveThreadExpandedMessageIds(new Set(collapsibleThreadMessageIds));
  }

  function foldAllThreadMessages() {
    if (!hasCollapsibleThreadMessages || areAllThreadMessagesFolded) return;
    stopFollowingThread();
    setPendingCollapsedThreadMessageId(null);
    setActiveThreadExpandedMessageIds(new Set());
  }

  async function exportThreadVectorImage() {
    const threadPanel = threadPanelRef.current;
    if (!activeRoot || !threadPanel) return;
    const previousExpandedMessageIds = new Set(expandedThreadMessageIds);
    const shouldTemporarilyExpand = collapsibleThreadMessageIds.some((messageId) => !previousExpandedMessageIds.has(messageId));
    if (shouldTemporarilyExpand) {
      setPendingCollapsedThreadMessageId(null);
      setActiveThreadExpandedMessageIds(new Set(collapsibleThreadMessageIds));
      await waitForNextFrame();
      await waitForNextFrame();
    }
    try {
      await downloadThreadPanelSvg(threadPanel, surfaceLabel);
    } finally {
      if (shouldTemporarilyExpand) setActiveThreadExpandedMessageIds(previousExpandedMessageIds);
    }
  }

  const activeTaskAssignee = activeTask
    ? agentsById.get(activeTask.assignee_id ?? "") ?? null
    : null;
  const taskAssigneeOptions = channelAgents.length > 0 ? channelAgents : agents;
  const mentionAgents = useMemo(
    () => mentionableAgentsForChannel(channel, agents, channelAgents),
    [agents, channel, channelAgents],
  );
  const taskWorkItems = activeTask
    ? agentWorkItems
        .filter((item) => item.task_id === activeTask.id)
        .sort((left, right) => new Date(right.created_at).getTime() - new Date(left.created_at).getTime())
    : [];
  const taskRunIds = new Set(taskWorkItems.map((item) => item.run_id).filter(Boolean));
  const taskActivities = activeTask
    ? agentActivities
        .filter((activity) =>
          (activity.run_id && taskRunIds.has(activity.run_id)) ||
          metadataString(activity.metadata, "task_id") === activeTask.id)
        .filter((activity) => !isNoisyTaskActivity(activity))
        .sort((left, right) => new Date(right.created_at).getTime() - new Date(left.created_at).getTime())
        .slice(0, 12)
    : [];
  const latestFinishedTaskWorkItem = taskWorkItems.find((item) => ["done", "failed", "cancelled", "silent"].includes(item.status));
  const showTaskReviewActions = Boolean(activeTask && activeTask.status === "in_review" && latestFinishedTaskWorkItem?.status === "done");

  return (
    <aside className="thread" ref={threadPanelRef}>
      <button
        className="thread-resize-handle"
        aria-label="Resize thread panel"
        onPointerDown={onResizeStart}
      />
      <header>
        <button
          type="button"
          className="thread-mobile-back"
          onClick={onClose}
          aria-label={isDm ? "Back to direct message" : "Back to channel"}
        >
          <ArrowLeft size={18} />
        </button>
        <div className="thread-title">
          <span className="hash-card thread-title-card" aria-hidden="true">
            <MessageSquare size={21} />
          </span>
          <h2>
            Thread <span>{channel ? isDm ? `- @${dmAgent?.handle || "agent"}` : `- #${channel.name}` : "- no channel"}</span>
          </h2>
        </div>
        <span className="thread-mobile-actions">
          <button
            type="button"
            className="thread-mobile-actions-trigger"
            aria-label="Thread actions"
            aria-expanded={showMobileThreadActions}
            onClick={() => setShowMobileThreadActions((current) => !current)}
          >
            <MoreHorizontal size={20} />
          </button>
          {showMobileThreadActions && (
            <span className="thread-mobile-actions-menu" role="menu" aria-label="Thread actions">
              <button
                type="button"
                role="menuitem"
                disabled={!activeRoot}
                onClick={() => {
                  setShowMobileThreadActions(false);
                  exportThreadVectorImage();
                }}
              >
                <FileImage size={18} />
                <span>Export as SVG</span>
              </button>
              <button
                type="button"
                role="menuitem"
                disabled={!hasCollapsibleThreadMessages || areAllThreadMessagesExpanded}
                onClick={() => {
                  setShowMobileThreadActions(false);
                  expandAllThreadMessages();
                }}
              >
                <Maximize2 size={18} />
                <span>Expand all messages</span>
              </button>
              <button
                type="button"
                role="menuitem"
                disabled={!hasCollapsibleThreadMessages || areAllThreadMessagesFolded}
                onClick={() => {
                  setShowMobileThreadActions(false);
                  foldAllThreadMessages();
                }}
              >
                <Minimize2 size={18} />
                <span>Fold all messages</span>
              </button>
              <button
                type="button"
                role="menuitem"
                disabled={!activeRoot}
                onClick={() => {
                  setShowMobileThreadActions(false);
                  if (activeRoot) onLocateRoot(activeRoot);
                }}
              >
                <Crosshair size={18} />
                <span>{isDm ? "Locate in DM" : "Locate in channel"}</span>
              </button>
              <button
                type="button"
                role="menuitem"
                disabled={!activeRoot}
                onClick={() => {
                  setShowMobileThreadActions(false);
                  if (activeRoot) insertMessageReference(activeRoot, "thread");
                }}
              >
                <Quote size={18} />
                <span>Reference thread</span>
              </button>
            </span>
          )}
        </span>
        <span className="thread-header-actions">
          <button
            type="button"
            className="thread-export-svg"
            onClick={exportThreadVectorImage}
            aria-disabled={!activeRoot}
            data-tooltip="Export thread as SVG"
            title="Export thread as SVG"
            aria-label="Export thread as SVG"
          >
            <FileImage size={18} />
          </button>
          <button
            type="button"
            className="thread-expand-all"
            onClick={expandAllThreadMessages}
            aria-disabled={!hasCollapsibleThreadMessages || areAllThreadMessagesExpanded}
            data-tooltip="Expand all messages in this thread"
            title="Expand all messages in this thread"
            aria-label="Expand all messages in this thread"
          >
            <Maximize2 size={18} />
          </button>
          <button
            type="button"
            className="thread-fold-all"
            onClick={foldAllThreadMessages}
            aria-disabled={!hasCollapsibleThreadMessages || areAllThreadMessagesFolded}
            data-tooltip="Fold all messages in this thread"
            title="Fold all messages in this thread"
            aria-label="Fold all messages in this thread"
          >
            <Minimize2 size={18} />
          </button>
          <button
            type="button"
            className="thread-locate-root"
            onClick={() => {
              if (activeRoot) onLocateRoot(activeRoot);
            }}
            disabled={!activeRoot}
            data-tooltip={isDm ? "Locate this thread in the DM" : "Locate this thread in the channel"}
            aria-label={isDm ? "Locate this thread in the DM" : "Locate this thread in the channel"}
          >
            <Crosshair size={18} />
          </button>
          <button
            type="button"
            className="thread-locate-root"
            onClick={() => {
              if (activeRoot) insertMessageReference(activeRoot, "thread");
            }}
            disabled={!activeRoot}
            data-tooltip="Reference this thread"
            aria-label="Reference this thread"
          >
            <Quote size={18} />
          </button>
          <button type="button" className="thread-close" onClick={onClose} aria-label="Close thread panel"><X size={18} /></button>
        </span>
      </header>

      <section className="thread-focus">
        <div className="thread-scroll-shell">
          <div className="thread-progress-layer">
            <ActivityProgressDock progress={progress} onOpenWorkItem={openWorkItem} />
          </div>
          <div
            ref={threadScrollRef}
            className="thread-scroll"
            onScroll={handleThreadScroll}
            onWheelCapture={handleThreadWheel}
            onPointerDownCapture={handleThreadPointerDown}
            onTouchMoveCapture={handleThreadTouchMove}
            onLoadCapture={handleThreadContentLoad}
          >
            <div ref={threadScrollContentRef} className="thread-scroll-content">
            {activeRoot && <MessageRow
              key={activeRoot.id} data={rows[activeRoot.id]} actions={rowActions} variant="thread-root" dateDivider
              saved={savedMessageIds.has(activeRoot.id)} expanded={expandedThreadMessageIds.has(activeRoot.id)}
              jumpFocused={focusedMessageId === activeRoot.id} tapFocused={tapFocusedMessageId === activeRoot.id}
              showImageThumbnails={showImageThumbnails}
            />}

          {activeTask && (
            <section className="thread-task-card">
              <div className="task-card-head">
                <span>Task #{activeTask.number}</span>
                <strong>{taskStatusLabel(activeTask.status)}</strong>
              </div>
              <input
                value={taskTitleDrafts[activeTask.id] ?? activeTask.title}
                onChange={(event) => setTaskTitleDraft(activeTask, event.target.value)}
                onBlur={() => saveTaskTitle(activeTask)}
                onKeyDown={(event) => {
                  if (isImeComposing(event)) return;
                  if (event.key === "Enter") saveTaskTitle(activeTask);
                }}
              />
              <TaskAssigneePicker
                agents={taskAssigneeOptions}
                assignee={activeTaskAssignee}
                disabled={activeTask.status === "done"}
                done={activeTask.status === "done"}
                onChange={(agentId) => claimTask(activeTask, agentId)}
                taskNumber={activeTask.number}
              />
              <div className="status-row">
                {TASK_STATUSES.map((status) => (
                  <button
                    type="button"
                    key={status}
                    className={activeTask.status === status ? "active" : ""}
                    data-state={status}
                    onClick={() => updateTaskStatus(activeTask, status)}
                  >
                    {taskStatusLabel(status)}
                  </button>
                ))}
              </div>
              {showTaskReviewActions && (
                <div className="task-review-actions" aria-label={`Review task #${activeTask.number}`}>
                  <button type="button" onClick={() => updateTaskStatus(activeTask, "done")}>
                    <CheckCircle2 size={15} /> Done
                  </button>
                  <button type="button" onClick={() => updateTaskStatus(activeTask, "in_progress")}>
                    <RotateCcw size={15} /> Follow-up
                  </button>
                </div>
              )}
              <div className="task-execution-panel">
                <div className="task-execution-head">
                  <strong>Execution</strong>
                  <span>{taskActivities.length || taskWorkItems.length ? `${taskActivities.length || taskWorkItems.length} events` : "No runs yet"}</span>
                </div>
                {taskActivities.length > 0 ? (
                  <div className="task-execution-timeline">
                    {taskActivities.map((activity) => {
                      const detail = activityDetailText(activity);
                      return (
                        <div className="task-execution-row" key={activity.id}>
                          <time>{formatClockTime(activity.created_at)}</time>
                          <span className="activity-dot" data-kind={activity.kind} data-status={activity.status} />
                          <div>
                            <strong>{taskActivityLabel(activity)}</strong>
                            <small>{activity.agent_handle ? `@${activity.agent_handle}` : "Lantor"} · {activity.status}</small>
                            {detail && <p>{detail}</p>}
                          </div>
                        </div>
                      );
                    })}
                  </div>
                ) : taskWorkItems.length > 0 ? (
                  <div className="task-execution-timeline">
                    {taskWorkItems.slice(0, 6).map((item) => (
                      <div className="task-execution-row" key={item.id}>
                        <time>{formatClockTime(item.created_at)}</time>
                        <span className="activity-dot" data-kind="task" data-status={item.status === "failed" ? "error" : item.status === "done" ? "success" : "active"} />
                        <div>
                          <strong>{item.title}</strong>
                          <small>@{item.agent_handle} · {item.status}</small>
                        </div>
                      </div>
                    ))}
                  </div>
                ) : (
                  <p className="task-execution-empty">Assign an agent to start execution.</p>
                )}
              </div>
            </section>
          )}

          {activeRoot && (
            <div className="thread-replies-divider" aria-label="Beginning of replies">
              <span />
              <div>
                <strong>Beginning of replies</strong>
                <small>{replies.length} {replies.length === 1 ? "reply" : "replies"}</small>
              </div>
              <span />
            </div>
          )}

          <section className="reply-list">
            {!activeRoot && (
              <div className="empty-state compact">
                <MessageSquare size={28} />
                <h2>No thread selected</h2>
                <p>Select a root message after you create one.</p>
              </div>
            )}
            {replies.map((reply, index) => <MessageRow
              key={reply.id} data={rows[reply.id]} actions={rowActions} variant="reply"
              compact={isCompactFollowupMessage(reply, replies[index - 1])}
              dateDivider={index === 0 || !isSameCalendarDay(reply.created_at, replies[index - 1]?.created_at ?? "")}
              saved={savedMessageIds.has(reply.id)} expanded={expandedThreadMessageIds.has(reply.id)}
              jumpFocused={focusedMessageId === reply.id} tapFocused={tapFocusedMessageId === reply.id}
              showImageThumbnails={showImageThumbnails}
            />)}
          </section>
          <div className="thread-bottom-anchor" aria-hidden="true" />
          {messageMenu && (
            <MessageActionMenu
              x={messageMenu.x}
              y={messageMenu.y}
              isSaved={savedMessageIds.has(messageMenu.message.id)}
              onCopyLink={() => copyMessageLink(messageMenu.message)}
              onCopyMarkdown={() => copyMessageMarkdown(messageMenu.message)}
              onCopyReferenceMessage={() => copyMessageReference(messageMenu.message, "message")}
              onCopyReferenceThread={() => copyMessageReference(messageMenu.message, "thread")}
              onReferenceMessage={() => insertMessageReference(messageMenu.message, "message")}
              onReferenceThread={() => insertMessageReference(messageMenu.message, "thread")}
              onToggleSaved={() => {
                onToggleMessageSaved(messageMenu.message, !savedMessageIds.has(messageMenu.message.id));
                setMessageMenu(null);
              }}
              onClose={() => setMessageMenu(null)}
            />
          )}
            </div>
          </div>
          {activeRoot && showBackToBottom && (
            <button type="button" className="thread-back-to-bottom" onClick={returnThreadToBottom}>
              <ArrowDown size={15} />
              Back to bottom
            </button>
          )}
        </div>

        <ThreadReplyComposer
          activeRoot={activeRoot}
          isDm={isDm}
          dmAgent={dmAgent}
          mentionAgents={mentionAgents}
          channels={channels}
          replyDraft={replyDraft}
          replyAttachments={replyAttachments}
          setReplyDraft={setReplyDraft}
          resolveReferencePreviewItems={referencePreviewItemsForText}
          removeDraftReference={removeDraftReference}
          addReplyAttachments={addReplyAttachments}
          removeReplyAttachment={removeReplyAttachment}
          sendReply={sendReply}
        />
      </section>

    </aside>
  );
}

type ThreadReplyComposerProps = {
  activeRoot: Message | null;
  isDm: boolean;
  dmAgent: Agent | null;
  mentionAgents: Agent[];
  channels: Channel[];
  replyDraft: string;
  replyAttachments: DraftAttachment[];
  setReplyDraft: (value: string) => void;
  resolveReferencePreviewItems: (text: string) => MessageReferencePreviewItem[];
  removeDraftReference: (token: string) => void;
  addReplyAttachments: (files: FileList | File[]) => void;
  removeReplyAttachment: (id: string) => void;
  sendReply: (bodyOverride?: string, attachmentsOverride?: DraftAttachment[]) => void;
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
    setText((current) => current === value ? current : value);
  }

  function commitText(value = textRef.current) {
    if (value === committedRef.current) return;
    committedRef.current = value;
    setDraftRef.current(value);
  }

  function markCommitted(value: string) {
    textRef.current = value;
    committedRef.current = value;
    setText((current) => current === value ? current : value);
  }

  return { text, updateText, commitText, markCommitted };
}

function ThreadReplyComposer({
  activeRoot,
  isDm,
  dmAgent,
  mentionAgents,
  channels,
  replyDraft,
  replyAttachments,
  setReplyDraft,
  resolveReferencePreviewItems,
  removeDraftReference,
  addReplyAttachments,
  removeReplyAttachment,
  sendReply,
}: ThreadReplyComposerProps) {
  const [isReplyDragOver, setIsReplyDragOver] = useState(false);
  const replyDragDepthRef = useRef(0);
  const replyCompositionRef = useRef(false);
  const ignoreReplyCompositionEndRef = useRef(false);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const shouldUseShortPlaceholder = useMobileViewport();
  const usesSoftKeyboard = useCoarsePointer();
  const { text, updateText, commitText, markCommitted } = useBufferedComposerText(replyDraft, activeRoot?.id, setReplyDraft);
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
  const referencePreviewItems = useMemo(() => resolveReferencePreviewItems(text), [resolveReferencePreviewItems, text]);

  useEffect(() => {
    replyDragDepthRef.current = 0;
    setIsReplyDragOver(false);
    closeMentionPicker();
  }, [activeRoot?.id]);

  function submitReply() {
    const body = textareaRef.current?.value ?? text;
    if (!activeRoot || (!body.trim() && replyAttachments.length === 0)) return;
    if (replyCompositionRef.current) ignoreReplyCompositionEndRef.current = true;
    replyCompositionRef.current = false;
    markCommitted("");
    if (textareaRef.current) textareaRef.current.value = "";
    sendReply(body, replyAttachments);
    closeMentionPicker();
    focusComposer();
  }

  function handleReplyKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (isImeComposing(event)) return;
    if (handleMentionKeyDown(event)) return;
    if (!usesSoftKeyboard && event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      submitReply();
    }
  }

  function handleReplyDragEnter(event: DragEvent<HTMLElement>) {
    if (!hasDraggedFiles(event)) return;
    event.preventDefault();
    event.stopPropagation();
    replyDragDepthRef.current += 1;
    event.dataTransfer.dropEffect = activeRoot ? "copy" : "none";
    if (activeRoot) setIsReplyDragOver(true);
  }

  function handleReplyDragOver(event: DragEvent<HTMLElement>) {
    if (!hasDraggedFiles(event)) return;
    event.preventDefault();
    event.stopPropagation();
    event.dataTransfer.dropEffect = activeRoot ? "copy" : "none";
    if (activeRoot) setIsReplyDragOver(true);
  }

  function handleReplyDragLeave(event: DragEvent<HTMLElement>) {
    if (!hasDraggedFiles(event)) return;
    event.preventDefault();
    event.stopPropagation();
    replyDragDepthRef.current = Math.max(0, replyDragDepthRef.current - 1);
    if (replyDragDepthRef.current === 0) setIsReplyDragOver(false);
  }

  function handleReplyDrop(event: DragEvent<HTMLElement>) {
    if (!hasDraggedFiles(event)) return;
    event.preventDefault();
    event.stopPropagation();
    replyDragDepthRef.current = 0;
    setIsReplyDragOver(false);
    if (!activeRoot || event.dataTransfer.files.length === 0) return;
    addReplyAttachments(event.dataTransfer.files);
    focusComposer();
  }

  function handleReplyPaste(event: ClipboardEvent<HTMLTextAreaElement>) {
    const imageFiles = Array.from(event.clipboardData.files).filter((file) => file.type.startsWith("image/"));
    if (imageFiles.length === 0) return;
    event.preventDefault();
    if (!activeRoot) return;
    addReplyAttachments(imageFiles);
    focusComposer();
  }

  function applyReplyText(value: string, cursor: number | null) {
    updateText(value);
    refreshMentionState(value, cursor ?? value.length);
  }

  const fullReplyPlaceholder = activeRoot
    ? isDm ? `Reply to @${dmAgent?.handle || "agent"}` : "Reply in thread"
    : "Select a thread to reply";
  const replyPlaceholder = shouldUseShortPlaceholder
    ? activeRoot ? "Reply" : "No thread"
    : fullReplyPlaceholder;

  return (
    <section
      className={`reply-composer ${isReplyDragOver ? "drag-over" : ""}`}
      onDragEnter={handleReplyDragEnter}
      onDragOver={handleReplyDragOver}
      onDragLeave={handleReplyDragLeave}
      onDrop={handleReplyDrop}
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
          if (event.target.files) addReplyAttachments(event.target.files);
          event.target.value = "";
        }}
      />
      <MessageReferencePreview
        items={referencePreviewItems}
        variant="composer"
        onRemove={(item) => {
          if (!item.token) return;
          const nextText = removeMessageReferenceToken(text, item.token);
          updateText(nextText);
          removeDraftReference(item.token);
        }}
      />
      <DraftAttachmentsPreview attachments={replyAttachments} onRemove={removeReplyAttachment} />
      <ComposerReferenceTextarea
        ref={textareaRef}
        {...disableWritingSuggestionsAttrs}
        rows={1}
        value={text}
        autoCapitalize="none"
        autoComplete="off"
        autoCorrect="off"
        spellCheck={false}
        onCompositionStart={() => {
          replyCompositionRef.current = true;
          ignoreReplyCompositionEndRef.current = false;
        }}
        onCompositionEnd={(event) => {
          replyCompositionRef.current = false;
          if (ignoreReplyCompositionEndRef.current) {
            ignoreReplyCompositionEndRef.current = false;
            event.currentTarget.value = "";
            markCommitted("");
            return;
          }
          applyReplyText(event.currentTarget.value, event.currentTarget.selectionStart);
        }}
        onChange={(event) => {
          if (replyCompositionRef.current || isInputComposing(event)) return;
          applyReplyText(event.target.value, event.target.selectionStart);
        }}
        onBlur={(event) => {
          replyCompositionRef.current = false;
          applyReplyText(event.currentTarget.value, event.currentTarget.selectionStart);
          commitText(event.currentTarget.value);
        }}
        onSelect={(event) => refreshMentionState(text, event.currentTarget.selectionStart)}
        onKeyDown={handleReplyKeyDown}
        onPaste={handleReplyPaste}
        disabled={!activeRoot}
        placeholder={replyPlaceholder}
        aria-label={fullReplyPlaceholder}
      />
      <div className="reply-composer-actions">
        <button
          type="button"
          className="attach-button"
          disabled={!activeRoot}
          onClick={() => fileInputRef.current?.click()}
        >
          <Paperclip size={15} />
        </button>
        <button
          type="button"
          className="reply-send"
          title="Send reply"
          aria-label="Send reply"
          disabled={!activeRoot || (!text.trim() && replyAttachments.length === 0)}
          onClick={submitReply}
        >
          <Send size={17} />
        </button>
      </div>
    </section>
  );
}
