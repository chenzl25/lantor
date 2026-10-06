import { useLayoutEffect, useRef, useState, type UIEvent, type WheelEvent, type PointerEvent, type KeyboardEvent, type TouchEvent, type SyntheticEvent } from "react";
import { captureMessageAnchor, countUnseenRoots, firstUnreadRoot, lastVisibleMessageSeq, readChannelPosition, rememberChannelPosition, type MessageScrollAnchor } from "../channel-reading-position";
import { observeScrollGeometry } from "../scroll-geometry";
import type { Message } from "../types";

export type ChannelReadLocation = { channelId: string; latestRootId: string | null; atLatest: boolean };
type Options = {
  channelId: string | null;
  active: boolean;
  ready: boolean;
  roots: Message[];
  focusedMessageId: string | null;
  hasMore: boolean;
  historyBeforeSeq?: number;
  loading: boolean;
  loadOlder: () => Promise<boolean | void>;
  onReadLocation?: (location: ChannelReadLocation) => void;
  /** The server's first unread top-level message in this channel. */
  firstUnreadSeq?: number | null;
  /** True while the app catches up after returning from the background. */
  syncing?: boolean;
};
type Controller = {
  viewport: HTMLDivElement;
  changed: () => void;
  scroll: () => void;
  userScroll: () => void;
  bottom: () => void;
  focus: (id: string) => void;
  resize: () => void;
};

// Without a sync signal after returning to the page, settle on current data.
const RESUME_SYNC_WAIT_MS = 1500;
// Space kept above the "New messages" divider when it opens at the top.
const DIVIDER_TOP_GAP = 8;

// Own the entire viewport lifecycle here. In particular, an old DOM node,
// pending frame or history response must never control a new channel's list.
//
// Where a channel opens: at the first unread top-level message (with a
// divider above it) when the server reports one; otherwise at a position
// read earlier in this page session, or at the latest message. Returning
// to the page while at the latest message applies the same rule once the
// catch-up sync finishes.
export function useChannelMessageScroll(options: Options) {
  const viewportRef = useRef<HTMLDivElement | null>(null);
  const contentRef = useRef<HTMLDivElement | null>(null);
  const latest = useRef(options);
  const controller = useRef<Controller | null>(null);
  const [restoring, setRestoring] = useState(true);
  const [showBackToBottom, setShowBackToBottom] = useState(false);
  const [unseenCount, setUnseenCount] = useState(0);
  const [unreadDividerId, setUnreadDividerId] = useState<string | null>(null);
  useLayoutEffect(() => { latest.current = options; });

  useLayoutEffect(() => {
    const node = viewportRef.current, content = contentRef.current;
    const channelId = options.channelId;
    if (!node || !content || !channelId || !options.active) return;
    const viewport: HTMLDivElement = node;
    const saved = readChannelPosition(channelId);
    let anchor: MessageScrollAnchor | null = null;
    let follow = true, decided = false;
    let unreadSeq: number | null = null, unreadAnchor = false;
    let seen = saved?.seenSeq ?? 0;
    let initial = true, placing = false, disposed = false, inFlight = false;
    let frame = 0, settleFrame = 0, loadFrame = 0, reportFrame = 0, userUntil = 0;
    let focusId: string | null = null, lastFocusId: string | null = null;
    let lastReported: ChannelReadLocation | null = null;
    let failedRestore = false;
    let writtenTop = -1;
    let preserveSavedPosition = false;
    let metrics = { top: 0, height: 0, clientHeight: 0 };
    let wasHidden = false, hiddenFollow = false, awaitingResume = false, sawSync = false;
    let resumeTimer: ReturnType<typeof setTimeout> | undefined;
    setRestoring(true);
    setShowBackToBottom(false);
    setUnseenCount(0);
    setUnreadDividerId(null);

    const isLive = () => !disposed && viewportRef.current === viewport && latest.current.channelId === channelId;
    const isVisible = () => viewport.clientHeight > 0 && viewport.clientWidth > 0 && viewport.getClientRects().length > 0;
    const distance = () => viewport.scrollHeight - viewport.scrollTop - viewport.clientHeight;
    const lastRoot = () => latest.current.roots[latest.current.roots.length - 1] ?? null;
    const row = (id: string) => viewport.querySelector<HTMLElement>(`article[data-message-id="${CSS.escape(id)}"]`);
    function report() {
      const location = { channelId: channelId!, latestRootId: lastRoot()?.id ?? null,
        atLatest: !initial && !placing && latest.current.ready && isVisible() && distance() <= 2 };
      if (location.atLatest !== lastReported?.atLatest || location.latestRootId !== lastReported?.latestRootId) {
        lastReported = location;
        latest.current.onReadLocation?.(location);
      }
      if (!initial && !placing && isVisible() && document.visibilityState === "visible") {
        seen = Math.max(seen, lastVisibleMessageSeq(viewport));
      }
      setUnseenCount(initial ? 0 : countUnseenRoots(latest.current.roots, seen));
      setShowBackToBottom(!initial && isVisible() && distance() > 32);
    }
    function remember(preserveAnchor = false) {
      if (initial || !latest.current.ready || !isVisible()) return;
      // Keep the requested anchor across programmatic placement. Re-saving
      // rounded DOM offsets on each switch causes cumulative subpixel drift.
      if (!preserveAnchor || !anchor || !row(anchor.messageId)) anchor = captureMessageAnchor(viewport);
      metrics = { top: viewport.scrollTop, height: viewport.scrollHeight, clientHeight: viewport.clientHeight };
      if (!preserveSavedPosition && anchor && lastRoot()) {
        rememberChannelPosition(channelId!, { anchor, atBottom: distance() <= 2, latestRootId: lastRoot()!.id, seenSeq: seen });
      }
    }
    function cancelFrames() {
      cancelAnimationFrame(frame); cancelAnimationFrame(settleFrame); cancelAnimationFrame(reportFrame);
      frame = settleFrame = reportFrame = 0;
    }
    function finishPlacement() {
      if (!isLive()) return;
      initial = placing = false;
      setRestoring(false);
      // Unread messages that all fit on screen leave the list at the latest
      // message, so keep following what arrives next.
      if (unreadAnchor && distance() <= 2) { follow = true; unreadAnchor = false; }
      remember(!follow); report();
    }
    function nearestAnchor(): MessageScrollAnchor | null {
      if (!anchor || !latest.current.roots.length) return null;
      const target = latest.current.roots.reduce((best, message) =>
        Math.abs(message.seq - anchor!.seq) < Math.abs(best.seq - anchor!.seq) ? message : best);
      return { messageId: target.id, seq: target.seq, offset: 0 };
    }
    function openAtUnread(message: Message) {
      seen = Math.max(seen, message.seq - 1);
      anchor = { messageId: message.id, seq: message.seq, offset: DIVIDER_TOP_GAP };
      follow = false; unreadAnchor = true;
      setUnreadDividerId(message.id);
    }
    // Decide once per opening, from the data present when the list is ready.
    function decide() {
      decided = true;
      const firstUnread = latest.current.firstUnreadSeq;
      // Messages this page already showed stay read even before the server
      // receipt lands, which only happens at the latest message.
      const from = firstUnread == null ? null : Math.max(firstUnread, seen + 1);
      if (from !== null && from <= (lastRoot()?.seq ?? 0)) {
        unreadSeq = from;
        follow = false;
        return;
      }
      if (saved && !saved.atBottom) { anchor = saved.anchor; follow = false; }
      else follow = true;
      seen = Math.max(seen, lastRoot()?.seq ?? 0);
    }
    // Keep the "New messages" divider, and any date divider right above it,
    // on screen below the top edge.
    function dividerGap(target: HTMLElement) {
      let top = target;
      while (top.previousElementSibling instanceof HTMLElement
        && top.previousElementSibling.matches(".message-unread-divider, .message-date-divider")) top = top.previousElementSibling;
      return target.getBoundingClientRect().top - top.getBoundingClientRect().top + DIVIDER_TOP_GAP;
    }
    function requestOlder(forRestore: boolean) {
      if (inFlight || latest.current.loading || !latest.current.hasMore) return;
      inFlight = true;
      const before = latest.current.roots;
      if (!forRestore) { follow = false; unreadAnchor = false; remember(); }
      void latest.current.loadOlder().then((progress) => {
        if (!isLive()) return;
        // Wait for React to commit the prepended page. A failed/no-progress page
        // must not cause an unbounded restore/retry loop.
        loadFrame = requestAnimationFrame(() => {
          loadFrame = 0;
          if (!isLive()) return;
          inFlight = false;
          if (forRestore && (progress === false || (before.length === latest.current.roots.length
            && before[0]?.id === latest.current.roots[0]?.id))) {
            failedRestore = true;
            preserveSavedPosition = true;
          }
          schedule();
        });
      }).catch(() => {
        if (!isLive()) return;
        inFlight = false; failedRestore = true; preserveSavedPosition = true; schedule();
      });
    }
    // Contextual roots (saved tasks / work items) can be older than the
    // contiguous loaded timeline. Their presence alone is not hydration.
    function needsHistory(seq: number, id?: string) {
      if (!latest.current.hasMore || failedRestore) return false;
      const boundary = latest.current.historyBeforeSeq;
      if (boundary !== undefined) return seq < boundary;
      return id ? !row(id) : !latest.current.roots.some((message) => message.seq <= seq);
    }
    function place(pass = 0) {
      frame = 0;
      if (!isLive() || !latest.current.ready || !isVisible() || inFlight) return;
      if (initial && !decided) decide();
      if (focusId) {
        const target = row(focusId);
        if (target) {
          follow = false; unreadAnchor = false; unreadSeq = null;
          anchor = { messageId: focusId, seq: Number(target.dataset.messageSeq ?? 0),
            offset: Math.max(0, (viewport.clientHeight - target.getBoundingClientRect().height) / 2) };
          focusId = null;
        }
      }
      if (unreadSeq !== null) {
        if (needsHistory(unreadSeq)) { requestOlder(true); return; }
        const message = firstUnreadRoot(latest.current.roots, unreadSeq);
        unreadSeq = null;
        if (message) openAtUnread(message);
        else { follow = true; seen = Math.max(seen, lastRoot()?.seq ?? 0); }
      }
      if (initial && !follow && anchor && !unreadAnchor && needsHistory(anchor.seq, anchor.messageId)) {
        requestOlder(true); return;
      }
      if (!follow && anchor && !row(anchor.messageId)) {
        anchor = nearestAnchor(); unreadAnchor = false;
        if (!anchor) follow = true;
      }
      placing = true;
      if (follow) {
        // Read live geometry at the write boundary: cached ResizeObserver
        // height can still belong to the preview or pre-hydration layout.
        const bottom = Math.max(0, viewport.scrollHeight - viewport.clientHeight);
        if (Math.abs(viewport.scrollTop - bottom) > 0.5) { viewport.scrollTop = bottom; writtenTop = viewport.scrollTop; }
      } else if (anchor) {
        const target = row(anchor.messageId);
        if (target) {
          if (unreadAnchor) anchor.offset = dividerGap(target);
          const delta = target.getBoundingClientRect().top - viewport.getBoundingClientRect().top - anchor.offset;
          if (Math.abs(delta) > 0.5) { viewport.scrollTop += delta; writtenTop = viewport.scrollTop; }
        }
      }
      cancelAnimationFrame(settleFrame);
      settleFrame = requestAnimationFrame(() => {
        settleFrame = 0;
        if (!isLive()) return;
        const target = anchor && row(anchor.messageId);
        if (target && unreadAnchor) anchor!.offset = dividerGap(target);
        const delta = follow ? distance() : target
          ? target.getBoundingClientRect().top - viewport.getBoundingClientRect().top - anchor!.offset : 0;
        // A clamped scroll position cannot move further toward the anchor.
        const clamped = !follow && delta > 0 && distance() <= 0.5;
        // content-visibility estimates may settle over several layouts. Keep
        // the desired anchor until settled, not an intermediate pixel offset.
        if (Math.abs(delta) > 1 && !clamped && pass < 6) { place(pass + 1); return; }
        finishPlacement();
      });
    }
    function schedule() {
      if (!isLive() || frame || inFlight) return;
      frame = requestAnimationFrame(() => place());
    }
    function stopResume() {
      clearTimeout(resumeTimer);
      awaitingResume = sawSync = hiddenFollow = false;
    }
    // After returning to the page: move a reader who was at the latest
    // message to what arrived meanwhile, now that the data is current.
    function finishResume() {
      if (!isLive() || !awaitingResume) return;
      const wasFollowing = hiddenFollow;
      stopResume();
      const message = firstUnreadRoot(latest.current.roots, latest.current.firstUnreadSeq, seen);
      if (wasFollowing) {
        if (message) openAtUnread(message);
        else follow = true;
        schedule();
      } else if (message) setUnreadDividerId(message.id);
    }
    function userScroll() {
      if (!isLive() || !latest.current.ready) return;
      userUntil = Date.now() + 800;
      preserveSavedPosition = false;
      follow = false; unreadAnchor = false; unreadSeq = null;
      initial = placing = false;
      focusId = null;
      stopResume();
      cancelFrames(); setRestoring(false); remember(); report();
    }
    function changed() {
      const id = latest.current.focusedMessageId;
      if (id && id !== lastFocusId) { focusId = id; follow = false; }
      lastFocusId = id;
      if (awaitingResume) {
        if (latest.current.syncing) sawSync = true;
        else if (sawSync) finishResume();
      }
      // This runs from a layout effect. Reporting here can update the parent,
      // commit another message snapshot and re-enter this effect before the
      // browser gets a frame. Coalesce reports without waiting for history
      // restoration, which still needs to invalidate an old read location.
      if (!reportFrame) reportFrame = requestAnimationFrame(() => {
        reportFrame = 0;
        if (isLive()) report();
      });
      schedule();
    }
    controller.current = {
      viewport, changed, userScroll, resize: schedule,
      scroll() {
        if (!isLive() || initial || !latest.current.ready) return;
        if (placing) {
          // Placement writes emit scroll events too; ignore only those. Other
          // movement (touch momentum, scrollbar, keys) supersedes placement.
          // Settling against an anchor captured before it pulls the viewport
          // back by one frame of scrolling on every frame.
          if (Math.abs(viewport.scrollTop - writtenTop) <= 1) return;
          cancelAnimationFrame(settleFrame); settleFrame = 0; placing = false;
        }
        const movedUp = viewport.scrollTop < metrics.top - 0.5 && viewport.scrollHeight === metrics.height
          && viewport.clientHeight === metrics.clientHeight;
        follow = distance() < 32;
        if (follow || movedUp) unreadAnchor = false;
        if (movedUp) preserveSavedPosition = false;
        remember(); report();
        // Also support native overlay scrollbars and accessibility scrolling,
        // which need not produce a DOM wheel/pointer event.
        if ((movedUp || Date.now() < userUntil) && viewport.scrollTop <= 96) requestOlder(false);
      },
      bottom() {
        cancelFrames(); stopResume(); preserveSavedPosition = false;
        follow = true; unreadAnchor = false; unreadSeq = null; initial = false; focusId = null; userUntil = 0; schedule();
      },
      focus(id) { userScroll(); focusId = id; follow = false; schedule(); },
    };
    // Streaming height changes while following do not invalidate the read
    // location or rerender the App on every token. New roots are fenced by ID.
    const stopObserving = observeScrollGeometry(viewport, content, () => { if (!follow || !isVisible()) report(); schedule(); });
    const onPageHide = () => { if (!placing) remember(!follow && Math.abs(viewport.scrollTop - metrics.top) < 1); };
    const onVisibility = () => {
      if (!isLive()) return;
      if (document.visibilityState === "hidden") {
        onPageHide();
        const following = hiddenFollow || (!initial && follow);
        stopResume();
        wasHidden = true;
        // Hold the current view while away. Messages that arrive meanwhile
        // must not scroll the reader past the point they last saw.
        if (following) {
          if (follow) anchor = captureMessageAnchor(viewport) ?? anchor;
          hiddenFollow = true; follow = false; unreadAnchor = false;
        }
        return;
      }
      if (!wasHidden || initial) return;
      wasHidden = false;
      awaitingResume = true;
      sawSync = Boolean(latest.current.syncing);
      clearTimeout(resumeTimer);
      resumeTimer = setTimeout(() => { if (!sawSync) finishResume(); }, RESUME_SYNC_WAIT_MS);
    };
    window.addEventListener("pagehide", onPageHide);
    document.addEventListener("visibilitychange", onVisibility);
    changed();
    return () => {
      // The last committed position is already captured. The keyed old node
      // may have been detached now, so never measure it during this cleanup.
      disposed = true; cancelFrames(); cancelAnimationFrame(loadFrame); clearTimeout(resumeTimer); stopObserving();
      window.removeEventListener("pagehide", onPageHide);
      document.removeEventListener("visibilitychange", onVisibility);
      controller.current = null;
    };
  }, [options.channelId, options.active]);

  useLayoutEffect(() => { controller.current?.changed(); },
    [options.roots, options.ready, options.focusedMessageId, options.syncing, options.firstUnreadSeq]);
  const current = (element: HTMLDivElement) => controller.current?.viewport === element ? controller.current : null;
  return {
    viewportRef, contentRef, restoring: Boolean(options.channelId) && restoring, showBackToBottom,
    unseenCount, unreadDividerId,
    onScroll: (event: UIEvent<HTMLDivElement>) => current(event.currentTarget)?.scroll(),
    onWheel: (event: WheelEvent<HTMLDivElement>) => { if (event.deltaY < 0) current(event.currentTarget)?.userScroll(); },
    onTouchMove: (event: TouchEvent<HTMLDivElement>) => current(event.currentTarget)?.userScroll(),
    onPointerDown: (event: PointerEvent<HTMLDivElement>) => {
      const element = event.currentTarget;
      const width = element.offsetWidth - element.clientWidth;
      if (width > 0 && event.clientX >= element.getBoundingClientRect().right - width - 2) current(element)?.userScroll();
    },
    onKeyDown: (event: KeyboardEvent<HTMLDivElement>) => {
      if (event.target instanceof HTMLElement && event.target.closest("input,textarea,[contenteditable=true]")) return;
      if (["ArrowUp", "PageUp", "Home", "ArrowDown", "PageDown", "End", " "].includes(event.key)) current(event.currentTarget)?.userScroll();
    },
    onContentLoad: (event: SyntheticEvent<HTMLDivElement>) => current(event.currentTarget)?.resize(),
    toBottom: () => controller.current?.bottom(),
    focusMessage: (id: string) => controller.current?.focus(id),
  };
}
