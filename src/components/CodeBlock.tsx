import { type CSSProperties, type MouseEvent, type ReactNode, useEffect, useMemo, useRef, useState } from "react";
import { openExternalUrl, opensLinksNatively } from "../apiClient";
import { copyText } from "../clipboard";
import {
  CODE_BLOCK_COLLAPSE_LINES,
  CODE_BLOCK_EXPAND_EVENT,
  type DiffLine,
  type HighlightLanguage,
  MAX_HIGHLIGHT_CHARS,
  parseCodeFence,
  parseDiffLines,
} from "../code-blocks";
import type { ThemedToken } from "../code-highlighter";
import { reportUiError } from "../ui-notice";

type HighlighterModule = typeof import("../code-highlighter");

let highlighterModule: HighlighterModule | null = null;
let highlighterImport: Promise<HighlighterModule> | null = null;
let warnedUnavailable = false;
const TOKEN_CACHE_LIMIT = 200;
// Rows remount while scrolling; cached tokens make re-entry paint highlighted.
const tokenCache = new Map<string, ThemedToken[][]>();
const failedTokenKeys = new Set<string>();
const expandedBlocks = new Set<string>();

function loadHighlighting(language: HighlightLanguage) {
  highlighterImport ??= import("../code-highlighter").then((module) => (highlighterModule = module));
  return highlighterImport.then((module) => module.loadHighlightLanguage(language));
}

function cachedTokens(code: string, language: HighlightLanguage) {
  const key = `${language}\u0000${code}`;
  const cached = tokenCache.get(key);
  if (!cached) return null;
  tokenCache.delete(key);
  tokenCache.set(key, cached);
  return cached;
}

function tokenize(code: string, language: HighlightLanguage) {
  const key = `${language}\u0000${code}`;
  if (!highlighterModule || tokenCache.has(key) || failedTokenKeys.has(key)) return;
  let tokens: ThemedToken[][] | null;
  try {
    tokens = highlighterModule.tokenizeLoaded(code, language);
  } catch (error) {
    if (failedTokenKeys.size >= TOKEN_CACHE_LIMIT) failedTokenKeys.clear();
    failedTokenKeys.add(key);
    console.warn("Code block highlighting failed", error);
    return;
  }
  if (!tokens) return;
  tokenCache.set(key, tokens);
  if (tokenCache.size > TOKEN_CACHE_LIMIT) tokenCache.delete(tokenCache.keys().next().value!);
}

// Tokenizing runs after the frame that shows the blocks, a few milliseconds at
// a time. Opening a thread renders every reply at once, and tokenizing their
// code blocks inside that render was about a third of its script time, mostly
// for blocks above the screen. The newest blocks go first: a thread or channel
// opens at its latest message.
const TOKENIZE_SLICE_MS = 8;
const pendingTokenizing: Array<() => void> = [];
let tokenizingScheduled = false;

function scheduleTokenizing(task: () => void) {
  pendingTokenizing.push(task);
  if (tokenizingScheduled) return;
  tokenizingScheduled = true;
  requestAnimationFrame(() => setTimeout(drainTokenizing, 0));
}

function drainTokenizing() {
  const deadline = performance.now() + TOKENIZE_SLICE_MS;
  while (pendingTokenizing.length && performance.now() < deadline) pendingTokenizing.pop()!();
  if (pendingTokenizing.length) requestAnimationFrame(() => setTimeout(drainTokenizing, 0));
  else tokenizingScheduled = false;
}

/**
 * Highlighted rows, or null until they are tokenized (the block stays plain):
 * while the grammar loads, and until the first frame showing the block has
 * been drawn. Tokens cached earlier apply at once.
 */
function useSyntaxTokens(code: string, language: HighlightLanguage | null, enabled: boolean) {
  const active = enabled && language !== null && code.length <= MAX_HIGHLIGHT_CHARS;
  const [loads, setLoads] = useState(0);
  const tokens = useMemo(
    () => (active && language ? cachedTokens(code, language) : null),
    // `loads` re-reads the cache once the block has been tokenized.
    [active, code, language, loads],
  );
  const missing = active && tokens === null;
  useEffect(() => {
    if (!missing || !language) return;
    let cancelled = false;
    loadHighlighting(language).then(
      () => {
        if (cancelled) return;
        scheduleTokenizing(() => {
          if (cancelled) return;
          tokenize(code, language);
          setLoads((count) => count + 1);
        });
      },
      (error) => {
        // A failed chunk keeps every block readable as plain text until reload.
        if (!warnedUnavailable) console.warn("Code block highlighting unavailable", error);
        warnedUnavailable = true;
      },
    );
    return () => { cancelled = true; };
  }, [missing, language, code]);
  return tokens;
}

function tokenStyle(token: ThemedToken): CSSProperties | undefined {
  if (!token.color && !token.fontStyle) return undefined;
  const style: CSSProperties = { color: token.color };
  const fontStyle = token.fontStyle ?? 0;
  if (fontStyle & 1) style.fontStyle = "italic";
  if (fontStyle & 2) style.fontWeight = "bold";
  if (fontStyle & 4) style.textDecoration = "underline";
  return style;
}

function lineContent(tokens: ThemedToken[] | undefined, text: string): ReactNode {
  if (!tokens) return text;
  return tokens.map((token, index) => <span key={index} style={tokenStyle(token)}>{token.content}</span>);
}

function lineCount(code: string) {
  let lines = 1;
  for (let index = 0; index < code.length; index += 1) if (code.charCodeAt(index) === 10) lines += 1;
  return lines;
}

// Rows are inline(-block) with real newlines between them, so selection and
// innerText reproduce the source exactly, including blank lines.
function rows(lines: ReactNode[]) {
  return lines.flatMap((line, index) => (index === 0 ? [line] : ["\n", line]));
}

function isDiffBody(line: DiffLine) {
  return line.kind === "add" || line.kind === "del" || line.kind === "context";
}

function CopyButton({ code, className }: { code: string; className: string }) {
  const [copied, setCopied] = useState(false);
  async function handleCopy() {
    await copyText(code);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1200);
  }
  return (
    <button type="button" className={className} onClick={handleCopy} aria-label="Copy code">
      {copied ? "Copied" : "Copy"}
    </button>
  );
}

function openHeaderLink(event: MouseEvent<HTMLAnchorElement>, href: string) {
  event.stopPropagation();
  if (opensLinksNatively()) return;
  event.preventDefault();
  void openExternalUrl(href).catch((error) => reportUiError("Couldn’t open link", error));
}

export type CodeBlockProps = {
  code: string;
  lang?: string | null;
  meta?: string | null;
  /** Streaming blocks skip highlighting and collapsing until complete. */
  streaming?: boolean;
  /** Keeps a block expanded when its row remounts. */
  stateKey?: string;
};

export function CodeBlock({ code, lang, meta, streaming = false, stateKey }: CodeBlockProps) {
  const fence = useMemo(() => parseCodeFence(lang, meta), [lang, meta]);
  const diff = useMemo(() => (fence.diff ? parseDiffLines(code) : null), [code, fence.diff]);
  const highlightSource = useMemo(
    () => (diff ? diff.lines.filter(isDiffBody).map((line) => line.content).join("\n") : code),
    [code, diff],
  );
  const tokens = useSyntaxTokens(highlightSource, fence.language, !streaming);
  const [expandedState, setExpandedState] = useState(() => (stateKey ? expandedBlocks.has(stateKey) : false));
  const lines = diff ? diff.lines.length : lineCount(code);
  const collapsible = !streaming && lines > CODE_BLOCK_COLLAPSE_LINES;
  const collapsed = collapsible && !expandedState;
  const startLine = diff ? null : fence.startLine;
  const numbered = diff ? diff.numbered : startLine !== null;

  function setExpanded(next: boolean) {
    setExpandedState(next);
    if (!stateKey) return;
    if (next) expandedBlocks.add(stateKey);
    else expandedBlocks.delete(stateKey);
  }
  const setExpandedRef = useRef(setExpanded);
  setExpandedRef.current = setExpanded;
  const shellRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const shell = shellRef.current;
    if (!shell) return;
    const expand = () => setExpandedRef.current(true);
    shell.addEventListener(CODE_BLOCK_EXPAND_EVENT, expand);
    return () => shell.removeEventListener(CODE_BLOCK_EXPAND_EVENT, expand);
  }, []);

  let body: ReactNode;
  let gutterDigits = 0;
  if (diff) {
    let tokenRow = 0;
    for (const line of diff.lines) {
      gutterDigits = Math.max(gutterDigits, String(line.oldLine ?? "").length, String(line.newLine ?? "").length);
    }
    body = rows(diff.lines.map((line, index) => {
      const rowTokens = isDiffBody(line) ? tokens?.[tokenRow++] : undefined;
      return (
        <span key={index} className={`code-line diff-${line.kind}`}>
          {numbered && <span className="code-gutter code-gutter-diff" aria-hidden="true"
            data-old={line.oldLine ?? ""} data-new={line.newLine ?? ""} />}
          {isDiffBody(line) && <span className="code-diff-marker">{line.marker}</span>}
          {isDiffBody(line) ? lineContent(rowTokens, line.content) : line.content}
        </span>
      );
    }));
  } else if (tokens || numbered) {
    const sourceLines = code.split("\n");
    if (startLine !== null) gutterDigits = String(startLine + sourceLines.length - 1).length;
    body = rows(sourceLines.map((text, index) => (
      <span key={index} className="code-line">
        {startLine !== null && <span className="code-gutter" aria-hidden="true" data-line={startLine + index} />}
        {lineContent(tokens?.[index], text)}
      </span>
    )));
  } else {
    body = code;
  }

  const range = startLine !== null
    ? `L${startLine}${fence.endLine && fence.endLine > startLine ? `-L${fence.endLine}` : ""}`
    : null;
  const shellClass = [
    "code-block-shell",
    fence.title ? "has-header" : "",
    collapsible ? "expandable" : "",
    collapsed ? "collapsed" : "",
  ].filter(Boolean).join(" ");
  const preClass = [
    diff ? "is-diff" : "",
    numbered ? "has-gutter" : "",
    tokens ? "is-highlighted" : "",
  ].filter(Boolean).join(" ") || undefined;
  const preStyle = numbered ? { "--code-gutter-digits": String(Math.max(gutterDigits, 1)) } as CSSProperties : undefined;

  return (
    <div ref={shellRef} className={shellClass} data-language={fence.language ?? undefined}>
      {fence.title ? (
        <div className="code-block-header">
          <span className="code-block-location">
            {fence.href
              ? <a className="code-block-title" href={fence.href} title={fence.href} target="_blank" rel="noreferrer"
                onClick={(event) => openHeaderLink(event, fence.href!)}>{fence.title}</a>
              : <span className="code-block-title" title={fence.title}>{fence.title}</span>}
            {range && <span className="code-block-range">{range}</span>}
          </span>
          {fence.label && <span className="code-block-language">{fence.label}</span>}
          <CopyButton code={code} className="code-block-copy" />
        </div>
      ) : <CopyButton code={code} className="code-block-copy floating" />}
      <pre className={preClass} style={preStyle}><code>{body}</code></pre>
      {collapsible && (
        <button type="button" className="code-block-expand" aria-expanded={!collapsed} onClick={() => setExpanded(!expandedState)}>
          {collapsed ? `Show all ${lines} lines` : "Show less"}
        </button>
      )}
    </div>
  );
}
