import { useEffect, useLayoutEffect, useRef, useState, type RefObject } from "react";
import {
  findNeedle,
  findOffsets,
  findPosition,
  findTotal,
  foldFindText,
  mergeFindEntries,
  reconcileCursor,
  stepFind,
  type FindCursor,
  type FindEntry,
} from "../thread-find";
import type { ThreadFindHit } from "../types";
import { CODE_BLOCK_EXPAND_EVENT } from "../code-blocks";

// The thread find bar (Cmd+F): counts matches in the rendered thread, asks
// the server about replies that are not loaded, and moves between matches.
// See docs/bootstrap-payload.md#finding-in-a-thread.

const MATCH_HIGHLIGHT = "lantor-thread-find";
const CURRENT_HIGHLIGHT = "lantor-thread-find-current";
/** Message text that find searches; MessageRow marks the body with it. */
const FIND_SCOPE = "[data-find-text]";
/** Text inside a find scope that is not message text. */
const FIND_SKIP = "button, .katex-mathml, [data-find-skip]";
const UNLOADED_FIND_DELAY_MS = 180;
const RECOUNT_DELAY_MS = 120;
const RENDER_WAIT_FRAMES = 60;
const SCROLL_SETTLE_FRAMES = 12;

type ThreadFindOptions = {
  threadId: string | null;
  /** The thread's scrolling viewport and the content inside it. */
  viewportRef: RefObject<HTMLElement | null>;
  contentRef: RefObject<HTMLElement | null>;
  /** Seq of the oldest loaded reply; null once every reply is loaded, undefined before the first page. */
  loadedReplyStart: number | null | undefined;
  findUnloaded: (threadId: string, query: string, beforeSeq: number) => Promise<ThreadFindHit[]>;
  /** Loads the replies from `seq` on, so that reply renders. */
  revealReply: (threadId: string, seq: number) => Promise<void>;
  /** Expands a collapsed long message. */
  expandMessage: (messageId: string) => void;
  /** Scrolls the thread so the match is in view; returns false when it had to scroll. */
  scrollToMatch: (range: Range) => boolean;
};

export type ThreadFind = ReturnType<typeof useThreadFind>;

type Matches = { entries: FindEntry[]; ranges: Map<string, Range[]> };

const NO_MATCHES: Matches = { entries: [], ranges: new Map() };

export function useThreadFind({
  threadId, viewportRef, contentRef, loadedReplyStart, findUnloaded, revealReply, expandMessage, scrollToMatch,
}: ThreadFindOptions) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [entries, setEntries] = useState<FindEntry[]>([]);
  const [cursor, setCursor] = useState<FindCursor | null>(null);
  const [searching, setSearching] = useState(false);
  const [moving, setMoving] = useState(false);
  const [focusRequest, setFocusRequest] = useState(0);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const matchesRef = useRef<Matches>(NO_MATCHES);
  const cursorRef = useRef<FindCursor | null>(null);
  const hitsRef = useRef<{ key: string; hits: ThreadFindHit[] }>({ key: "", hits: [] });
  /** Unloaded hits that did not render with a match once loaded. */
  const skippedRef = useRef(new Set<string>());
  const loadedStartRef = useRef(loadedReplyStart);
  loadedStartRef.current = loadedReplyStart;
  const optionsRef = useRef({ threadId, contentRef, findUnloaded, revealReply, expandMessage, scrollToMatch });
  optionsRef.current = { threadId, contentRef, findUnloaded, revealReply, expandMessage, scrollToMatch };
  const needle = open ? findNeedle(query) : "";
  const needleRef = useRef(needle);
  needleRef.current = needle;
  const searchKey = threadId && needle ? `${threadId}\n${needle}` : "";
  const moveRef = useRef(0);

  function updateCursor(next: FindCursor | null) {
    cursorRef.current = next;
    setCursor(next);
    paintCurrent();
  }

  /** Counts the matches again from the page and the server's hits. */
  function recount() {
    const content = optionsRef.current.contentRef.current;
    const currentNeedle = needleRef.current;
    if (!content || !currentNeedle) {
      matchesRef.current = NO_MATCHES;
      setEntries([]);
      clearHighlights(content);
      return NO_MATCHES;
    }
    const { rendered, ranges } = renderedMatches(content, currentNeedle);
    const start = loadedStartRef.current;
    const hits = hitsRef.current.key === searchKeyRef.current && typeof start === "number"
      ? hitsRef.current.hits.filter((hit) => hit.seq < start && !skippedRef.current.has(hit.message_id))
      : [];
    const previous = matchesRef.current.entries;
    const merged = mergeFindEntries(rendered, hits);
    const next = { entries: sameEntries(previous, merged) ? previous : merged, ranges };
    matchesRef.current = next;
    setEntries(next.entries);
    paintMatches(ranges);
    const reconciled = reconcileCursor(next.entries, cursorRef.current, previous);
    const current = cursorRef.current;
    cursorRef.current = current && reconciled && current.messageId === reconciled.messageId && current.index === reconciled.index
      ? current
      : reconciled;
    setCursor(cursorRef.current);
    paintCurrent();
    return next;
  }
  const searchKeyRef = useRef(searchKey);
  searchKeyRef.current = searchKey;
  const recountRef = useRef(recount);
  recountRef.current = recount;

  function paintCurrent() {
    const content = optionsRef.current.contentRef.current;
    for (const element of content?.querySelectorAll<HTMLElement>("[data-find-current]") ?? []) {
      if (element.dataset.messageId !== cursorRef.current?.messageId) delete element.dataset.findCurrent;
    }
    const range = currentRange();
    if (cursorRef.current && content) {
      const article = content.querySelector<HTMLElement>(`article[data-message-id="${cursorRef.current.messageId}"]`);
      // A row outside the viewport skips rendering; the current match needs real geometry.
      if (article) article.dataset.findCurrent = "true";
    }
    if (!highlightsSupported()) return;
    if (range) {
      const highlight = new Highlight(range);
      highlight.priority = 1;
      CSS.highlights.set(CURRENT_HIGHLIGHT, highlight);
    } else {
      CSS.highlights.delete(CURRENT_HIGHLIGHT);
    }
  }

  function currentRange() {
    const current = cursorRef.current;
    return current ? matchesRef.current.ranges.get(current.messageId)?.[current.index] ?? null : null;
  }

  /** Opens collapsed blocks around the current match, then scrolls to it. */
  async function revealCurrent(token: number) {
    let range = currentRange();
    if (!range) return;
    const container = range.startContainer.parentElement;
    const article = container?.closest<HTMLElement>("article[data-message-id]");
    let expanded = false;
    if (article && container?.closest(".message-long-preview.collapsed")) {
      optionsRef.current.expandMessage(article.dataset.messageId!);
      expanded = true;
    }
    const codeBlock = container?.closest(".code-block-shell.collapsed");
    if (codeBlock) {
      codeBlock.dispatchEvent(new Event(CODE_BLOCK_EXPAND_EVENT));
      expanded = true;
    }
    if (expanded) {
      await nextFrame();
      if (token !== moveRef.current) return;
      recountRef.current();
      range = currentRange();
      if (!range) return;
    }
    // Rows far from the view skip rendering and are placed by an estimated
    // height, so the first scroll can land short; measure again until the
    // match stays in view for two frames.
    let settled = 0;
    for (let pass = 0; pass < SCROLL_SETTLE_FRAMES && range && settled < 2; pass += 1) {
      settled = optionsRef.current.scrollToMatch(range) ? settled + 1 : 0;
      await nextFrame();
      if (token !== moveRef.current) return;
      range = currentRange();
    }
  }

  /** Moves to the next (1) or previous (-1) match, loading its reply first when needed. */
  async function go(step: 1 | -1) {
    const token = ++moveRef.current;
    let { entries: current } = matchesRef.current;
    let target = stepFind(current, cursorRef.current, step);
    // Each pass either lands on a match or drops one unloaded hit.
    for (let attempt = 0; target && attempt < 20; attempt += 1) {
      const entry = current.find((candidate) => candidate.messageId === target!.messageId);
      if (!entry) return;
      if (entry.loaded) {
        updateCursor(target);
        await revealCurrent(token);
        return;
      }
      const { threadId: currentThreadId } = optionsRef.current;
      if (!currentThreadId) return;
      setMoving(true);
      try {
        await optionsRef.current.revealReply(currentThreadId, entry.seq);
        await waitForRow(optionsRef.current.contentRef, entry.messageId, RENDER_WAIT_FRAMES);
      } catch {
        return;
      } finally {
        if (token === moveRef.current) setMoving(false);
      }
      if (token !== moveRef.current) return;
      current = recountRef.current().entries;
      const loaded = current.find((candidate) => candidate.messageId === entry.messageId && candidate.loaded);
      if (loaded) {
        const index = step < 0 && target.index === entry.count - 1 ? loaded.count - 1 : Math.min(target.index, loaded.count - 1);
        updateCursor({ messageId: loaded.messageId, index });
        await revealCurrent(token);
        return;
      }
      // The reply did not render with a match (hidden, or the match is not visible text).
      skippedRef.current.add(entry.messageId);
      current = recountRef.current().entries;
      target = stepFind(current, cursorRef.current, step);
    }
  }

  function openFind() {
    setOpen(true);
    setFocusRequest((current) => current + 1);
  }

  // Focus once the bar has rendered: Cmd+F while it is open selects the query again.
  useLayoutEffect(() => {
    if (!open || focusRequest === 0) return;
    inputRef.current?.focus();
    inputRef.current?.select();
  }, [open, focusRequest]);

  function close() {
    moveRef.current += 1;
    hitsRef.current = { key: "", hits: [] };
    setOpen(false);
    setMoving(false);
  }

  // Another thread starts closed; the query stays for the next Cmd+F.
  useEffect(() => {
    moveRef.current += 1;
    setOpen(false);
    setMoving(false);
  }, [threadId]);

  // A new query counts again and selects the first match from the top of the view.
  useLayoutEffect(() => {
    skippedRef.current = new Set();
    cursorRef.current = null;
    const { entries: next, ranges } = recountRef.current();
    const viewport = viewportRef.current;
    const initial = viewport ? firstMatchInView(next, ranges, viewport) : null;
    const token = ++moveRef.current;
    updateCursor(initial);
    if (initial) void revealCurrent(token);
  }, [searchKey]);

  // Replies that are not loaded are counted by the server.
  const hasUnloadedReplies = typeof loadedReplyStart === "number";
  useEffect(() => {
    if (!searchKey || !hasUnloadedReplies || !threadId) {
      setSearching(false);
      return;
    }
    if (hitsRef.current.key === searchKey) {
      setSearching(false);
      return;
    }
    let cancelled = false;
    setSearching(true);
    const timer = window.setTimeout(() => {
      const beforeSeq = loadedStartRef.current;
      if (typeof beforeSeq !== "number") {
        setSearching(false);
        return;
      }
      void optionsRef.current.findUnloaded(threadId, query, beforeSeq).then((hits) => {
        if (cancelled) return;
        hitsRef.current = { key: searchKey, hits };
        recountRef.current();
      }).catch(() => {}).finally(() => {
        if (!cancelled) setSearching(false);
      });
    }, UNLOADED_FIND_DELAY_MS);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [searchKey, hasUnloadedReplies, threadId]);

  // Streaming text, highlighted code and new pages change the rendered text.
  useEffect(() => {
    const content = contentRef.current;
    if (!searchKey || !content || typeof MutationObserver === "undefined") return;
    let timer: number | null = null;
    const observer = new MutationObserver(() => {
      if (timer !== null) return;
      timer = window.setTimeout(() => {
        timer = null;
        recountRef.current();
      }, RECOUNT_DELAY_MS);
    });
    observer.observe(content, { childList: true, subtree: true, characterData: true });
    return () => {
      observer.disconnect();
      if (timer !== null) window.clearTimeout(timer);
    };
  }, [searchKey, threadId]);

  useEffect(() => {
    if (searchKey) recountRef.current();
  }, [loadedReplyStart]);

  useEffect(() => {
    if (open) return;
    clearHighlights(contentRef.current);
  }, [open]);

  useEffect(() => () => clearHighlights(contentRef.current), []);

  // Cmd+G and Shift+Cmd+G move between matches while the bar is open.
  useEffect(() => {
    if (!open) return;
    function onKeyDown(event: KeyboardEvent) {
      if (!(event.metaKey || event.ctrlKey) || event.altKey || event.key.toLowerCase() !== "g") return;
      event.preventDefault();
      void go(event.shiftKey ? -1 : 1);
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open]);

  const total = findTotal(entries);
  return {
    open,
    query,
    setQuery,
    inputRef,
    total,
    position: findPosition(entries, cursor),
    searching,
    moving,
    openFind,
    close,
    next: () => void go(1),
    previous: () => void go(-1),
  };
}

function renderedMatches(content: HTMLElement, needle: string) {
  const rendered: FindEntry[] = [];
  const ranges = new Map<string, Range[]>();
  for (const article of content.querySelectorAll<HTMLElement>("article[data-message-id]")) {
    const messageId = article.dataset.messageId!;
    const found = rangesIn(article, needle);
    ranges.set(messageId, found);
    rendered.push({ messageId, seq: Number(article.dataset.messageSeq) || 0, count: found.length, loaded: true });
  }
  return { rendered, ranges };
}

/** Matches in a message's text, each scope searched as one string. */
function rangesIn(article: HTMLElement, needle: string) {
  const ranges: Range[] = [];
  for (const scope of article.querySelectorAll<HTMLElement>(FIND_SCOPE)) {
    if (scope.parentElement?.closest(FIND_SCOPE) || scope.closest("article[data-message-id]") !== article) continue;
    const nodes: Text[] = [];
    const starts: number[] = [];
    let text = "";
    const walker = document.createTreeWalker(scope, NodeFilter.SHOW_TEXT, {
      acceptNode: (node) => (node.parentElement?.closest(FIND_SKIP) && scope.contains(node.parentElement.closest(FIND_SKIP))
        ? NodeFilter.FILTER_REJECT
        : NodeFilter.FILTER_ACCEPT),
    });
    for (let node = walker.nextNode(); node; node = walker.nextNode()) {
      const value = (node as Text).data;
      if (!value) continue;
      nodes.push(node as Text);
      starts.push(text.length);
      text += value;
    }
    for (const offset of findOffsets(foldFindText(text), needle)) {
      const range = document.createRange();
      const [startNode, startOffset] = position(nodes, starts, offset, false);
      const [endNode, endOffset] = position(nodes, starts, offset + needle.length, true);
      range.setStart(startNode, startOffset);
      range.setEnd(endNode, endOffset);
      ranges.push(range);
    }
  }
  return ranges;
}

/** The text node and offset for a position in the joined text. An end position stays in the node it ends. */
function position(nodes: Text[], starts: number[], offset: number, end: boolean): [Text, number] {
  let low = 0;
  let high = nodes.length - 1;
  while (low < high) {
    const middle = (low + high + 1) >>> 1;
    if (end ? starts[middle] < offset : starts[middle] <= offset) low = middle;
    else high = middle - 1;
  }
  return [nodes[low], offset - starts[low]];
}

/** The first match at or below the top of the thread's view; otherwise the last match. */
function firstMatchInView(entries: FindEntry[], ranges: Map<string, Range[]>, viewport: HTMLElement): FindCursor | null {
  const top = viewport.getBoundingClientRect().top;
  let last: FindCursor | null = null;
  for (const entry of entries) {
    const found = entry.loaded ? ranges.get(entry.messageId) ?? [] : [];
    if (!found.length) continue;
    last = { messageId: entry.messageId, index: found.length - 1 };
    // Rows above the view are passed by their box, without measuring their text.
    const article = found[0].startContainer.parentElement?.closest("article");
    if (article && article.getBoundingClientRect().bottom < top) continue;
    const index = found.findIndex((range) => range.getBoundingClientRect().bottom >= top);
    if (index >= 0) return { messageId: entry.messageId, index };
  }
  return last;
}

function sameEntries(left: FindEntry[], right: FindEntry[]) {
  return left.length === right.length && left.every((entry, index) => {
    const other = right[index];
    return entry.messageId === other.messageId && entry.count === other.count && entry.loaded === other.loaded && entry.seq === other.seq;
  });
}

function highlightsSupported() {
  return typeof CSS !== "undefined" && "highlights" in CSS && typeof Highlight === "function";
}

function paintMatches(ranges: Map<string, Range[]>) {
  if (!highlightsSupported()) return;
  // Added one by one: a short query can match more ranges than a call takes arguments.
  const highlight = new Highlight();
  for (const found of ranges.values()) for (const range of found) highlight.add(range);
  if (highlight.size) CSS.highlights.set(MATCH_HIGHLIGHT, highlight);
  else CSS.highlights.delete(MATCH_HIGHLIGHT);
}

function clearHighlights(content: HTMLElement | null) {
  for (const element of content?.querySelectorAll<HTMLElement>("[data-find-current]") ?? []) delete element.dataset.findCurrent;
  if (!highlightsSupported()) return;
  CSS.highlights.delete(MATCH_HIGHLIGHT);
  CSS.highlights.delete(CURRENT_HIGHLIGHT);
}

function nextFrame() {
  return new Promise<void>((resolve) => window.requestAnimationFrame(() => resolve()));
}

async function waitForRow(contentRef: RefObject<HTMLElement | null>, messageId: string, frames: number) {
  for (let frame = 0; frame < frames; frame += 1) {
    if (contentRef.current?.querySelector(`article[data-message-id="${messageId}"]`)) {
      // One more frame lets its Markdown settle.
      await nextFrame();
      return;
    }
    await nextFrame();
  }
}
