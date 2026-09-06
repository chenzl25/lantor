import { Bookmark, CheckCircle2, MessageSquare, Quote } from "lucide-react";
import { memo, useCallback } from "react";
import type { MessageRowData } from "../hooks/useMessageRows";
import type { ResolvedMessageReference } from "../message-references";
import { isPrimaryUnmodifiedClick } from "../message-interactions";
import { messageHasVisibleContent, wasEdited } from "../message-grouping";
import { shouldCollapseMessage } from "../message-preview";
import type { Artifact, Message } from "../types";
import { formatClockTime, formatDateDivider, formatTime } from "../ui-utils";
import { AgentAvatar, AgentAvatarWithProfile } from "./AgentAvatar";
import { MessageAttachments } from "./MessageAttachments";
import { MessageArtifacts } from "./MessageArtifacts";
import { MessageMarkdown } from "./MessageMarkdown";
import { MessageReplySummary } from "./MessageReplySummary";

export type MessageRowAction = "reference" | "save" | "thread" | "expand" | "focus";
export type MessageRowActions = {
  onAction: (message: Message, action: MessageRowAction) => void;
  onMenu: (message: Message, x: number, y: number) => void;
  onAgent: (handle: string) => void;
  onReference: (sourceMessageId: string, reference: ResolvedMessageReference) => void;
  onArtifact: (artifact: Artifact) => void;
  onMount?: (messageId: string, node: HTMLElement | null) => void;
};

type MessageRowProps = {
  data: MessageRowData;
  actions: MessageRowActions;
  variant: "channel" | "thread-root" | "reply";
  compact?: boolean;
  dateDivider?: boolean;
  saved: boolean;
  expanded: boolean;
  focused?: boolean;
  jumpFocused: boolean;
  tapFocused?: boolean;
  showImageThumbnails: boolean;
  replyCount?: number;
  unreadReplyCount?: number;
  taskNumber?: number;
  taskStatus?: string;
};

const INTERACTIVE_TARGETS = "a,button,input,select,textarea,summary,[contenteditable='true'],[role='button'],[role='link'],.message-artifacts,.message-attachments";

export const MessageRow = memo(function MessageRow({
  data, actions, variant, compact = false, dateDivider = false, saved, expanded,
  focused = false, jumpFocused, tapFocused = false, showImageThumbnails,
  replyCount = 0, unreadReplyCount = 0, taskNumber, taskStatus,
}: MessageRowProps) {
  const { message, agent, deletedAgent, ownerAvatar, references, reply } = data;
  const channel = variant === "channel";
  const root = variant === "thread-root";
  const system = message.sender_role === "system";
  const collapsible = shouldCollapseMessage(message.body);
  const onMount = actions.onMount;
  const setNode = useCallback((node: HTMLElement | null) => onMount?.(message.id, node), [message.id, onMount]);
  const act = (action: MessageRowAction) => actions.onAction(message, action);
  const body = message.body.trim() ? <MessageMarkdown
    body={message.body}
    references={references}
    sourceMessageId={references ? message.id : undefined}
    onOpenReference={references ? actions.onReference : undefined}
    onLocalAgentLink={actions.onAgent}
    scrollKey={`message:${message.id}`}
  /> : null;
  const avatar = compact ? <time className="message-compact-time" dateTime={message.created_at}>{formatClockTime(message.created_at)}</time>
    : agent ? <button type="button" className="message-agent-avatar-trigger" aria-label={`View @${agent.handle} details`}
      onClick={(event) => { event.stopPropagation(); actions.onAgent(agent.handle); }}><AgentAvatarWithProfile agent={agent} /></button>
    : deletedAgent ? <AgentAvatar agent={deletedAgent} size="md" title={`@${deletedAgent.handle} has been deleted`} />
    : ownerAvatar ? <AgentAvatar agent={ownerAvatar} size="md" showStatus={false} />
    : <div className="avatar">{message.sender_name.slice(0, 1)}</div>;
  const saveLabel = saved ? "Unsave message" : "Save message";
  const contents = <>
    {avatar}
    <div className={channel ? "message-body" : root ? "thread-message-content" : "reply-body"}>
      {!compact && <div className="meta">
        <strong>{message.sender_name}</strong><span>{message.sender_role}</span><time>{formatTime(message.created_at)}</time>
        {wasEdited(message) && <span className="edited-indicator">edited</span>}
        {taskNumber !== undefined && <mark><CheckCircle2 size={14} /> #{taskNumber} · {taskStatus?.replace("_", " ")}</mark>}
        <button type="button" className={`message-save-button mobile-message-save-tag ${saved ? "saved" : ""}`}
          title={saveLabel} aria-label={saveLabel} aria-pressed={saved}
          onPointerDown={(event) => event.stopPropagation()}
          onClick={(event) => { event.stopPropagation(); act("save"); }}><Bookmark size={14} /></button>
      </div>}
      <div className="message-hover-actions" aria-label="Message actions">
        <button type="button" data-tooltip="Reference" title="Reference message" aria-label="Reference message"
          onPointerDown={(event) => event.stopPropagation()}
          onClick={(event) => { event.stopPropagation(); act("reference"); }}><Quote size={14} /></button>
        {channel && <button type="button" data-tooltip={replyCount > 0 ? "View thread" : "Reply in thread"}
          title={replyCount > 0 ? "View thread replies" : "Reply in thread"} aria-label={replyCount > 0 ? "View thread replies" : "Reply in thread"}
          onPointerDown={(event) => event.stopPropagation()}
          onClick={(event) => { event.stopPropagation(); if (isPrimaryUnmodifiedClick(event)) act("thread"); }}><MessageSquare size={14} /></button>}
        <button type="button" className={saved ? "saved" : ""} data-tooltip={saved ? "Unsave" : "Save"} title={saveLabel} aria-label={saveLabel}
          onPointerDown={(event) => event.stopPropagation()}
          onClick={(event) => { event.stopPropagation(); act("save"); }}><Bookmark size={14} /></button>
      </div>
      {(message.delivery_state !== "streaming" || messageHasVisibleContent(message)) && <>
        <div className={collapsible && !expanded ? "message-long-preview collapsed" : "message-long-preview"}>{body}</div>
        {collapsible && <button type="button" className="message-expand-button" aria-expanded={expanded}
          onPointerDown={(event) => event.stopPropagation()}
          onClick={(event) => { event.stopPropagation(); act("expand"); }}>{expanded ? "Show less" : "Show more"}</button>}
      </>}
      <MessageAttachments attachments={message.attachments} showImageThumbnails={showImageThumbnails} />
      <MessageArtifacts artifacts={message.artifacts} onOpenArtifact={actions.onArtifact} />
      {message.delivery_state === "sending" && <div className="message-stream-state sending">Sending...</div>}
      {message.delivery_state === "error" && <div className="message-stream-state error">Response interrupted</div>}
      {channel && reply && <MessageReplySummary reply={reply} replyCount={replyCount} unreadReplyCount={unreadReplyCount} onOpenThread={() => act("thread")} />}
    </div>
  </>;
  return <>
    {dateDivider && <div className="message-date-divider" role="separator"><span /><time dateTime={message.created_at}>{formatDateDivider(message.created_at)}</time><span /></div>}
    <article ref={setNode} data-message-id={message.id}
      className={[channel && !system ? "message-card" : root ? "thread-root" : "", system ? "system-message" : "", compact ? "compact" : "", focused ? "focused" : "", saved ? "saved" : "", tapFocused ? "tap-focused" : ""].filter(Boolean).join(" ")}
      data-jump-focused={jumpFocused ? "true" : "false"}
      onClick={(event) => {
        if (system || !isPrimaryUnmodifiedClick(event) || window.getSelection()?.toString().trim()) return;
        if (channel) {
          if (event.nativeEvent.composedPath().some((node) => node instanceof Element && node.matches(INTERACTIVE_TARGETS))
            || (event.target instanceof Element && event.target.closest(INTERACTIVE_TARGETS))) return;
          if (window.matchMedia("(max-width: 760px)").matches) act("thread");
        } else act("focus");
      }}
      onContextMenu={(event) => {
        if (system || window.matchMedia("(hover: none)").matches) return;
        event.preventDefault(); event.stopPropagation(); actions.onMenu(message, event.clientX, event.clientY);
      }}>
      {system ? <div className="system-message-line">{body}<time>{formatTime(message.created_at)}</time></div>
        : root ? <div className="thread-message-with-avatar">{contents}</div> : contents}
    </article>
  </>;
});
