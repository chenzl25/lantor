// Invoked by the Rust fixture: real Axum route, fresh child using supervisor's
// runtime writer, and native Chromium EventSource. No live app data or daemon.
import assert from "node:assert/strict";
import { chromium } from "playwright";

const base = process.argv[2];
assert.ok(base?.startsWith("http://127.0.0.1:"));
const browser = await chromium.launch({ headless: true });
const results = [];
const readMetrics = async () => (await fetch(`${base}/__test__/metrics`)).json();
let lastCursor;
try {
  for (const tabs of [1, 3]) {
    const pages = [];
    for (let index = 0; index < tabs; index++) {
      const page = await browser.newPage();
      page.setDefaultTimeout(5000);
      await page.goto(`${base}/__test__/blank`);
      await page.evaluate(async () => {
        window.deliveries = [];
        window.source = new EventSource("/api/events");
        source.addEventListener("lantor", (event) => {
          const value = JSON.parse(event.data);
          if (value.type === "message_delta") {
            const sample = JSON.parse(value.append);
            deliveries.push({ cursor: Number(event.lastEventId), sample: sample.sample, latency: Date.now() - sample.sent_at_ms });
          }
        });
        await new Promise((resolve, reject) => { source.onopen = resolve; source.onerror = reject; });
      });
      pages.push(page);
    }
    await pages[0].waitForTimeout(150);
    const idleStart = await readMetrics();
    const idleStartedAt = Date.now();
    await pages[0].waitForTimeout(1000);
    const before = await readMetrics();
    const idleWindowMs = Date.now() - idleStartedAt;
    assert.equal(before.event_table_queries, idleStart.event_table_queries, "idle tabs do not query ui_events");
    assert.ok(before.pragma_reads > idleStart.pragma_reads, "one metadata observer remains alive");
    const write = await fetch(`${base}/__test__/write`, { method: "POST" });
    assert.equal(write.status, 200);
    for (const page of pages) await page.waitForFunction(() => deliveries.length === 25, null, { timeout: 5000 });
    const copies = await Promise.all(pages.map((page) => page.evaluate(() => deliveries)));
    const after = await readMetrics();
    for (const copy of copies) {
      assert.deepEqual(copy.map((item) => item.sample), Array.from({ length: 25 }, (_, index) => index));
      assert.equal(new Set(copy.map((item) => item.cursor)).size, 25);
      assert.deepEqual(copy.map((item) => item.cursor), copies[0].map((item) => item.cursor));
    }
    lastCursor = copies[0].at(-1).cursor;
    const latencies = copies.flat().map((item) => item.latency).sort((a, b) => a - b);
    const queryCount = after.event_table_queries - before.event_table_queries;
    assert.ok(queryCount <= 52, `shared read query count: ${queryCount}`);
    const result = { tabs, idleWindowMs, samples: latencies.length, idleEventTableQueries: 0,
      idlePragmaChecks: before.pragma_reads - idleStart.pragma_reads,
      eventTableQueries: queryCount, medianMs: latencies[Math.floor(latencies.length / 2)],
      p95Ms: latencies[Math.floor(latencies.length * 0.95)], maxMs: latencies.at(-1) };
    assert.ok(result.maxMs < 100, `cross-process write-to-browser latency: ${JSON.stringify(result)}`);
    results.push(result);
    for (const page of pages) await page.close();
  }
  assert.ok(results[1].eventTableQueries <= results[0].eventTableQueries + 2, "three tabs must not triple DB reads");
  async function firstEvent(cursor) {
    const response = await fetch(`${base}/api/events?cursor=0`, { headers: { "Last-Event-ID": String(cursor) } });
    const reader = response.body.getReader();
    let buffer = "";
    try {
      while (true) {
        const { value, done } = await reader.read();
        assert.ok(!done, "SSE response ended early");
        buffer += new TextDecoder().decode(value);
        const blocks = buffer.split("\n\n");
        buffer = blocks.pop();
        for (const block of blocks) {
          const data = block.split("\n").find((line) => line.startsWith("data: "));
          if (data) return { id: Number(block.split("\n").find((line) => line.startsWith("id: "))?.slice(4)), event: JSON.parse(data.slice(6)) };
        }
      }
    } finally { await reader.cancel(); }
  }
  const replay = await firstEvent(lastCursor - 1);
  assert.equal(replay.id, lastCursor);
  assert.equal(replay.event.type, "message_delta", "Last-Event-ID overrides query cursor");
  await fetch(`${base}/__test__/prune`, { method: "POST" });
  const gap = await firstEvent(0);
  assert.equal(gap.id, lastCursor);
  assert.equal(gap.event.reason, "event_replay_gap");
  console.log(JSON.stringify({ results, checks: "committed cross-process deltas, idle no table reads, shared fan-out, ordered unique cursors, Last-Event-ID precedence/replay, pruned gap refresh" }));
} finally {
  await browser.close();
}
