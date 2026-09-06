import {
  Children,
  ReactNode,
  isValidElement,
  type MouseEvent,
  type PointerEvent,
  type UIEvent,
  useCallback,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import ReactMarkdown, { defaultUrlTransform, type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import type { PluggableList } from "unified";
import { openExternalUrl } from "../apiClient";
import { copyText } from "../clipboard";
import {
  MESSAGE_REFERENCE_PATTERN,
  type ResolvedMessageReference,
  resolveMessageReference,
} from "../message-references";
import type { Channel, Message } from "../types";
import { MessageReferenceCard } from "./MessageReferenceCard";

export type MessageMarkdownProps = {
  body: string;
  references?: Record<string, ResolvedMessageReference>;
  onLocalAgentLink?: (handle: string) => void;
  messages?: Message[];
  channels?: Channel[];
  sourceMessageId?: string;
  onOpenReference?: (sourceMessageId: string, reference: ResolvedMessageReference) => void;
  scrollKey?: string;
  enableLantorLinks?: boolean;
};

const INLINE_CODE_SPLIT = /(`[^`\n]*(?:`|$))/g;
const FENCE_SPLIT = /((?:^|\n)```[\s\S]*?(?:\n```|$))/g;
const LOCAL_ENTITY_PATH_PREFIX = "/lantor/";
const tableScrollPositions = new Map<string, number>();
const defaultRemarkPlugins: PluggableList = [remarkGfm];
const LOCAL_ENTITY_LEADING_BOUNDARY = String.raw`(^|[^A-Za-z0-9_/@.-])`;
const LOCAL_ENTITY_TRAILING_BOUNDARY = String.raw`(?=$|[^A-Za-z0-9_-])`;
const agentMentionPattern = new RegExp(
  `${LOCAL_ENTITY_LEADING_BOUNDARY}@([A-Za-z][A-Za-z0-9_-]{1,31})${LOCAL_ENTITY_TRAILING_BOUNDARY}`, "g",
);
const channelMentionPattern = new RegExp(
  `${LOCAL_ENTITY_LEADING_BOUNDARY}#([A-Za-z][A-Za-z0-9_-]{1,63})${LOCAL_ENTITY_TRAILING_BOUNDARY}`, "g",
);
const taskMentionPattern = new RegExp(
  `${LOCAL_ENTITY_LEADING_BOUNDARY}task #([0-9]+)${LOCAL_ENTITY_TRAILING_BOUNDARY}`, "gi",
);

function encodeLocalPath(value: string) {
  return encodeURIComponent(value.replace(/^[@#]/, ""));
}

function linkifyPlainText(value: string) {
  return value
    .replace(agentMentionPattern, (_match, prefix, handle) => (
      `${prefix}[@${handle}](${LOCAL_ENTITY_PATH_PREFIX}agent/${encodeLocalPath(handle)})`
    ))
    .replace(channelMentionPattern, (_match, prefix, channel) => (
      `${prefix}[#${channel}](${LOCAL_ENTITY_PATH_PREFIX}channel/${encodeLocalPath(channel)})`
    ))
    .replace(taskMentionPattern, (_match, prefix, taskNumber) => (
      `${prefix}[task #${taskNumber}](${LOCAL_ENTITY_PATH_PREFIX}task/${taskNumber})`
    ));
}

function linkifyMessageBody(body: string) {
  return body
    .split(FENCE_SPLIT)
    .map((segment) => {
      if (segment.startsWith("```") || segment.startsWith("\n```")) return segment;
      return segment
        .split(INLINE_CODE_SPLIT)
        .map((inlineSegment) => inlineSegment.startsWith("`") ? inlineSegment : linkifyPlainText(inlineSegment))
        .join("");
    })
    .join("");
}

function messageReferenceMarkdown(body: string) {
  return body.replace(MESSAGE_REFERENCE_PATTERN, (_match, kind: string, id: string) => (
    `[${kind}:${id}](${LOCAL_ENTITY_PATH_PREFIX}reference/${kind}/${id})`
  ));
}

function textFromNode(node: ReactNode): string {
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(textFromNode).join("");
  if (isValidElement<{ children?: ReactNode }>(node)) return textFromNode(node.props.children);
  return "";
}

function CopyableCodeBlock({ children }: { children?: ReactNode }) {
  const [copied, setCopied] = useState(false);
  const text = textFromNode(children).replace(/\n$/, "");

  async function handleCopy() {
    await copyText(text);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1200);
  }

  return (
    <div className="code-block-shell">
      <button type="button" onClick={handleCopy} aria-label="Copy code block">
        {copied ? "Copied" : "Copy"}
      </button>
      <pre>{children}</pre>
    </div>
  );
}

function isolateLinkEvent(event: MouseEvent<HTMLAnchorElement> | PointerEvent<HTMLAnchorElement>) {
  event.stopPropagation();
}

function localAgentHandleFromHref(href: string | undefined) {
  if (!href?.startsWith(`${LOCAL_ENTITY_PATH_PREFIX}agent/`)) return null;
  const encodedHandle = href.slice(`${LOCAL_ENTITY_PATH_PREFIX}agent/`.length).split(/[?#]/, 1)[0];
  try {
    return decodeURIComponent(encodedHandle).replace(/^@/, "");
  } catch {
    return null;
  }
}

function referenceFromHref(
  href: string | undefined,
  messages: Message[] | undefined,
  channels: Channel[] | undefined,
  references: MessageMarkdownProps["references"],
) {
  if (!href?.startsWith(`${LOCAL_ENTITY_PATH_PREFIX}reference/`)) return null;
  const [, kind, id] = href.match(/^\/lantor\/reference\/(message|thread)\/([0-9a-fA-F-]{8,36})/) ?? [];
  if (kind !== "message" && kind !== "thread") return null;
  if (references) return references[`${kind}:${id}`] ?? null;
  if (!messages || !channels) return null;
  return resolveMessageReference({ kind, id, token: `[[${kind}:${id}]]` }, messages, channels);
}

function handleLinkClick(
  event: MouseEvent<HTMLAnchorElement>,
  href: string | undefined,
  isLocalLink: boolean,
  onLocalAgentLink: ((handle: string) => void) | undefined,
) {
  event.preventDefault();
  event.stopPropagation();
  if (isLocalLink) {
    const agentHandle = localAgentHandleFromHref(href);
    if (agentHandle && event.detail <= 1) onLocalAgentLink?.(agentHandle);
    return;
  }
  if (!href || event.detail > 1) return;

  void openExternalUrl(href).catch((err) => {
    console.error("Failed to open external link", err);
  });
}

function transformMessageUrl(url: string) {
  return /^file:\/\//i.test(url) ? url : defaultUrlTransform(url);
}

function MarkdownTableScroll({ children, scrollKey }: { children?: ReactNode; scrollKey?: string }) {
  const scrollRef = useRef<HTMLDivElement | null>(null);

  useLayoutEffect(() => {
    if (!scrollKey) return;
    const element = scrollRef.current;
    if (!element) return;
    const storedScrollLeft = tableScrollPositions.get(scrollKey);
    if (storedScrollLeft === undefined || storedScrollLeft === 0) return;
    if (element.scrollLeft !== 0) return;
    const maxScrollLeft = element.scrollWidth - element.clientWidth;
    if (maxScrollLeft <= 0) return;
    element.scrollLeft = Math.min(storedScrollLeft, maxScrollLeft);
  }, [scrollKey]);

  function handleScroll(event: UIEvent<HTMLDivElement>) {
    if (!scrollKey) return;
    tableScrollPositions.set(scrollKey, event.currentTarget.scrollLeft);
  }

  return (
    <div
      ref={scrollRef}
      className="markdown-table-scroll"
      role="region"
      tabIndex={0}
      aria-label="Scrollable table"
      onScroll={handleScroll}
    >
      <table>{children}</table>
    </div>
  );
}

export function MarkdownRenderer({
  body,
  references,
  onLocalAgentLink,
  messages,
  channels,
  sourceMessageId,
  onOpenReference,
  scrollKey,
  enableLantorLinks = true,
  remarkPlugins = defaultRemarkPlugins,
  rehypePlugins,
}: MessageMarkdownProps & { remarkPlugins?: PluggableList; rehypePlugins?: PluggableList }) {
  const linkedBody = useMemo(
    () => enableLantorLinks ? messageReferenceMarkdown(linkifyMessageBody(body)) : body,
    [body, enableLantorLinks],
  );
  const tableIndexRef = useRef(0);
  tableIndexRef.current = 0;
  const handleOpenReference = useCallback((reference: ResolvedMessageReference) => {
    if (!sourceMessageId) return;
    onOpenReference?.(sourceMessageId, reference);
  }, [onOpenReference, sourceMessageId]);
  const markdownComponents = useMemo<Components>(() => ({
    a: ({ children, href, node: _node, ...props }) => {
      const reference = referenceFromHref(href, messages, channels, references);
      if (reference) {
        return (
          <MessageReferenceCard
            reference={reference}
            compact
            onOpen={sourceMessageId && onOpenReference ? handleOpenReference : undefined}
          />
        );
      }
      const isLocalLink = Boolean(href?.startsWith(LOCAL_ENTITY_PATH_PREFIX));
      return (
        <a
          {...props}
          href={href}
          className={isLocalLink ? "local-entity-link" : undefined}
          target={isLocalLink ? undefined : "_blank"}
          rel={isLocalLink ? undefined : "noreferrer"}
          onPointerDown={isolateLinkEvent}
          onContextMenu={isolateLinkEvent}
          onClick={(event) => handleLinkClick(event, href, isLocalLink, onLocalAgentLink)}
        >
          {children}
        </a>
      );
    },
    pre: ({ children }) => (
      <CopyableCodeBlock>{Children.toArray(children)}</CopyableCodeBlock>
    ),
    table: ({ children }) => {
      const tableScrollKey = scrollKey ? `${scrollKey}:table:${tableIndexRef.current}` : undefined;
      tableIndexRef.current += 1;
      return <MarkdownTableScroll scrollKey={tableScrollKey}>{children}</MarkdownTableScroll>;
    },
  }), [references, channels, handleOpenReference, messages, onLocalAgentLink, onOpenReference, scrollKey, sourceMessageId]);

  return (
    <div className="markdown-body">
      <ReactMarkdown
        remarkPlugins={remarkPlugins}
        rehypePlugins={rehypePlugins}
        urlTransform={transformMessageUrl}
        components={markdownComponents}
      >
        {linkedBody}
      </ReactMarkdown>
    </div>
  );
}
