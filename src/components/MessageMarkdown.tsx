import {
  Children,
  ReactNode,
  isValidElement,
  type MouseEvent,
  type PointerEvent,
  type UIEvent,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import ReactMarkdown, { defaultUrlTransform, type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import { openExternalUrl } from "../apiClient";
import { copyText } from "../clipboard";

type MessageMarkdownProps = {
  body: string;
  onLocalAgentLink?: (handle: string) => void;
  scrollKey?: string;
};

const INLINE_CODE_SPLIT = /(`[^`\n]*(?:`|$))/g;
const FENCE_SPLIT = /(```[\s\S]*?(?:```|$))/g;
const LOCAL_ENTITY_PATH_PREFIX = "/lantor/";
const tableScrollPositions = new Map<string, number>();

function encodeLocalPath(value: string) {
  return encodeURIComponent(value.replace(/^[@#]/, ""));
}

function linkifyPlainText(value: string) {
  return value
    .replace(/(^|[\s([{])@([A-Za-z][A-Za-z0-9_-]{1,31})(?=$|[\s.,;:!?)\]}])/g, (_match, prefix, handle) => (
      `${prefix}[@${handle}](${LOCAL_ENTITY_PATH_PREFIX}agent/${encodeLocalPath(handle)})`
    ))
    .replace(/(^|[\s([{])#([A-Za-z][A-Za-z0-9_-]{1,63})(?=$|[\s.,;:!?)\]}])/g, (_match, prefix, channel) => (
      `${prefix}[#${channel}](${LOCAL_ENTITY_PATH_PREFIX}channel/${encodeLocalPath(channel)})`
    ))
    .replace(/(^|[\s([{])task #([0-9]+)(?=$|[\s.,;:!?)\]}])/gi, (_match, prefix, taskNumber) => (
      `${prefix}[task #${taskNumber}](${LOCAL_ENTITY_PATH_PREFIX}task/${taskNumber})`
    ));
}

function linkifyMessageBody(body: string) {
  return body
    .split(FENCE_SPLIT)
    .map((segment) => {
      if (segment.startsWith("```")) return segment;
      return segment
        .split(INLINE_CODE_SPLIT)
        .map((inlineSegment) => inlineSegment.startsWith("`") ? inlineSegment : linkifyPlainText(inlineSegment))
        .join("");
    })
    .join("");
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

export function MessageMarkdown({ body, onLocalAgentLink, scrollKey }: MessageMarkdownProps) {
  const linkedBody = linkifyMessageBody(body);
  const tableIndexRef = useRef(0);
  tableIndexRef.current = 0;
  const markdownComponents = useMemo<Components>(() => ({
    a: ({ children, href, node: _node, ...props }) => {
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
  }), [onLocalAgentLink, scrollKey]);

  return (
    <div className="markdown-body">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        urlTransform={transformMessageUrl}
        components={markdownComponents}
      >
        {linkedBody}
      </ReactMarkdown>
    </div>
  );
}
