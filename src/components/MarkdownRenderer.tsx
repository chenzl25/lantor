import { reportUiError } from "../ui-notice";
import {
  ReactNode,
  type MouseEvent,
  type PointerEvent,
  type UIEvent,
  useCallback,
  useLayoutEffect,
  useMemo,
  useRef,
  useSyncExternalStore,
} from "react";
import ReactMarkdown, { defaultUrlTransform, type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import type { PluggableList } from "unified";
import { attachmentAssetUrl, isTauriRuntime, openExternalUrl } from "../apiClient";
import { openAttachmentSheet, usesAttachmentSheet } from "../attachment-sheet";
import {
  MESSAGE_REFERENCE_PATTERN,
  type ResolvedMessageReference,
  resolveMessageReference,
} from "../message-references";
import type { Channel, Message, MessageAttachment } from "../types";
import type { MessageReferenceStore } from "../message-reference-store";
import { StreamingReferenceCard } from "./StreamingReferenceCard";
import { MessageReferenceCard } from "./MessageReferenceCard";
import { CodeBlock } from "./CodeBlock";
import { GithubReferenceLink } from "./GithubReferenceLink";
import {
  getGithubReferenceIndex,
  githubReferenceKey,
  githubReferenceUrl,
  parseGithubIssueUrl,
  resolveGithubRepository,
  subscribeGithubReferences,
} from "../github-references";
import { REFERENCE_TITLE_PROPERTY, remarkInlineEnrichment } from "../markdown-inline";

export type MessageMarkdownProps = {
  body: string;
  streamingReferenceStore?: MessageReferenceStore;
  references?: Record<string, ResolvedMessageReference>;
  onLocalAgentLink?: (handle: string) => void;
  messages?: Message[];
  channels?: Channel[];
  sourceMessageId?: string;
  onOpenReference?: (sourceMessageId: string, reference: ResolvedMessageReference) => void;
  scrollKey?: string;
  enableLantorLinks?: boolean;
  /** Attachments of the message; local file links resolve to their snapshots. */
  attachments?: MessageAttachment[];
  /** The message is still streaming: code blocks defer highlighting and collapsing. */
  streaming?: boolean;
  /** Channel whose bound GitHub repository resolves bare `#123` references. */
  channelId?: string;
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

type HastNode = {
  type: string;
  tagName?: string;
  value?: string;
  properties?: { className?: unknown };
  data?: { meta?: unknown };
  children?: HastNode[];
};

function hastText(node: HastNode): string {
  if (node.type === "text") return node.value ?? "";
  return node.children?.map(hastText).join("") ?? "";
}

function codeFence(pre: HastNode | undefined) {
  const code = pre?.children?.find((child) => child.type === "element" && child.tagName === "code");
  if (!code) return null;
  const classes = Array.isArray(code.properties?.className) ? code.properties.className : [];
  const languageClass = classes.find((name): name is string => typeof name === "string" && name.startsWith("language-"));
  return {
    // mdast-util-to-hast appends the newline that ends the fence body.
    code: hastText(code).replace(/\n$/, ""),
    lang: languageClass?.slice("language-".length) ?? null,
    meta: typeof code.data?.meta === "string" ? code.data.meta : null,
  };
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

// Decodes a local file href the same way the backend records `source_path`.
function localPathFromHref(href: string, stripPosition = false) {
  let path = href.replace(/^file:\/\/(?:localhost)?/i, "");
  if (!(path.startsWith("/") && !path.startsWith("//")) && !path.startsWith("~/")) return null;
  if (stripPosition) {
    const suffix = path.match(/(?::(\d+)(?::(\d+))?|#L(\d+)(?:-L?(\d+))?)$/);
    if (suffix && suffix.slice(1).filter(Boolean).every((value) => Number(value) > 0 && Number(value) <= 0xffffffff)) {
      path = path.slice(0, suffix.index);
    }
  }
  try {
    return decodeURIComponent(path);
  } catch {
    return path;
  }
}

function linkedAttachment(href: string | undefined, attachments: MessageAttachment[] | undefined) {
  if (!href || !attachments?.length) return null;
  const path = localPathFromHref(href);
  if (!path) return null;
  // Preserve exact matches (including literal encoded filename punctuation),
  // then resolve editor-style positions through the underlying file snapshot.
  return attachments.find((attachment) => attachment.source_path === path)
    ?? attachments.find((attachment) => attachment.source_path === localPathFromHref(href, true))
    ?? null;
}

async function openLink(href: string, attachment: MessageAttachment | null) {
  try {
    await openExternalUrl(href);
  } catch (error) {
    // The desktop app opens the original file; once that is gone, open the snapshot.
    if (!attachment || !isTauriRuntime()) throw error;
    await openExternalUrl(attachment.storage_path);
  }
}

function handleLinkClick(
  event: MouseEvent<HTMLAnchorElement>,
  href: string | undefined,
  isLocalLink: boolean,
  onLocalAgentLink: ((handle: string) => void) | undefined,
  attachment: MessageAttachment | null,
) {
  event.preventDefault();
  event.stopPropagation();
  if (isLocalLink) {
    const agentHandle = localAgentHandleFromHref(href);
    if (agentHandle && event.detail <= 1) onLocalAgentLink?.(agentHandle);
    return;
  }
  if (!href || event.detail > 1) return;
  if (attachment && usesAttachmentSheet()) {
    openAttachmentSheet(attachment);
    return;
  }

  void openLink(href, attachment).catch((err) => {
    console.error("Failed to open external link", err);
    reportUiError("Could not open link", err);
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
  streamingReferenceStore,
  onLocalAgentLink,
  messages,
  channels,
  sourceMessageId,
  onOpenReference,
  scrollKey,
  enableLantorLinks = true,
  attachments,
  streaming = false,
  remarkPlugins = defaultRemarkPlugins,
  rehypePlugins,
  channelId,
}: MessageMarkdownProps & { remarkPlugins?: PluggableList; rehypePlugins?: PluggableList }) {
  const linkedBody = useMemo(
    () => enableLantorLinks ? messageReferenceMarkdown(linkifyMessageBody(body)) : body,
    [body, enableLantorLinks],
  );
  const githubReferences = useSyncExternalStore(subscribeGithubReferences, getGithubReferenceIndex, getGithubReferenceIndex);
  const enrichedRemarkPlugins = useMemo<PluggableList>(() => [
    ...remarkPlugins,
    [remarkInlineEnrichment, {
      resolveGithubReference: (prefix: string | null, number: number) => {
        const repository = resolveGithubRepository(githubReferences, prefix, channelId);
        return repository ? githubReferenceUrl(githubReferences, repository, number) : null;
      },
      githubReferenceKey: (url: string) => {
        const link = parseGithubIssueUrl(url);
        return link ? githubReferenceKey(link) : null;
      },
    }],
  ], [channelId, githubReferences, remarkPlugins]);
  const tableIndexRef = useRef(0);
  tableIndexRef.current = 0;
  const codeIndexRef = useRef(0);
  codeIndexRef.current = 0;
  const handleOpenReference = useCallback((reference: ResolvedMessageReference) => {
    if (!sourceMessageId) return;
    onOpenReference?.(sourceMessageId, reference);
  }, [onOpenReference, sourceMessageId]);
  const markdownComponents = useMemo<Components>(() => ({
    a: ({ children, href, node, ...props }) => {
      const [, kind, id] = href?.match(/^\/lantor\/reference\/(message|thread)\/([0-9a-fA-F-]{8,36})/) ?? [];
      if (streamingReferenceStore && (kind === "message" || kind === "thread")) {
        return <StreamingReferenceCard store={streamingReferenceStore} reference={{ kind, id, token: `[[${kind}:${id}]]` }}
          onOpen={sourceMessageId && onOpenReference ? handleOpenReference : undefined} />;
      }
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
      const githubLink = parseGithubIssueUrl(href);
      if (githubLink && href) {
        // A bare URL autolinks to itself; show it as `rw#123` instead.
        const bareUrl = node ? hastText(node as HastNode) === href : false;
        const { [REFERENCE_TITLE_PROPERTY]: referenceTitle, ...linkProps } = props as typeof props & Record<string, unknown>;
        return (
          <GithubReferenceLink
            {...linkProps}
            href={href}
            link={githubLink}
            references={githubReferences}
            compactLabel={bareUrl}
            showTitle={referenceTitle !== undefined}
            onPointerDown={isolateLinkEvent}
            onContextMenu={isolateLinkEvent}
            // The link may point at the resolved /pull/ page instead of `href`.
            onClick={(event) => handleLinkClick(event, event.currentTarget.getAttribute("href") ?? href, false, onLocalAgentLink, null)}
          >
            {children}
          </GithubReferenceLink>
        );
      }
      const isLocalLink = Boolean(href?.startsWith(LOCAL_ENTITY_PATH_PREFIX));
      const attachment = isLocalLink ? null : linkedAttachment(href, attachments);
      // Browsers cannot reach host paths, so they get the snapshot instead.
      const target = attachment && !isTauriRuntime()
        ? attachmentAssetUrl(attachment.storage_path, attachment.id)
        : href;
      return (
        <a
          {...props}
          href={target}
          className={isLocalLink ? "local-entity-link" : undefined}
          target={isLocalLink ? undefined : "_blank"}
          rel={isLocalLink ? undefined : "noreferrer"}
          onPointerDown={isolateLinkEvent}
          onContextMenu={isolateLinkEvent}
          onClick={(event) => handleLinkClick(event, target, isLocalLink, onLocalAgentLink, attachment)}
        >
          {children}
        </a>
      );
    },
    pre: ({ children, node }) => {
      const fence = codeFence(node as HastNode | undefined);
      if (!fence) return <pre>{children}</pre>;
      const stateKey = scrollKey ? `${scrollKey}:code:${codeIndexRef.current}` : undefined;
      codeIndexRef.current += 1;
      return <CodeBlock {...fence} streaming={streaming} stateKey={stateKey} />;
    },
    table: ({ children }) => {
      const tableScrollKey = scrollKey ? `${scrollKey}:table:${tableIndexRef.current}` : undefined;
      tableIndexRef.current += 1;
      return <MarkdownTableScroll scrollKey={tableScrollKey}>{children}</MarkdownTableScroll>;
    },
  }), [streamingReferenceStore, references, channels, handleOpenReference, messages, onLocalAgentLink, onOpenReference, scrollKey, sourceMessageId, attachments, streaming, githubReferences]);

  return (
    <div className="markdown-body">
      <ReactMarkdown
        remarkPlugins={enrichedRemarkPlugins}
        rehypePlugins={rehypePlugins}
        urlTransform={transformMessageUrl}
        components={markdownComponents}
      >
        {linkedBody}
      </ReactMarkdown>
    </div>
  );
}
