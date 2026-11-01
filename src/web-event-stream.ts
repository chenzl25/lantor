import { checkWebSession, isWebSessionExpired, subscribeWebSession } from "./web-session";

export type EventReplay = {
  cursor: number;
  replayGap: boolean;
  events: { cursor: number; event: string }[];
};
export type EventSubscription = (() => void) & { reconcile: () => Promise<void> };

/** A stream that has delivered nothing for this long while replay recovers
 * events it missed is treated as dead. iOS can resume a suspended Home Screen
 * app with an EventSource that still reports open but never delivers again. */
export const STALE_EVENT_STREAM_MS = 15_000;

export function eventRetryDelay(attempt: number, random = Math.random()): number {
  return Math.min(30_000, 1_000 * 2 ** Math.min(attempt, 5)) * (0.8 + 0.2 * random);
}

// Close EventSource on error to disable the browser's fixed reconnect interval.
// Both HTTP reconciliation and SSE feed the same cursor gate, so overlapping
// replay cannot double-append streamed text or resurrect an older message.
export function subscribeWebEvents(
  handler: (payload: string) => void,
  options: { cursor?: number | null; onCursor?: (cursor: number) => void },
  replay: (cursor: number) => Promise<EventReplay>,
): EventSubscription {
  let cursor = Number.isSafeInteger(options.cursor) && (options.cursor ?? -1) >= 0 ? options.cursor! : 0;
  let source: EventSource | null = null;
  let disposed = false;
  let attempt = 0;
  let retryTimer: ReturnType<typeof setTimeout> | null = null;
  let stableTimer: ReturnType<typeof setTimeout> | null = null;
  let reconciliation: Promise<void> | null = null;
  let lastStreamActivityAt = 0;

  function deliver(next: number, payload: string, gap = false) {
    if (disposed || (gap ? next === cursor : next <= cursor)) return false;
    handler(payload);
    cursor = next;
    options.onCursor?.(cursor);
    return true;
  }
  function connect() {
    if (disposed || source || isWebSessionExpired()) return;
    const connection = new EventSource(`/api/events?cursor=${cursor}`);
    source = connection;
    lastStreamActivityAt = Date.now();
    connection.addEventListener("lantor", (event) => {
      if (source !== connection || disposed) return;
      lastStreamActivityAt = Date.now();
      const message = event as MessageEvent<string>;
      // Legacy unnumbered events remain compatible with older backends.
      if (!message.lastEventId) { handler(message.data); return; }
      const id = Number(message.lastEventId);
      if (!Number.isSafeInteger(id) || id < 0) return;
      let gap = false;
      try { gap = JSON.parse(message.data).reason === "event_replay_gap"; } catch { /* handler reports malformed payload */ }
      deliver(id, message.data, gap);
    });
    connection.onopen = () => {
      if (source !== connection || disposed) return;
      lastStreamActivityAt = Date.now();
      // A server that opens then immediately fails must still back off.
      stableTimer = setTimeout(() => { attempt = 0; stableTimer = null; }, 10_000);
    };
    connection.onerror = () => {
      if (source !== connection || disposed) return;
      connection.close();
      source = null;
      if (stableTimer !== null) clearTimeout(stableTimer);
      stableTimer = null;
      retryTimer = setTimeout(() => { retryTimer = null; connect(); }, eventRetryDelay(attempt++));
      void checkWebSession();
    };
  }
  const unsubscribeSession = subscribeWebSession(() => {
    if (isWebSessionExpired()) {
      source?.close();
      source = null;
      if (retryTimer !== null) clearTimeout(retryTimer);
      if (stableTimer !== null) clearTimeout(stableTimer);
      retryTimer = stableTimer = null;
    } else {
      attempt = 0;
      connect();
    }
  });
  const stop = (() => {
    disposed = true;
    unsubscribeSession();
    source?.close();
    source = null;
    if (retryTimer !== null) clearTimeout(retryTimer);
    if (stableTimer !== null) clearTimeout(stableTimer);
  }) as EventSubscription;
  stop.reconcile = () => {
    if (disposed || isWebSessionExpired()) return Promise.resolve();
    if (reconciliation) return reconciliation;
    // Foreground/online recovery gets one immediate reconnect, independent of
    // a timer that may have been suspended while the page was in the background.
    if (!source) {
      if (retryTimer !== null) clearTimeout(retryTimer);
      retryTimer = null;
      connect();
    }
    const requestedCursor = cursor;
    const streamAtRequest = source;
    reconciliation = replay(requestedCursor).then((result) => {
      if (disposed) return;
      let recovered = false;
      if (result.replayGap) {
        // An SSE gap/reconnect may already have recovered this request's cursor.
        if (cursor === requestedCursor) recovered = deliver(result.cursor, JSON.stringify({ type: "refresh", reason: "event_replay_gap" }), true);
      } else {
        for (const item of result.events) recovered = deliver(item.cursor, item.event) || recovered;
      }
      // Replay found events that an idle "open" stream never delivered, so the
      // connection is silently dead: replace it instead of waiting for an error
      // that a half-open socket never raises.
      if (recovered && source && source === streamAtRequest
        && Date.now() - lastStreamActivityAt > STALE_EVENT_STREAM_MS) {
        source.close();
        source = null;
        if (stableTimer !== null) clearTimeout(stableTimer);
        stableTimer = null;
        connect();
      }
    }).finally(() => { reconciliation = null; });
    return reconciliation;
  };
  connect();
  return stop;
}
