import { useLayoutEffect, useRef, useState, type UIEvent, type WheelEvent, type PointerEvent, type KeyboardEvent, type TouchEvent, type SyntheticEvent } from "react";
import { captureMessageAnchor, flushChannelPositions, readChannelPosition, rememberChannelPosition, type MessageScrollAnchor } from "../channel-reading-position";
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

// Own the entire viewport lifecycle here. In particular, an old DOM node,
// pending frame or history response must never control a new channel's list.
export function useChannelMessageScroll(options: Options) {
  const viewportRef = useRef<HTMLDivElement | null>(null);
  const contentRef = useRef<HTMLDivElement | null>(null);
  const latest = useRef(options);
  const controller = useRef<Controller | null>(null);
  const [restoring, setRestoring] = useState(true);
  const [showBackToBottom, setShowBackToBottom] = useState(false);
  useLayoutEffect(() => { latest.current = options; });

  useLayoutEffect(() => {
    const node = viewportRef.current, content = contentRef.current;
    const channelId = options.channelId;
    if (!node || !content || !channelId || !options.active) return;
    const viewport: HTMLDivElement = node;
    const saved = readChannelPosition(channelId);
    let anchor = saved?.anchor ?? null;
    let follow = !saved || saved.atBottom;
    let initial = true, placing = false, disposed = false, inFlight = false;
    let frame = 0, settleFrame = 0, loadFrame = 0, reportFrame = 0, userUntil = 0;
    let focusId: string | null = null, lastFocusId: string | null = null;
    let lastReported: ChannelReadLocation | null = null;
    let failedRestore = false;
    let preserveSavedPosition = false;
    let metrics = { top: 0, height: 0, clientHeight: 0 };
    setRestoring(true);
    setShowBackToBottom(false);

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
      setShowBackToBottom(!initial && isVisible() && distance() > 32);
    }
    function remember(preserveAnchor = false) {
      if (initial || !latest.current.ready || !isVisible()) return;
      // Keep the requested anchor across programmatic placement. Re-saving
      // rounded DOM offsets on each switch causes cumulative subpixel drift.
      if (!preserveAnchor || !anchor || !row(anchor.messageId)) anchor = captureMessageAnchor(viewport);
      metrics = { top: viewport.scrollTop, height: viewport.scrollHeight, clientHeight: viewport.clientHeight };
      if (!preserveSavedPosition && anchor && lastRoot()) rememberChannelPosition(channelId!, { anchor, atBottom: distance() <= 2, latestRootId: lastRoot()!.id });
    }
    function cancelFrames() {
      cancelAnimationFrame(frame); cancelAnimationFrame(settleFrame); cancelAnimationFrame(reportFrame);
      frame = settleFrame = reportFrame = 0;
    }
    function finishPlacement() {
      if (!isLive()) return;
      initial = placing = false;
      setRestoring(false);
      remember(!follow); report();
    }
    function nearestAnchor(): MessageScrollAnchor | null {
      if (!anchor || !latest.current.roots.length) return null;
      const target = latest.current.roots.reduce((best, message) =>
        Math.abs(message.seq - anchor!.seq) < Math.abs(best.seq - anchor!.seq) ? message : best);
      return { messageId: target.id, seq: target.seq, offset: 0 };
    }
    function requestOlder(forRestore: boolean) {
      if (inFlight || latest.current.loading || !latest.current.hasMore) return;
      inFlight = true;
      const before = latest.current.roots;
      if (!forRestore) { follow = false; remember(); }
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
    function place(pass = 0) {
      frame = 0;
      if (!isLive() || !latest.current.ready || !isVisible() || inFlight) return;
      if (initial && saved?.atBottom && saved.latestRootId !== lastRoot()?.id) follow = false;
      if (focusId) {
        const target = row(focusId);
        if (target) {
          follow = false;
          anchor = { messageId: focusId, seq: Number(target.dataset.messageSeq ?? 0),
            offset: Math.max(0, (viewport.clientHeight - target.getBoundingClientRect().height) / 2) };
          focusId = null;
        }
      }
      if (initial && !follow && anchor && latest.current.hasMore && !failedRestore) {
        // Contextual roots (saved tasks / work items) can be older than the
        // contiguous loaded timeline. Their presence alone is not hydration.
        const boundary = latest.current.historyBeforeSeq;
        const needsHistory = boundary === undefined ? !row(anchor.messageId) : anchor.seq < boundary;
        if (needsHistory) { requestOlder(true); return; }
      }
      if (!follow && anchor && !row(anchor.messageId)) {
        anchor = nearestAnchor();
        if (!anchor) follow = true;
      }
      placing = true;
      if (follow) {
        // Read live geometry at the write boundary: cached ResizeObserver
        // height can still belong to the preview or pre-hydration layout.
        const bottom = Math.max(0, viewport.scrollHeight - viewport.clientHeight);
        if (Math.abs(viewport.scrollTop - bottom) > 0.5) viewport.scrollTop = bottom;
      } else if (anchor) {
        const target = row(anchor.messageId);
        if (target) {
          const delta = target.getBoundingClientRect().top - viewport.getBoundingClientRect().top - anchor.offset;
          if (Math.abs(delta) > 0.5) viewport.scrollTop += delta;
        }
      }
      cancelAnimationFrame(settleFrame);
      settleFrame = requestAnimationFrame(() => {
        settleFrame = 0;
        if (!isLive()) return;
        const target = anchor && row(anchor.messageId);
        const delta = follow ? distance() : target
          ? target.getBoundingClientRect().top - viewport.getBoundingClientRect().top - anchor!.offset : 0;
        // content-visibility estimates may settle over several layouts. Keep
        // the desired anchor until settled, not an intermediate pixel offset.
        if (Math.abs(delta) > 1 && pass < 6) { place(pass + 1); return; }
        finishPlacement();
      });
    }
    function schedule() {
      if (!isLive() || frame || inFlight) return;
      frame = requestAnimationFrame(() => place());
    }
    function userScroll() {
      if (!isLive() || !latest.current.ready) return;
      userUntil = Date.now() + 800;
      preserveSavedPosition = false;
      follow = false;
      initial = placing = false;
      focusId = null;
      cancelFrames(); setRestoring(false); remember(); report();
    }
    function changed() {
      const id = latest.current.focusedMessageId;
      if (id && id !== lastFocusId) { focusId = id; follow = false; }
      lastFocusId = id;
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
        if (!isLive() || initial || placing || !latest.current.ready) return;
        const movedUp = viewport.scrollTop < metrics.top - 0.5 && viewport.scrollHeight === metrics.height
          && viewport.clientHeight === metrics.clientHeight;
        follow = distance() < 32;
        if (movedUp) preserveSavedPosition = false;
        remember(); report();
        // Also support native overlay scrollbars and accessibility scrolling,
        // which need not produce a DOM wheel/pointer event.
        if ((movedUp || Date.now() < userUntil) && viewport.scrollTop <= 96) requestOlder(false);
      },
      bottom() { cancelFrames(); preserveSavedPosition = false; follow = true; initial = false; focusId = null; userUntil = 0; schedule(); },
      focus(id) { userScroll(); focusId = id; follow = false; schedule(); },
    };
    // Streaming height changes while following do not invalidate the read
    // location or rerender the App on every token. New roots are fenced by ID.
    const stopObserving = observeScrollGeometry(viewport, content, () => { if (!follow || !isVisible()) report(); schedule(); });
    const flush = () => {
      if (!placing) remember(!follow && Math.abs(viewport.scrollTop - metrics.top) < 1);
      flushChannelPositions();
    };
    window.addEventListener("pagehide", flush);
    document.addEventListener("visibilitychange", flush);
    changed();
    return () => {
      // The last committed position is already captured. The keyed old node
      // may have been detached now, so never measure it during this cleanup.
      disposed = true; cancelFrames(); cancelAnimationFrame(loadFrame); stopObserving(); flushChannelPositions();
      window.removeEventListener("pagehide", flush);
      document.removeEventListener("visibilitychange", flush);
      controller.current = null;
    };
  }, [options.channelId, options.active]);

  useLayoutEffect(() => { controller.current?.changed(); }, [options.roots, options.ready, options.focusedMessageId]);
  const current = (element: HTMLDivElement) => controller.current?.viewport === element ? controller.current : null;
  return {
    viewportRef, contentRef, restoring: Boolean(options.channelId) && restoring, showBackToBottom,
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
