import { Copy, Hash, MessageSquare, X } from "lucide-react";
import { useState, type PointerEvent } from "react";
import { copyText } from "../clipboard";
import { messageReferenceToken, type MessageReferenceKind } from "../message-references";

export type MessageReferencePreviewItem = {
  key: string;
  kind: MessageReferenceKind;
  id: string;
  token?: string;
  channelName: string;
  senderName: string;
  preview: string;
  meta: string;
  missing?: boolean;
};

type MessageReferencePreviewProps = {
  items: MessageReferencePreviewItem[];
  variant: "composer" | "message";
  collapsedLimit?: number;
  onRemove?: (item: MessageReferencePreviewItem) => void;
  onOpen?: (item: MessageReferencePreviewItem) => void;
};

export function MessageReferencePreview({
  items,
  variant,
  collapsedLimit = 3,
  onRemove,
  onOpen,
}: MessageReferencePreviewProps) {
  const [expanded, setExpanded] = useState(false);
  function preserveComposerSelection(event: PointerEvent<HTMLButtonElement>) {
    event.stopPropagation();
    // Keep the textarea/caret and soft keyboard in place while using its
    // reference toolbar. Blurring on pointerdown can move the button before click.
    if (variant === "composer") event.preventDefault();
  }
  if (items.length === 0) return null;
  const shouldCollapse = items.length > collapsedLimit;
  const visibleItems = shouldCollapse && !expanded ? items.slice(0, collapsedLimit) : items;
  const hiddenCount = items.length - visibleItems.length;

  return (
    <div className={`message-reference-stack reference-${variant}`}>
      {items.length > 1 && (
        <div className="message-reference-stack-summary">
          <span>{items.length} references attached</span>
          <button
            type="button"
            onPointerDown={preserveComposerSelection}
            onClick={() => {
              void copyText(items.map((item) => item.token ?? messageReferenceToken(item.kind, item.id)).join("\n"));
            }}
          >
            <Copy size={13} />
            Copy all
          </button>
        </div>
      )}
      {visibleItems.map((item) => (
        <div
          key={item.key}
          className={`message-reference-card reference-${item.kind} ${item.missing ? "missing" : ""}`}
          role={onOpen ? "button" : undefined}
          tabIndex={onOpen ? 0 : undefined}
          onClick={(event) => {
            if (!onOpen) return;
            event.stopPropagation();
            onOpen(item);
          }}
          onKeyDown={(event) => {
            if (!onOpen || event.key !== "Enter") return;
            event.preventDefault();
            onOpen(item);
          }}
        >
          <span className="message-reference-rail" aria-hidden="true" />
          <span className="message-reference-icon" aria-hidden="true">
            {item.kind === "thread" ? <MessageSquare size={15} /> : <Hash size={15} />}
          </span>
          <span className="message-reference-copy">
            <span className="message-reference-title">
              <strong>{item.kind === "thread" ? "Thread" : "Message"}</strong>
              <span>#{item.channelName}</span>
              {item.meta && <em>{item.meta}</em>}
            </span>
          <span className="message-reference-preview">
            <b>{item.senderName}</b>
            <span>{item.preview}</span>
          </span>
          </span>
          <button
            type="button"
            className="message-reference-remove"
            aria-label={`Copy ${item.kind} reference`}
            title={`Copy ${item.kind} reference`}
            onPointerDown={preserveComposerSelection}
            onClick={(event) => {
              event.stopPropagation();
              void copyText(item.token ?? messageReferenceToken(item.kind, item.id));
            }}
          >
            <Copy size={14} />
          </button>
          {onRemove && (
            <button
              type="button"
              className="message-reference-remove"
              aria-label={`Remove ${item.kind} reference`}
              onPointerDown={preserveComposerSelection}
              onClick={(event) => {
                event.stopPropagation();
                onRemove(item);
              }}
            >
              <X size={14} />
            </button>
          )}
        </div>
      ))}
      {shouldCollapse && (
        <button
          type="button"
          className="message-reference-stack-toggle"
          onPointerDown={preserveComposerSelection}
          onClick={() => setExpanded((current) => !current)}
        >
          {expanded ? "Show fewer references" : `+${hiddenCount} more ${hiddenCount === 1 ? "reference" : "references"}`}
        </button>
      )}
    </div>
  );
}
