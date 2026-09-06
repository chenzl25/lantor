import {
  ArrowLeft,
  ArrowRight,
  BookOpen,
  CheckCircle2,
  Flag,
  Github,
  Hash,
  LayoutList,
  MessageSquare,
  Paperclip,
  Send,
  Settings,
  Trash2,
  UserPlus,
} from "lucide-react";
import { useEffect, useLayoutEffect, useMemo, useRef, useState, type ClipboardEvent, type DragEvent, type FocusEvent, type MouseEvent as ReactMouseEvent, type KeyboardEvent, type PointerEvent as ReactPointerEvent, type TextareaHTMLAttributes, type WheelEvent as ReactWheelEvent } from "react";
import { useEventCallback } from "../hooks/useEventCallback";
import { useMessageRows } from "../hooks/useMessageRows";
import { useRetainedValue } from "../hooks/useRetainedValue";
import { useAutoGrowTextarea } from "../hooks/useAutoGrowTextarea";
import { useCoarsePointer } from "../hooks/useCoarsePointer";
import { useMentionPicker } from "../hooks/useMentionPicker";
import { useMobileViewport } from "../hooks/useMobileViewport";
import { isImeComposing, isInputComposing } from "../input-utils";
import { mentionableAgentsForChannel } from "../mentions";
import { copyText } from "../clipboard";
import { APP_DISPLAY_NAME } from "../branding";
import { observeScrollGeometry } from "../scroll-geometry";
import { isCompactFollowupMessage } from "../message-grouping";
import { messageShareLink, messageToMarkdown } from "../message-share";
import { appendMessageReferenceToken, messageReferenceToken, parseMessageReferences, removeMessageReferenceToken, withoutMessageReferenceTokens, type MessageReferenceKind, type ResolvedMessageReference } from "../message-references";
import { Agent, AgentActivity, AgentRun, AgentWorkItem, Artifact, Channel, DraftAttachment, GithubIssueTaskResult, GithubReviewTaskResult, Message, OwnerProfile, TASK_STATUSES, Task, ThreadReplySummary } from "../types";
import { formatTime, isSameCalendarDay, visibleAgentDescription, visibleChannelDescription } from "../ui-utils";
import { ActivityProgressDock, activeProgressByAgent, indexProgress, type ActiveAgentProgress } from "./ActivityProgressDock";
import { AgentAvatar, AgentAvatarWithProfile } from "./AgentAvatar";
import { ComposerReferenceTextarea } from "./ComposerReferenceTextarea";
import { DraftAttachmentsPreview } from "./DraftAttachmentsPreview";
import { GithubPanel } from "./GithubPanel";
import { WikiPanel } from "./WikiPanel";
import { MessageActionMenu } from "./MessageActionMenu";
import { MessageRow, type MessageRowActions, type MessageRowAction } from "./MessageRow";
import { MessageReferencePreview, type MessageReferencePreviewItem } from "./MessageReferencePreview";
import { TaskAssigneePicker } from "./TaskAssigneePicker";
import { UnreadBadge } from "./UnreadBadge";

type WritingSuggestionsTextareaAttrs = TextareaHTMLAttributes<HTMLTextAreaElement> & { "writingsuggestions": "false" };

const disableWritingSuggestionsAttrs: WritingSuggestionsTextareaAttrs = { writingsuggestions: "false" };

type ConversationProps = {
  channel: Channel | null;
  channels: Channel[];
  agents: Agent[];
  ownerProfile: OwnerProfile;
  agentActivities: AgentActivity[];
  agentRuns: AgentRun[];
  agentWorkItems: AgentWorkItem[];
  channelAgents: Agent[];
  activeTab: "chat" | "tasks" | "github" | "wiki";
  activeRoot: Message | null;
  rootMessages: Message[];
  messages: Message[];
  threadReplyCounts: Record<string, number>;
  threadUnreadCounts: Record<string, number>;
  threadReplySummaries: Record<string, ThreadReplySummary>;
  visibleTasks: Task[];
  draft: string;
  draftAttachments: DraftAttachment[];
  taskTitleDrafts: Record<string, string>;
  setActiveTab: (tab: "chat" | "tasks" | "github" | "wiki") => void;
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
  createGithubReviewTask: (pullNumber: number, agentId: string) => Promise<GithubReviewTaskResult>;
  createGithubIssueTask: (issueNumber: number, agentId: string) => Promise<GithubIssueTaskResult>;
  setDraft: (value: string) => void;
  addDraftAttachments: (files: FileList | File[]) => void;
  removeDraftAttachment: (id: string) => void;
  sendRootMessage: (asTask?: boolean, bodyOverride?: string, attachmentsOverride?: DraftAttachment[]) => void;
  openAgentDetail: (agent: Agent) => void;
  openArtifact: (artifact: Artifact) => void;
  openWorkItem?: (item: AgentWorkItem, focusedMessageIdOverride?: string | null) => void;
  onReferenceMessageJump: (originMessageId: string, targetMessageId: string) => void;
  onReferenceThreadJump: (originMessageId: string, threadId: string) => void;
  shareBaseUrl: string | null;
  savedMessageIds: Set<string>;
  focusedMessageId: string | null;
  showImageThumbnails: boolean;
  hasMoreRootMessages: boolean;
  isLoadingOlderRootMessages: boolean;
  onLoadOlderRootMessages: () => Promise<void>;
  onToggleMessageSaved: (message: Message, saved: boolean) => void;
};

type MessageMenuState = {
  x: number;
  y: number;
  message: Message;
} | null;

const LOAD_OLDER_SCROLL_TOP_PX = 96;

function taskStatusLabel(status: string) {
  return status.replace("_", " ");
}


function compactReferencePreview(body: string) {
  const text = withoutMessageReferenceTokens(body).replace(/\s+/g, " ").trim();
  if (!text) return "No text preview";
  return text.length > 140 ? `${text.slice(0, 139).trimEnd()}...` : text;
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
  messages,
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
  createGithubReviewTask,
  createGithubIssueTask,
  setDraft,
  addDraftAttachments,
  removeDraftAttachment,
  sendRootMessage,
  openAgentDetail,
  openArtifact,
  openWorkItem,
  onReferenceMessageJump,
  onReferenceThreadJump,
  shareBaseUrl,
  savedMessageIds,
  focusedMessageId,
  showImageThumbnails,
  hasMoreRootMessages,
  isLoadingOlderRootMessages,
  onLoadOlderRootMessages,
  onToggleMessageSaved,
}: ConversationProps) {
  const [showChannelActions, setShowChannelActions] = useState(false);
  const [messageMenu, setMessageMenu] = useState<MessageMenuState>(null);
  const [expandedChannelMessageIds, setExpandedChannelMessageIds] = useState<Set<string>>(() => new Set());
  const messageListRef = useRef<HTMLDivElement | null>(null);
  const messageListContentRef = useRef<HTMLDivElement | null>(null);
  const messageListBottomAnchorRef = useRef<HTMLDivElement | null>(null);
  const bottomScrollFrameRef = useRef<number | null>(null);
  const messageListGeometryRef = useRef({ scrollHeight: 0, clientHeight: 0 });
  const shouldFollowMessagesRef = useRef(true);
  const focusedMessageScrollKeyRef = useRef<string | null>(null);
  const userMessageScrollUntilRef = useRef(0);
  const messageListMetricsRef = useRef({ scrollHeight: 0, scrollTop: 0, clientHeight: 0 });
  const olderMessagesAnchorRef = useRef<{ element: HTMLElement; top: number } | null>(null);
  const olderMessagesLoadInFlightRef = useRef(false);
  const messageListContextEpochRef = useRef(0);
  const channelActionsRef = useRef<HTMLDivElement | null>(null);
  const isDm = channel?.kind === "dm";
  const agentsById = useMemo(() => new Map(agents.map((agent) => [agent.id, agent])), [agents]);
  const dmAgent = isDm ? agentsById.get(channel?.dm_agent_id ?? "") ?? null : null;
  const openLinkedAgentDetail = useEventCallback((handle: string) => {
    const agent = agents.find((candidate) => candidate.handle.toLowerCase() === handle.toLowerCase());
    if (agent) openAgentDetail(agent);
  });
  const channelId = channel?.id ?? null;
  const progressIndex = useMemo(() => indexProgress(agentActivities, agentRuns, agentWorkItems, agents),
    [agentActivities, agentRuns, agentWorkItems, agents]);
  const progressState = useRetainedValue(useMemo(() => {
    const byRoot: Record<string, ActiveAgentProgress[]> = {};
    if (channelId) {
      const surfaces = progressIndex.workItemsByChannel.get(channelId);
      for (const message of rootMessages) {
        if (!surfaces?.has(message.id)) continue;
        const progress = activeProgressByAgent([], progressIndex, channelId, message.id);
        if (progress.length) byRoot[message.id] = progress;
      }
    }
    return { byRoot, dock: activeProgressByAgent(rootMessages, progressIndex, channelId, null) };
  }, [progressIndex, rootMessages, channelId]));
  const { rows, referenceStore } = useMessageRows(rootMessages, messages, channels, agents, ownerProfile, isDm, threadReplySummaries, progressState.byRoot);
  const lastRootMessage = rootMessages[rootMessages.length - 1] ?? null;
  const { activeTasks, reviewTasks, unassignedTasks, assignedTasks } = useMemo(() => ({
    activeTasks: visibleTasks.filter((task) => task.status !== "done"),
    reviewTasks: visibleTasks.filter((task) => task.status === "in_review"),
    unassignedTasks: visibleTasks.filter((task) => task.status !== "done" && !task.assignee_id),
    assignedTasks: visibleTasks.filter((task) => task.assignee_id || task.status === "done"),
  }), [visibleTasks]);
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
  const rootMessageById = useMemo(() => new Map(rootMessages.map((message) => [message.id, message])), [rootMessages]);
  const channelNameById = useMemo(() => new Map(channels.map((value) => [value.id, value.name])), [channels]);

  function messageReferencePreviewItem(kind: MessageReferenceKind, id: string, token?: string): MessageReferencePreviewItem {
    const message = rootMessageById.get(id);
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
    const replyCount = threadReplyCounts[message.id] ?? 0;
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
    targetRootMessageIntoView(reference.id);
  });

  const onRowAction = useEventCallback((message: Message, action: MessageRowAction) => {
    switch (action) {
      case "reference": insertMessageReference(message, "message"); break;
      case "save": onToggleMessageSaved(message, !savedMessageIds.has(message.id)); break;
      case "thread": setActiveThreadId(message.id); break;
      case "expand": toggleChannelMessageExpanded(message.id); break;
    }
  });
  const onRowMenu = useEventCallback((message: Message, x: number, y: number) => setMessageMenu({ message, x, y }));
  const onRowArtifact = useEventCallback(openArtifact);
  const rowActions = useMemo<MessageRowActions>(() => ({
    referenceStore,
    onAction: onRowAction, onMenu: onRowMenu, onArtifact: onRowArtifact,
    onAgent: openLinkedAgentDetail, onReference: handleReferenceOpen,
  }), [onRowAction, onRowMenu, onRowArtifact, openLinkedAgentDetail, handleReferenceOpen, referenceStore]);

  function insertMessageReference(message: Message, kind: MessageReferenceKind) {
    const referenceId = kind === "thread" ? (message.thread_root_id ?? message.id) : message.id;
    setDraft(appendMessageReferenceToken(draft, kind, referenceId));
    setMessageMenu(null);
  }

  async function copyMessageReference(message: Message, kind: MessageReferenceKind) {
    const referenceId = kind === "thread" ? (message.thread_root_id ?? message.id) : message.id;
    await copyText(messageReferenceToken(kind, referenceId));
    setMessageMenu(null);
  }

  function isMessageListAtBottom(element: HTMLDivElement) {
    return messageListDistanceFromBottom(element) < 32;
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

  function cancelPendingMessageBottomScroll() {
    if (bottomScrollFrameRef.current !== null) {
      window.cancelAnimationFrame(bottomScrollFrameRef.current);
      bottomScrollFrameRef.current = null;
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

  function scrollMessagesToBottom() {
    if (bottomScrollFrameRef.current !== null) return;
    bottomScrollFrameRef.current = window.requestAnimationFrame(() => {
      bottomScrollFrameRef.current = null;
      const element = messageListRef.current;
      if (!element || !shouldFollowMessagesRef.current) return;
      element.scrollTop = messageListGeometryRef.current.scrollHeight || element.scrollHeight;
    });
  }

  function handleMessageListScroll() {
    const element = messageListRef.current;
    if (!element) return;
    const pendingAnchor = olderMessagesAnchorRef.current;
    if (pendingAnchor && element.querySelector("article[data-message-id]") === pendingAnchor.element) {
      // Follow user movement while the fetch is pending, up until rows prepend.
      pendingAnchor.top = pendingAnchor.element.getBoundingClientRect().top;
    }
    if (
      activeTab === "chat" &&
      channel &&
      rootMessages.length > 0 &&
      hasMoreRootMessages &&
      !isLoadingOlderRootMessages &&
      !olderMessagesLoadInFlightRef.current &&
      element.scrollTop <= LOAD_OLDER_SCROLL_TOP_PX
    ) {
      const contextEpoch = messageListContextEpochRef.current;
      const messageList = element;
      const firstMessage = element.querySelector<HTMLElement>("article[data-message-id]");
      const anchor = firstMessage ? { element: firstMessage, top: firstMessage.getBoundingClientRect().top } : null;
      olderMessagesAnchorRef.current = anchor;
      stopFollowingMessages(element);
      olderMessagesLoadInFlightRef.current = true;
      void onLoadOlderRootMessages()
        .finally(() => {
          if (messageListContextEpochRef.current !== contextEpoch) return;
          window.requestAnimationFrame(() => {
            if (
              messageListContextEpochRef.current !== contextEpoch
              || messageListRef.current !== messageList
            ) return;
            const list = messageListRef.current;
            if (!list) return;
            // At scrollTop=0 native anchoring may be suppressed. Correct the
            // actual old row once, then let native anchoring handle later estimates.
            if (anchor?.element.isConnected) {
              list.scrollTop += anchor.element.getBoundingClientRect().top - anchor.top;
            }
            olderMessagesAnchorRef.current = null;
            olderMessagesLoadInFlightRef.current = false;
            rememberMessageListMetrics(list);
          });
        });
    }
    const atBottom = isMessageListAtBottom(element);
    const previous = messageListMetricsRef.current;
    const layoutChanged = previous.scrollHeight !== element.scrollHeight || previous.clientHeight !== element.clientHeight;
    const reachedEnd = Math.abs(messageListDistanceFromBottom(element)) <= 1;
    if (atBottom && (!isUserScrollingMessages() || reachedEnd)) {
      shouldFollowMessagesRef.current = true;
      userMessageScrollUntilRef.current = 0;
    } else if (!layoutChanged && element.scrollTop < previous.scrollTop) {
      // Includes keyboard, scrollbar and assistive scrolling, beyond wheel/touch.
      stopFollowingMessages(element);
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

  function targetRootMessageIntoView(messageId: string) {
    const list = messageListRef.current;
    const element = list?.querySelector<HTMLElement>(`[data-message-id="${messageId}"]`);
    if (!list || !element) return;
    stopFollowingMessages(list);
    element.scrollIntoView({ block: "center" });
    window.requestAnimationFrame(() => {
      const currentList = messageListRef.current;
      if (currentList) rememberMessageListMetrics(currentList);
    });
  }

  function handleMessageListContentLoad() {
    if (!shouldFollowMessagesRef.current) return;
    scrollMessagesToBottom();
  }

  useEffect(() => {
    if (!isDm) return;
    if (activeTab !== "chat") setActiveTab("chat");
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
    messageListGeometryRef.current = { scrollHeight: 0, clientHeight: 0 };
    shouldFollowMessagesRef.current = true;
    scrollMessagesToBottom();
  }, [channel?.id]);

  useLayoutEffect(() => {
    messageListContextEpochRef.current += 1;
    olderMessagesLoadInFlightRef.current = false;
    olderMessagesAnchorRef.current = null;
  }, [activeTab, channel?.id]);

  useEffect(() => () => {
    if (bottomScrollFrameRef.current !== null) window.cancelAnimationFrame(bottomScrollFrameRef.current);
  }, []);

  useEffect(() => {
    if (activeTab !== "chat") return;
    const root = messageListRef.current;
    const content = messageListContentRef.current;
    if (!root || !content) return;
    return observeScrollGeometry(root, content, (geometry, viewportOnly) => {
      messageListGeometryRef.current = geometry;
      // Composer resizing must keep the reading position. Follow the next
      // content growth using the user's existing stick-to-bottom choice.
      if (!viewportOnly && shouldFollowMessagesRef.current) scrollMessagesToBottom();
    });
  }, [activeTab, channel?.id]);

  useEffect(() => {
    setExpandedChannelMessageIds(new Set());
  }, [channel?.id]);

  useLayoutEffect(() => {
    // Don't auto-follow to bottom while the user has jumped to a referenced
    // message (clicked a reference chip). Otherwise every agent-activity refresh
    // bumps progressState and yanks them back down to the bottom.
    if (focusedMessageId) return;
    if (!shouldFollowMessagesRef.current) return;
    scrollMessagesToBottom();
  }, [
    activeTab,
    channel?.id,
    focusedMessageId,
    progressState,
    rootMessages.length,
    lastRootMessage?.id,
    lastRootMessage?.updated_at,
    lastRootMessage?.delivery_state,
  ]);

  useLayoutEffect(() => {
    if (!focusedMessageId) {
      focusedMessageScrollKeyRef.current = null;
      return;
    }
    const focusedMessageScrollKey = `${channel?.id ?? "none"}:${focusedMessageId}`;
    if (focusedMessageScrollKeyRef.current === focusedMessageScrollKey) return;
    let frameId = 0;
    let settleFrameId = 0;
    let attemptsRemaining = 6;
    function scrollFocusedMessage() {
      const list = messageListRef.current;
      const element = list?.querySelector<HTMLElement>(`[data-message-id="${focusedMessageId}"]`);
      if (element) {
        focusedMessageScrollKeyRef.current = focusedMessageScrollKey;
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
          <>
            <button className={activeTab === "tasks" ? "active" : ""} onClick={() => setActiveTab("tasks")}>
              <LayoutList size={16} /> Tasks
            </button>
            <button
              className={`${activeTab === "github" ? "active" : ""} ${channel?.github_unread_count ? "has-unread" : ""}`}
              onClick={() => setActiveTab("github")}
            >
              <Github size={16} />
              GitHub
              {Boolean(channel?.github_unread_count) && (
                <UnreadBadge
                  value={channel?.github_unread_count ?? 0}
                  className="tab-unread-badge"
                />
              )}
            </button>
            <button className={activeTab === "wiki" ? "active" : ""} onClick={() => setActiveTab("wiki")}>
              <BookOpen size={16} /> Wiki
            </button>
          </>
        )}
      </div>

      {activeTab === "chat" ? (
        <div className="message-list-shell">
          <div className="message-progress-layer">
            <ActivityProgressDock progress={progressState.dock} onOpenWorkItem={openWorkItem} />
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
                  <div className="beginning" aria-live="polite">
                    {hasMoreRootMessages
                      ? isLoadingOlderRootMessages
                        ? "Loading earlier messages..."
                        : "Earlier messages available"
                      : isDm
                        ? `Beginning of your DM with @${dmAgent?.handle || "agent"}`
                        : `Beginning of #${channel.name}`}
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
              const task = taskForMessage(message.id);
              return <MessageRow
                key={message.id} data={rows[message.id]} actions={rowActions} variant="channel"
                compact={isCompactFollowupMessage(message, rootMessages[index - 1])}
                dateDivider={index === 0 || !isSameCalendarDay(message.created_at, rootMessages[index - 1]?.created_at ?? "")}
                saved={savedMessageIds.has(message.id)} expanded={expandedChannelMessageIds.has(message.id)}
                focused={message.id === activeRoot?.id} jumpFocused={focusedMessageId === message.id}
                showImageThumbnails={showImageThumbnails}
                replyCount={threadReplyCounts[message.id] ?? 0} unreadReplyCount={threadUnreadCounts[message.id] ?? 0}
                taskNumber={task?.number} taskStatus={task?.status}
              />;
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
      ) : activeTab === "tasks" ? (
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
      ) : activeTab === "wiki" ? (
        channel ? <WikiPanel channel={channel} /> : null
      ) : channel ? (
        <GithubPanel
          channel={channel}
          agents={taskAssigneeOptions}
          onCreateReviewTask={createGithubReviewTask}
          onCreateIssueTask={createGithubIssueTask}
          onOpenThread={(threadRootId) => {
            setActiveThreadId(threadRootId);
            setActiveTab("chat");
          }}
        />
      ) : null}

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
          resolveReferencePreviewItems={referencePreviewItemsForText}
          addDraftAttachments={addDraftAttachments}
          removeDraftAttachment={removeDraftAttachment}
          sendRootMessage={sendRootMessage}
        />
      )}
    </section>
  );

  function renderTaskCard(task: Task) {
    const assignee = agentsById.get(task.assignee_id ?? "") ?? null;
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
  resolveReferencePreviewItems: (text: string) => MessageReferencePreviewItem[];
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

function ConversationComposer({
  channel,
  isDm,
  dmAgent,
  mentionAgents,
  channels,
  draft,
  draftAttachments,
  setDraft,
  resolveReferencePreviewItems,
  addDraftAttachments,
  removeDraftAttachment,
  sendRootMessage,
}: ConversationComposerProps) {
  const [sendAsTask, setSendAsTask] = useState(false);
  const [isComposerDragOver, setIsComposerDragOver] = useState(false);
  const composerDragDepthRef = useRef(0);
  const composerCompositionRef = useRef(false);
  const ignoreComposerCompositionEndRef = useRef(false);
  const taskToggleHandledAtRef = useRef(0);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const shouldUseShortPlaceholder = useMobileViewport();
  const usesSoftKeyboard = useCoarsePointer();
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
  const referencePreviewItems = useMemo(() => resolveReferencePreviewItems(text), [resolveReferencePreviewItems, text]);

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
    const body = textareaRef.current?.value ?? text;
    if (!channel || (!body.trim() && draftAttachments.length === 0)) return;
    if (composerCompositionRef.current) ignoreComposerCompositionEndRef.current = true;
    composerCompositionRef.current = false;
    markCommitted("");
    if (textareaRef.current) textareaRef.current.value = "";
    sendRootMessage(isDm ? false : sendAsTask, body, draftAttachments);
    closeMentionPicker();
    focusComposer();
  }

  function handleComposerKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (isImeComposing(event)) return;
    if (handleMentionKeyDown(event)) return;
    if (!usesSoftKeyboard && event.key === "Enter" && !event.shiftKey) {
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

  function applyComposerText(value: string, cursor: number | null) {
    updateText(value);
    refreshMentionState(value, cursor ?? value.length);
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
      <MessageReferencePreview
        items={referencePreviewItems}
        variant="composer"
        onRemove={(item) => {
          if (!item.token) return;
          const nextText = removeMessageReferenceToken(text, item.token);
          updateText(nextText);
          setDraft(nextText);
        }}
      />
      <DraftAttachmentsPreview attachments={draftAttachments} onRemove={removeDraftAttachment} />
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
          composerCompositionRef.current = true;
          ignoreComposerCompositionEndRef.current = false;
        }}
        onCompositionEnd={(event) => {
          composerCompositionRef.current = false;
          if (ignoreComposerCompositionEndRef.current) {
            ignoreComposerCompositionEndRef.current = false;
            event.currentTarget.value = "";
            markCommitted("");
            return;
          }
          applyComposerText(event.currentTarget.value, event.currentTarget.selectionStart);
        }}
        onChange={(event) => {
          if (composerCompositionRef.current || isInputComposing(event)) return;
          applyComposerText(event.target.value, event.target.selectionStart);
        }}
        onBlur={(event) => {
          composerCompositionRef.current = false;
          applyComposerText(event.currentTarget.value, event.currentTarget.selectionStart);
          commitText(event.currentTarget.value);
        }}
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
