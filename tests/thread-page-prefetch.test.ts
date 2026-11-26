import assert from "node:assert/strict";
import test from "node:test";
import { ThreadPagePrefetches } from "../src/thread-page-prefetch";

function setup() {
  let now = 1_000;
  const requests: string[] = [];
  const prefetches = new ThreadPagePrefetches<string, string>(() => now, 5_000);
  const start = (threadId: string, args: object, baseline = "held") => prefetches.start(threadId, args, () => baseline, () => {
    requests.push(`${threadId} ${JSON.stringify(args)}`);
    return Promise.resolve(`page of ${threadId}`);
  });
  return { prefetches, requests, start, tick: (ms: number) => { now += ms; } };
}

test("opening a thread takes the request its tap started, once", async () => {
  const { prefetches, requests, start } = setup();
  start("t1", { threadRootId: "t1", limit: 50 }, "before the tap");
  const taken = prefetches.take("t1", { threadRootId: "t1", limit: 50 });
  assert.ok(taken);
  assert.equal(await taken.page, "page of t1");
  assert.equal(taken.baseline, "before the tap", "the page applies against what was held when it was asked for");
  assert.equal(prefetches.take("t1", { threadRootId: "t1", limit: 50 }), undefined, "a later open asks again");
  assert.equal(requests.length, 1);
});

test("repeated taps share one request while it is fresh", () => {
  const { requests, start, tick } = setup();
  start("t1", { threadRootId: "t1", limit: 50 });
  tick(1_000);
  start("t1", { threadRootId: "t1", limit: 50 });
  assert.equal(requests.length, 1);
  tick(5_000);
  start("t1", { threadRootId: "t1", limit: 50 });
  assert.equal(requests.length, 2, "a stale request is asked again");
});

test("a request for other arguments, or a stale one, is not used", () => {
  const { prefetches, start, tick } = setup();
  start("t1", { threadRootId: "t1", limit: 50 });
  assert.equal(prefetches.take("t1", { threadRootId: "t1", sinceSeq: 0, limit: 50 }), undefined);
  start("t2", { threadRootId: "t2", limit: 50 });
  tick(6_000);
  assert.equal(prefetches.take("t2", { threadRootId: "t2", limit: 50 }), undefined);
});

test("an unused prefetch that fails does not surface", async () => {
  const prefetches = new ThreadPagePrefetches<string, string>(() => 0);
  let unhandled = 0;
  const onUnhandled = () => { unhandled += 1; };
  process.on("unhandledRejection", onUnhandled);
  prefetches.start("t1", {}, () => "", () => Promise.reject(new Error("offline")));
  await new Promise((resolve) => setTimeout(resolve, 10));
  process.off("unhandledRejection", onUnhandled);
  assert.equal(unhandled, 0);
  await assert.rejects(prefetches.take("t1", {})!.page, /offline/, "whoever opens the thread still sees the failure");
});
