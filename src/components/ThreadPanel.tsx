import { ArrowDown, ArrowLeft, Bookmark, CheckCircle2, Hash, MessageSquare, Paperclip, RotateCcw, Send, X } from "lucide-react";
import { Fragment, useEffect, useLayoutEffect, useRef, useState, type ClipboardEvent, type DragEvent, type KeyboardEvent, type PointerEvent as ReactPointerEvent } from "react";
import { useAutoGrowTextarea } from "../hooks/useAutoGrowTextarea";
import { useMentionPicker } from "../hooks/useMentionPicker";
import { APP_DISPLAY_NAME } from "../branding";
import { isImeComposing } from "../input-utils";
import { copyText } from "../clipboard";
import { isCompactFollowupMessage, wasEdited } from "../message-grouping";
import { messageShareLink, messageToMarkdown } from "../message-share";
import { Agent, AgentActivity, AgentRun, AgentWorkItem, Artifact, Channel, DraftAttachment, Message, OwnerProfile, TASK_STATUSES, Task } from "../types";
import { agentForMessageSender, deletedAgentForMessageSender, formatClockTime, formatDateDivider, formatTime, isSameCalendarDay, ownerAsAvatarAgent, visibleAgentDescription, visibleChannelDescription } from "../ui-utils";
import { ActivityProgressDock } from "./ActivityProgressDock";
import { AgentAvatar, AgentAvatarWithProfile } from "./AgentAvatar";
import { DraftAttachmentsPreview } from "./DraftAttachmentsPreview";
import { MessageActionMenu } from "./MessageActionMenu";
import { MessageAttachments } from "./MessageAttachments";
import { MessageArtifacts } from "./MessageArtifacts";
import { MessageMarkdown } from "./MessageMarkdown";
import { TaskAssigneePicker } from "./TaskAssigneePicker";

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
  shareBaseUrl: string | null;
  savedMessageIds: Set<string>;
  focusedMessageId: string | null;
  onToggleMessageSaved: (message: Message, saved: boolean) => void;
  onResizeStart: (event: ReactPointerEvent<HTMLButtonElement>) => void;
};

type MessageMenuState = {
  x: number;
  y: number;
  message: Message;
} | null;

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
  shareBaseUrl,
  savedMessageIds,
  focusedMessageId,
  onToggleMessageSaved,
  onResizeStart,
}: ThreadPanelProps) {
  const [showBackToBottom, setShowBackToBottom] = useState(false);
  const [messageMenu, setMessageMenu] = useState<MessageMenuState>(null);
  const [tapFocusedMessageId, setTapFocusedMessageId] = useState<string | null>(null);
  const threadScrollRef = useRef<HTMLDivElement | null>(null);
  const shouldFollowThreadRef = useRef(true);
  function openLinkedAgentDetail(handle: string) {
    const agent = agents.find((candidate) => candidate.handle.toLowerCase() === handle.toLowerCase());
    if (agent) openAgentDetail(agent);
  }
  const isDm = channel?.kind === "dm";
  const dmAgent = isDm ? agents.find((agent) => agent.id === channel?.dm_agent_id) ?? null : null;
  const rootAgent = activeRoot ? agentForMessageSender(activeRoot, agents) : null;
  const deletedRootAgent = activeRoot && !rootAgent ? deletedAgentForMessageSender(activeRoot) : null;
  const rootSaved = activeRoot ? savedMessageIds.has(activeRoot.id) : false;
  const surfaceLabel = channel
    ? isDm
      ? `Thread in DM with @${dmAgent?.handle || "agent"}`
      : `Thread in #${channel.name}`
    : `${APP_DISPLAY_NAME} thread`;
  const lastReply = replies[replies.length - 1] ?? null;

  function isThreadScrollAtBottom(element: HTMLDivElement) {
    return element.scrollHeight - element.scrollTop - element.clientHeight < 32;
  }

  function scrollThreadToBottom(behavior: ScrollBehavior = "auto") {
    const element = threadScrollRef.current;
    if (!element) return;
    element.scrollTo({ top: element.scrollHeight, behavior });
  }

  function handleThreadScroll() {
    const element = threadScrollRef.current;
    if (!element) return;
    const atBottom = isThreadScrollAtBottom(element);
    shouldFollowThreadRef.current = atBottom;
    const shouldShow = Boolean(activeRoot) && !atBottom;
    setShowBackToBottom((current) => current === shouldShow ? current : shouldShow);
  }

  function handleThreadContentLoad() {
    if (!shouldFollowThreadRef.current) return;
    scrollThreadToBottom();
  }

  function returnThreadToBottom() {
    shouldFollowThreadRef.current = true;
    setShowBackToBottom(false);
    scrollThreadToBottom();
    window.requestAnimationFrame(() => {
      scrollThreadToBottom();
      shouldFollowThreadRef.current = true;
      setShowBackToBottom(false);
    });
  }

  useEffect(() => {
    setMessageMenu(null);
    setTapFocusedMessageId(null);
  }, [activeRoot?.id]);

  useLayoutEffect(() => {
    shouldFollowThreadRef.current = true;
    setShowBackToBottom(false);
    scrollThreadToBottom();
  }, [activeRoot?.id]);

  useLayoutEffect(() => {
    if (!shouldFollowThreadRef.current) return;
    scrollThreadToBottom();
  }, [activeRoot?.id, activeRoot?.updated_at, replies.length, lastReply?.id, lastReply?.updated_at, lastReply?.delivery_state]);

  useEffect(() => {
    if (!focusedMessageId) return;
    const element = threadScrollRef.current?.querySelector<HTMLElement>(`[data-message-id="${focusedMessageId}"]`);
    element?.scrollIntoView({ block: "center" });
  }, [activeRoot?.id, focusedMessageId, replies.length]);

  function hasSelectedText() {
    return Boolean(window.getSelection()?.toString().trim());
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

  const activeTaskAssignee = activeTask
    ? agents.find((agent) => agent.id === activeTask.assignee_id) ?? null
    : null;
  const taskAssigneeOptions = channelAgents.length > 0 ? channelAgents : agents;
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
    <aside className="thread">
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
        <button type="button" className="thread-close" onClick={onClose} aria-label="Close thread panel"><X size={18} /></button>
      </header>

      <section className="thread-focus">
        <div
          ref={threadScrollRef}
          className="thread-scroll"
          onScroll={handleThreadScroll}
          onLoadCapture={handleThreadContentLoad}
        >
          <ActivityProgressDock
            messages={replies}
            activities={agentActivities}
            runs={agentRuns}
            workItems={agentWorkItems}
            agents={agents}
            channelId={activeRoot ? channel?.id ?? null : null}
            threadRootId={activeRoot?.id ?? null}
          />
          {activeRoot && (
            <Fragment>
              <div className="message-date-divider" role="separator">
                <span />
                <time dateTime={activeRoot.created_at}>{formatDateDivider(activeRoot.created_at)}</time>
                <span />
              </div>
              <article
                data-message-id={activeRoot.id}
                className={`thread-root ${activeRoot.sender_role === "system" ? "system-message" : ""} ${tapFocusedMessageId === activeRoot.id ? "tap-focused" : ""} ${rootSaved ? "saved" : ""}`}
                data-jump-focused={focusedMessageId === activeRoot.id ? "true" : "false"}
                onClick={() => {
                  if (hasSelectedText()) return;
                  if (activeRoot.sender_role !== "system") setTapFocusedMessageId(activeRoot.id);
                }}
                onContextMenu={(event) => {
                  if (activeRoot.sender_role === "system") return;
                  if (shouldUseNativeMessageSelection()) return;
                  event.preventDefault();
                  setMessageMenu({ x: event.clientX, y: event.clientY, message: activeRoot });
                }}
              >
                {activeRoot.sender_role === "system" ? (
                  <div className="system-message-line">
                    <MessageMarkdown body={activeRoot.body} onLocalAgentLink={openLinkedAgentDetail} />
                    <time>{formatTime(activeRoot.created_at)}</time>
                  </div>
                ) : (
                  <div className="thread-message-with-avatar">
                    {rootAgent ? (
                      <button
                        type="button"
                        className="message-agent-avatar-trigger"
                        aria-label={`View @${rootAgent.handle} details`}
                        onClick={(event) => {
                          event.stopPropagation();
                          openAgentDetail(rootAgent);
                        }}
                      >
                        <AgentAvatarWithProfile agent={rootAgent} />
                      </button>
                    ) : deletedRootAgent ? (
                      <AgentAvatar
                        agent={deletedRootAgent}
                        size="md"
                        title={`@${deletedRootAgent.handle} has been deleted`}
                      />
                    ) : activeRoot.sender_role === "owner" ? (
                      <AgentAvatar agent={ownerAsAvatarAgent(ownerProfile)} size="md" showStatus={false} />
                    ) : (
                      <div className="avatar">{activeRoot.sender_name.slice(0, 1)}</div>
                    )}
                    <div className="thread-message-content">
                      <div className="meta">
                        <strong>{activeRoot.sender_name}</strong>
                        <span>{activeRoot.sender_role}</span>
                        <time>{formatTime(activeRoot.created_at)}</time>
                        {wasEdited(activeRoot) && <span className="edited-indicator">edited</span>}
                        <button
                          type="button"
                          className={`message-save-button mobile-message-save-tag ${rootSaved ? "saved" : ""}`}
                          title={rootSaved ? "Unsave message" : "Save message"}
                          aria-label={rootSaved ? "Unsave message" : "Save message"}
                          aria-pressed={rootSaved}
                          onPointerDown={(event) => event.stopPropagation()}
                          onClick={(event) => {
                            event.stopPropagation();
                            onToggleMessageSaved(activeRoot, !rootSaved);
                          }}
                        >
                          <Bookmark size={14} />
                        </button>
                      </div>
                      <div className="message-hover-actions" aria-label="Message actions">
                        <button
                          type="button"
                          className={rootSaved ? "saved" : ""}
                          data-tooltip={rootSaved ? "Unsave" : "Save"}
                          title={rootSaved ? "Unsave message" : "Save message"}
                          aria-label={rootSaved ? "Unsave message" : "Save message"}
                          onPointerDown={(event) => event.stopPropagation()}
                          onClick={(event) => {
                            event.stopPropagation();
                            onToggleMessageSaved(activeRoot, !rootSaved);
                          }}
                        >
                          <Bookmark size={14} />
                        </button>
                      </div>
                      {activeRoot.delivery_state !== "streaming" && (
                        <MessageMarkdown body={activeRoot.body} onLocalAgentLink={openLinkedAgentDetail} />
                      )}
                      <MessageAttachments attachments={activeRoot.attachments} />
                      <MessageArtifacts artifacts={activeRoot.artifacts} onOpenArtifact={openArtifact} />
                      {activeRoot.delivery_state === "sending" && (
                        <div className="message-stream-state sending">Sending...</div>
                      )}
                      {activeRoot.delivery_state === "error" && (
                        <div className="message-stream-state error">Response interrupted</div>
                      )}
                    </div>
                  </div>
                )}
              </article>
            </Fragment>
          )}

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
            {replies.map((reply, index) => {
              const replyAgent = agentForMessageSender(reply, agents);
              const deletedReplyAgent = replyAgent ? null : deletedAgentForMessageSender(reply);
              const replySaved = savedMessageIds.has(reply.id);
              const isCompact = isCompactFollowupMessage(reply, replies[index - 1]);
              const showDateDivider = index === 0 || !isSameCalendarDay(reply.created_at, replies[index - 1]?.created_at ?? "");
              if (reply.sender_role === "system") {
                return (
                  <Fragment key={reply.id}>
                    {showDateDivider && (
                      <div className="message-date-divider" role="separator">
                        <span />
                        <time dateTime={reply.created_at}>{formatDateDivider(reply.created_at)}</time>
                        <span />
                      </div>
                    )}
                    <article className="system-message">
                      <div className="system-message-line">
                        <MessageMarkdown body={reply.body} onLocalAgentLink={openLinkedAgentDetail} />
                        <time>{formatTime(reply.created_at)}</time>
                      </div>
                    </article>
                  </Fragment>
                );
              }
              return (
                <Fragment key={reply.id}>
                  {showDateDivider && (
                    <div className="message-date-divider" role="separator">
                      <span />
                      <time dateTime={reply.created_at}>{formatDateDivider(reply.created_at)}</time>
                      <span />
                    </div>
                  )}
                  <article
                    data-message-id={reply.id}
                    className={`${isCompact ? "compact" : ""} ${replySaved ? "saved" : ""} ${tapFocusedMessageId === reply.id ? "tap-focused" : ""}`}
                    data-jump-focused={focusedMessageId === reply.id ? "true" : "false"}
                    onClick={() => {
                      if (hasSelectedText()) return;
                      setTapFocusedMessageId(reply.id);
                    }}
                    onContextMenu={(event) => {
                      if (shouldUseNativeMessageSelection()) return;
                      event.preventDefault();
                      setMessageMenu({ x: event.clientX, y: event.clientY, message: reply });
                    }}
                  >
                    {isCompact ? (
                      <time className="message-compact-time" dateTime={reply.created_at}>
                        {formatClockTime(reply.created_at)}
                      </time>
                    ) : replyAgent ? (
                      <button
                        type="button"
                        className="message-agent-avatar-trigger"
                        aria-label={`View @${replyAgent.handle} details`}
                        onClick={(event) => {
                          event.stopPropagation();
                          openAgentDetail(replyAgent);
                        }}
                      >
                        <AgentAvatarWithProfile agent={replyAgent} />
                      </button>
                    ) : deletedReplyAgent ? (
                      <AgentAvatar
                        agent={deletedReplyAgent}
                        size="md"
                        title={`@${deletedReplyAgent.handle} has been deleted`}
                      />
                    ) : reply.sender_role === "owner" ? (
                      <AgentAvatar agent={ownerAsAvatarAgent(ownerProfile)} size="md" showStatus={false} />
                    ) : (
                      <div className="avatar">{reply.sender_name.slice(0, 1)}</div>
                    )}
                    <div className="reply-body">
                      {!isCompact && (
                        <div className="meta">
                          <strong>{reply.sender_name}</strong>
                          <span>{reply.sender_role}</span>
                          <time>{formatTime(reply.created_at)}</time>
                          {wasEdited(reply) && <span className="edited-indicator">edited</span>}
                          <button
                            type="button"
                            className={`message-save-button mobile-message-save-tag ${replySaved ? "saved" : ""}`}
                            title={replySaved ? "Unsave message" : "Save message"}
                            aria-label={replySaved ? "Unsave message" : "Save message"}
                            aria-pressed={replySaved}
                            onPointerDown={(event) => event.stopPropagation()}
                            onClick={(event) => {
                              event.stopPropagation();
                              onToggleMessageSaved(reply, !replySaved);
                            }}
                          >
                            <Bookmark size={14} />
                          </button>
                        </div>
                      )}
                      <div className="message-hover-actions" aria-label="Message actions">
                        <button
                          type="button"
                          className={replySaved ? "saved" : ""}
                          data-tooltip={replySaved ? "Unsave" : "Save"}
                          title={replySaved ? "Unsave message" : "Save message"}
                          aria-label={replySaved ? "Unsave message" : "Save message"}
                          onPointerDown={(event) => event.stopPropagation()}
                          onClick={(event) => {
                            event.stopPropagation();
                            onToggleMessageSaved(reply, !replySaved);
                          }}
                        >
                          <Bookmark size={14} />
                        </button>
                      </div>
                      {reply.delivery_state !== "streaming" && (
                        <MessageMarkdown body={reply.body} onLocalAgentLink={openLinkedAgentDetail} />
                      )}
                      <MessageAttachments attachments={reply.attachments} />
                      <MessageArtifacts artifacts={reply.artifacts} onOpenArtifact={openArtifact} />
                      {reply.delivery_state === "sending" && (
                        <div className="message-stream-state sending">Sending...</div>
                      )}
                      {reply.delivery_state === "error" && (
                        <div className="message-stream-state error">Response interrupted</div>
                      )}
                    </div>
                  </article>
                </Fragment>
              );
            })}
          </section>
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
          agents={agents}
          channels={channels}
          replyDraft={replyDraft}
          replyAttachments={replyAttachments}
          setReplyDraft={setReplyDraft}
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
  agents: Agent[];
  channels: Channel[];
  replyDraft: string;
  replyAttachments: DraftAttachment[];
  setReplyDraft: (value: string) => void;
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

function ThreadReplyComposer({
  activeRoot,
  isDm,
  dmAgent,
  agents,
  channels,
  replyDraft,
  replyAttachments,
  setReplyDraft,
  addReplyAttachments,
  removeReplyAttachment,
  sendReply,
}: ThreadReplyComposerProps) {
  const [isReplyDragOver, setIsReplyDragOver] = useState(false);
  const replyDragDepthRef = useRef(0);
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
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
  } = useMentionPicker({ agents, channels, value: text, setValue: updateText, textareaRef });
  useAutoGrowTextarea(textareaRef, text);

  useEffect(() => {
    replyDragDepthRef.current = 0;
    setIsReplyDragOver(false);
    closeMentionPicker();
  }, [activeRoot?.id]);

  function submitReply() {
    if (!activeRoot || (!text.trim() && replyAttachments.length === 0)) return;
    const body = text;
    markCommitted("");
    sendReply(body, replyAttachments);
    closeMentionPicker();
    focusComposer();
  }

  function handleReplyKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (isImeComposing(event)) return;
    if (handleMentionKeyDown(event)) return;
    if (event.key === "Enter" && !event.shiftKey) {
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
      <DraftAttachmentsPreview attachments={replyAttachments} onRemove={removeReplyAttachment} />
      <textarea
        ref={textareaRef}
        value={text}
        onChange={(event) => {
          updateText(event.target.value);
          refreshMentionState(event.target.value, event.target.selectionStart);
        }}
        onBlur={() => commitText()}
        onSelect={(event) => refreshMentionState(text, event.currentTarget.selectionStart)}
        onKeyDown={handleReplyKeyDown}
        onPaste={handleReplyPaste}
        disabled={!activeRoot}
        placeholder={activeRoot ? isDm ? `Reply to @${dmAgent?.handle || "agent"}` : "Reply in thread" : "Select a thread to reply"}
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
