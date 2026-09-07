import { Hash, MessageSquare, PanelRightOpen, X } from "lucide-react";
import { useSyncExternalStore, type CSSProperties, type KeyboardEvent, type MouseEvent } from "react";
import { createRoot } from "react-dom/client";
import type { Channel, Message } from "../types";
import { formatTime } from "../ui-utils";
import {
  type ResolvedMessageReference,
  parseMessageReferences,
  resolveMessageReference,
  withoutMessageReferenceTokens,
} from "../message-references";

type MessageReferenceCardProps = {
  reference: ResolvedMessageReference;
  compact?: boolean;
  removable?: boolean;
  onOpen?: (reference: ResolvedMessageReference) => void;
  onRemove?: (token: string) => void;
};

export function firstLinePreview(body: string, limit = 140) {
  const normalized = body.replace(/\s+/g, " ").trim();
  if (!normalized) return "Empty message";
  return normalized.length > limit ? `${normalized.slice(0, limit - 1).trimEnd()}...` : normalized;
}

function firstLinesPreview(body: string, maxLines = 3) {
  const lines = withoutMessageReferenceTokens(body)
    .replace(/\r/g, "")
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean);
  if (lines.length === 0) return "Empty message";
  return lines.slice(0, maxLines).join("\n");
}

function hovercardStyle(rect: DOMRect): CSSProperties {
  const MAX_WIDTH = 320;
  const left = Math.max(12, Math.min(rect.left, window.innerWidth - MAX_WIDTH - 12));
  const placeAbove = rect.bottom > window.innerHeight - 168;
  return {
    position: "fixed",
    left,
    maxWidth: MAX_WIDTH,
    pointerEvents: "none",
    ...(placeAbove
      ? { bottom: window.innerHeight - rect.top + 6 }
      : { top: rect.bottom + 6 }),
  };
}

function referenceLabel(reference: ResolvedMessageReference) {
  return reference.kind === "thread" ? "Thread" : "Message";
}

function referenceMeta(reference: ResolvedMessageReference) {
  const parts = [
    reference.channel ? `#${reference.channel.name}` : "Unknown channel",
    reference.message?.sender_name ?? "Unknown sender",
  ];
  if (reference.kind === "thread" && reference.replyCount !== null) {
    parts.push(`${reference.replyCount} ${reference.replyCount === 1 ? "reply" : "replies"}`);
  }
  if (reference.message) parts.push(formatTime(reference.message.created_at));
  return parts.join(" · ");
}

// Hover-preview state lives in a module-level singleton, not inside each chip.
// The inline chip is rendered by react-markdown, whose subtree can re-render /
// remount whenever the message feed refreshes (e.g. agent activity ticks). If
// the hover state lived in the chip component, that remount would drop it and
// the preview card would flicker away mid-hover. Keeping it here — with a single
// portal host rendered once into <body> — makes the card survive any chip
// remount: the anchored chip only reports enter/leave, it never owns the card.
type ReferenceHoverState = { reference: ResolvedMessageReference; rect: DOMRect } | null;

let hoverState: ReferenceHoverState = null;
const hoverListeners = new Set<() => void>();
let hoverLayerMounted = false;

function emitHoverChange() {
  for (const listener of hoverListeners) listener();
}

function subscribeHover(listener: () => void) {
  hoverListeners.add(listener);
  return () => {
    hoverListeners.delete(listener);
  };
}

function getHoverSnapshot(): ReferenceHoverState {
  return hoverState;
}

function hideReferenceHover() {
  if (!hoverState) return;
  hoverState = null;
  emitHoverChange();
}

function showReferenceHover(reference: ResolvedMessageReference, rect: DOMRect) {
  hoverState = { reference, rect };
  ensureHoverLayerMounted();
  emitHoverChange();
}

function ensureHoverLayerMounted() {
  if (hoverLayerMounted || typeof document === "undefined") return;
  hoverLayerMounted = true;
  const host = document.createElement("div");
  host.className = "reference-hover-root";
  document.body.appendChild(host);
  // Any scroll invalidates the anchored rect, so close rather than let the card
  // drift away from its chip.
  window.addEventListener("scroll", hideReferenceHover, true);
  createRoot(host).render(<ReferenceHoverLayer />);
}

function ReferenceHoverLayer() {
  const state = useSyncExternalStore(subscribeHover, getHoverSnapshot, getHoverSnapshot);
  if (!state) return null;
  const { reference, rect } = state;
  const message = reference.message;
  if (!message) return null;
  return (
    <div className={`message-reference-hovercard reference-${reference.kind}`} style={hovercardStyle(rect)} role="tooltip">
      <div className="message-reference-hovercard-head">
        <strong>{referenceLabel(reference)}</strong>
        <span>{referenceMeta(reference)}</span>
      </div>
      <div className="message-reference-hovercard-body">{firstLinesPreview(message.body)}</div>
    </div>
  );
}

export function MessageReferenceCard({
  reference,
  compact = false,
  removable = false,
  onOpen,
  onRemove,
}: MessageReferenceCardProps) {
  const Icon = reference.kind === "thread" ? PanelRightOpen : MessageSquare;
  const preview = reference.message
    ? firstLinePreview(reference.message.body, compact ? 96 : 150)
    : `Missing ${reference.kind} ${reference.id.slice(0, 8)}`;
  const className = [
    "message-reference-card",
    `reference-${reference.kind}`,
    compact ? "compact" : "",
    reference.message ? "" : "missing",
  ].filter(Boolean).join(" ");
  const openProps = {
    role: onOpen ? "button" : undefined,
    tabIndex: onOpen ? 0 : undefined,
    onClick: (event: MouseEvent) => {
      event.stopPropagation();
      onOpen?.(reference);
    },
    onKeyDown: (event: KeyboardEvent) => {
      if (!onOpen) return;
      if (event.key !== "Enter" && event.key !== " ") return;
      event.preventDefault();
      onOpen(reference);
    },
  };

  if (compact) {
    const channelLabel = reference.channel ? `#${reference.channel.name}` : "Unknown channel";
    const senderLabel = reference.message?.sender_name ?? "Unknown";
    return (
      <span
        className={className}
        {...openProps}
        onMouseEnter={(event) => {
          if (reference.message) showReferenceHover(reference, event.currentTarget.getBoundingClientRect());
        }}
        onMouseLeave={hideReferenceHover}
      >
        <span className="message-reference-icon" aria-hidden="true">
          <Icon size={13} />
        </span>
        <span className="message-reference-inline-copy">
          <strong>{referenceLabel(reference)}</strong>
          <span>{channelLabel} · {senderLabel}</span>
        </span>
      </span>
    );
  }

  return (
    <span
      className={className}
      {...openProps}
      title={reference.message ? preview : reference.token}
    >
      <span className="message-reference-icon" aria-hidden="true">
        <Icon size={compact ? 15 : 16} />
      </span>
      <span className="message-reference-copy">
        <span className="message-reference-kicker">
          <strong>{referenceLabel(reference)}</strong>
          <span>{referenceMeta(reference)}</span>
        </span>
        <span className="message-reference-preview">{preview}</span>
      </span>
      {reference.channel && (
        <span className="message-reference-channel" aria-hidden="true">
          <Hash size={12} />
        </span>
      )}
      {removable && (
        <button
          type="button"
          className="message-reference-remove"
          aria-label={`Remove ${reference.kind} reference`}
          onClick={(event) => {
            event.stopPropagation();
            onRemove?.(reference.token);
          }}
        >
          <X size={14} />
        </button>
      )}
    </span>
  );
}

type MessageReferencePreviewProps = {
  text: string;
  messages: Message[];
  channels: Channel[];
  onOpen?: (reference: ResolvedMessageReference) => void;
  onRemove?: (token: string) => void;
};

export function MessageReferencePreview({
  text,
  messages,
  channels,
  onOpen,
  onRemove,
}: MessageReferencePreviewProps) {
  const references = parseMessageReferences(text).map((reference) => resolveMessageReference(reference, messages, channels));
  if (references.length === 0) return null;
  return (
    <div className="message-reference-preview-list" aria-label="Message references">
      {references.map((reference) => (
        <MessageReferenceCard
          key={`${reference.kind}:${reference.id}`}
          reference={reference}
          removable={Boolean(onRemove)}
          onOpen={onOpen}
          onRemove={onRemove}
        />
      ))}
    </div>
  );
}
