import assert from "node:assert/strict";
import test from "node:test";
import { eventRetryDelay, subscribeWebEvents, type EventReplay } from "../src/web-event-stream";

test("SSE backoff grows, caps at 30s and includes bounded jitter", () => {
  assert.deepEqual([0, 1, 2, 3, 4, 5, 12].map((n) => eventRetryDelay(n, 1)), [1000, 2000, 4000, 8000, 16000, 30000, 30000]);
  assert.equal(eventRetryDelay(2, 0), 3200);
});

test("SSE closes failed connections, retains cursor across retries and ignores duplicate replay", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const sources: FakeSource[] = [];
  class FakeSource extends EventTarget {
    closed = false;
    onopen: (() => void) | null = null;
    onerror: (() => void) | null = null;
    constructor(public url: string) { super(); sources.push(this); }
    close() { this.closed = true; }
    send(id: number, data: string) { this.dispatchEvent(new MessageEvent("lantor", { lastEventId: String(id), data })); }
  }
  const original = globalThis.EventSource;
  globalThis.EventSource = FakeSource as unknown as typeof EventSource;
  t.after(() => { globalThis.EventSource = original; });
  let completeReplay: (value: EventReplay) => void = () => {};
  let replayCursor = -1;
  const values: string[] = [];
  const subscription = subscribeWebEvents((value) => values.push(value), { cursor: 4 }, (cursor) => {
    replayCursor = cursor;
    return new Promise((resolve) => { completeReplay = resolve; });
  });
  t.after(subscription);
  sources[0].send(5, "first");
  sources[0].onerror?.();
  assert.equal(sources[0].closed, true);
  t.mock.timers.tick(1000);
  assert.equal(sources.length, 2);
  assert.match(sources[1].url, /cursor=5$/);
  sources[1].onopen?.();
  sources[1].onerror?.();
  t.mock.timers.tick(1000);
  assert.equal(sources.length, 2, "short-lived connections must not reset exponential backoff");
  t.mock.timers.tick(1000);
  assert.equal(sources.length, 3);
  const pending = subscription.reconcile();
  assert.equal(subscription.reconcile(), pending, "foreground events share a single replay");
  assert.equal(replayCursor, 5);
  sources[2].send(6, "second");
  completeReplay({ cursor: 7, replayGap: false, events: [{ cursor: 6, event: "second" }, { cursor: 7, event: "third" }] });
  await pending;
  sources[2].send(7, "third");
  assert.deepEqual(values, ["first", "second", "third"]);
  sources[2].onerror?.();
  subscription();
  t.mock.timers.tick(60000);
  assert.equal(sources.length, 3, "disposal cancels retry timers");
  sources[2].send(8, "late");
  assert.equal(values.length, 3);
});

test("replay gaps can reset a cursor after database replacement and disposed replay is ignored", async (t) => {
  class FakeSource extends EventTarget { close() {} }
  const original = globalThis.EventSource;
  globalThis.EventSource = FakeSource as unknown as typeof EventSource;
  t.after(() => { globalThis.EventSource = original; });
  const cursors: number[] = [];
  const events: unknown[] = [];
  let finish: (value: EventReplay) => void = () => {};
  const subscription = subscribeWebEvents((value) => events.push(JSON.parse(value)), { cursor: 50, onCursor: (value) => cursors.push(value) },
    () => new Promise((resolve) => { finish = resolve; }));
  const gap = subscription.reconcile();
  finish({ cursor: 3, replayGap: true, events: [] });
  await gap;
  assert.deepEqual(cursors, [3]);
  assert.deepEqual(events, [{ type: "refresh", reason: "event_replay_gap" }]);
  const late = subscription.reconcile();
  subscription();
  finish({ cursor: 4, replayGap: false, events: [{ cursor: 4, event: '{"type":"late"}' }] });
  await late;
  assert.equal(events.length, 1);
});
