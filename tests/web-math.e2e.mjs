// Production-bundle regression: all API calls are mocked; no user data is read
// or changed. Run after `npm run build` with `npm run test:web-math`.
import assert from "node:assert/strict";
import { chromium } from "playwright";
import { build, preview } from "vite";

// Audit Rollup's actual module graph, not minified names or a size heuristic.
// write:false leaves the production dist (and its compressed sidecars) intact.
const graph = await build({ logLevel: "silent", build: { write: false } });
const chunks = new Map(graph.output.filter((output) => output.type === "chunk").map((chunk) => [chunk.fileName, chunk]));
const eager = new Set();
function visitChunk(name) {
  if (eager.has(name)) return;
  eager.add(name);
  chunks.get(name)?.imports.forEach(visitChunk);
}
for (const chunk of chunks.values()) if (chunk.isEntry) visitChunk(chunk.fileName);
const mathModule = /\/node_modules\/(?:katex|remark-math|rehype-katex|micromark-extension-math|mdast-util-math)\//;
assert.ok([...chunks.values()].some((chunk) => chunk.moduleIds.some((id) => mathModule.test(id))));
for (const name of eager) {
  assert.ok(!chunks.get(name)?.moduleIds.some((id) => mathModule.test(id)), `math dependency in eager chunk ${name}`);
}
console.log("PASS bundle graph: KaTeX, CSS and math plugins are exclusively lazy");

const channelId = "00000000-0000-4000-8000-000000000001";
const plainBody = "Plain message: $5 and $10, @Hancock.\n\n| A | B |\n|---|---|\n| one | two |\n\n```js\nconst literal = '$$';\n```";
const formulaBody = "Formula message: $$x^2$$.\n\n$$\n\\sum_{i=1}^{n} i\n$$\n\n```math\n\\frac{a}{b}\n```\n\nAfter formula.";
const message = (body) => ({
  id: "00000000-0000-4000-8000-000000000002", seq: 1,
  channel_id: channelId, thread_root_id: null, sender_agent_id: null,
  sender_name: "Test owner", sender_role: "owner", body, is_task: false,
  thread_followed: false, delivery_state: "complete", stream_key: "",
  task_number: null, task_status: null, attachments: [], artifacts: [],
  created_at: "2026-01-01T00:00:00Z", updated_at: "2026-01-01T00:00:00Z",
});
const bootstrap = (body) => ({
  db_url: "synthetic://web-math", web_base_url: null,
  owner_profile: { display_name: "Test owner", avatar: "T", description: "" },
  channels: [{ id: channelId, name: "math-test", description: "", kind: "channel",
    dm_agent_id: null, unread_count: 0, github_unread_count: 0, github_review_synced_at: null }],
  messages: [message(body)], channel_message_history: [{ channel_id: channelId, before_seq: null, has_more: false }],
  thread_activities: [], channel_members: [], agents: [], saved_messages: [],
  dismissed_inbox_items: {}, read_inbox_items: {}, artifacts: [], tasks: [], reminders: [],
  agent_schedules: [], agent_runs: [], agent_work_items: [], agent_activities: [],
  supervisor: { pid: null, status: "stopped", updated_at: null },
  launch_agent: { label: "", plist_path: "", installed: false, loaded: false }, ui_event_cursor: 0,
});
const mathResource = (url) => /\/(?:MathMarkdown-[^/]+\.(?:js|css)|KaTeX_[^/]+)$/.test(new URL(url).pathname);
const server = await preview({ preview: { host: "127.0.0.1", port: 0, strictPort: true } });
const origin = `http://127.0.0.1:${server.httpServer.address().port}`;
let browser;
try {
  browser = await chromium.launch({ headless: true });
  async function pageFixture(body, onMathRequest) {
    const context = await browser.newContext();
    const page = await context.newPage();
    const requests = [];
    const errors = [];
    page.on("request", (request) => requests.push(request.url()));
    page.on("pageerror", (error) => errors.push(error.message));
    await page.addInitScript(() => {
      window.__mathEventSources = [];
      window.EventSource = class extends EventTarget {
        constructor() { super(); window.__mathEventSources.push(this); }
        close() {}
      };
    });
    await page.route("**/*", async (route) => {
      const url = new URL(route.request().url());
      if (url.origin !== origin) return route.abort();
      if (url.pathname.startsWith("/api/")) {
        let response = { ok: true };
        if (url.pathname === "/api/bootstrap") response = bootstrap(body);
        if (url.pathname === "/api/load_channel_messages") {
          response = { messages: [message(body)], next_before_seq: null, has_more: false };
        }
        return route.fulfill({ contentType: "application/json", body: JSON.stringify(response) });
      }
      if (mathResource(url.href) && onMathRequest) return onMathRequest(route);
      return route.continue();
    });
    async function update(body, deliveryState = "complete") {
      await page.evaluate((payload) => {
        for (const source of window.__mathEventSources) {
          source.dispatchEvent(new MessageEvent("lantor", { data: JSON.stringify(payload) }));
        }
      }, { type: "message_upsert", message: { ...message(body), delivery_state: deliveryState } });
    }
    return { context, page, requests, errors, update };
  }

  let release;
  const gate = new Promise((resolve) => { release = resolve; });
  const normal = await pageFixture(plainBody, async (route) => { await gate; await route.continue(); });
  await normal.page.goto(origin, { waitUntil: "networkidle" });
  await normal.page.locator(".markdown-body").filter({ hasText: "Plain message" }).waitFor();
  assert.equal(await normal.page.locator(".markdown-body table").count(), 1);
  assert.equal(await normal.page.getByRole("button", { name: "Copy code block" }).count(), 1);
  assert.equal(await normal.page.locator(".markdown-body .local-entity-link").count(), 1);
  assert.deepEqual(normal.requests.filter(mathResource), [], "plain channel must not request math JS/CSS/fonts");

  const requested = normal.page.waitForRequest((request) => /\/MathMarkdown-.*\.js$/.test(request.url()));
  await normal.update(formulaBody);
  await requested;
  // The lazy chunk is held: ordinary Markdown remains visible, not an empty row.
  await normal.page.locator(".markdown-body").filter({ hasText: "After formula." }).waitFor();
  assert.equal(await normal.page.locator(".katex").count(), 0);
  assert.ok((await normal.page.locator(".markdown-body").innerText()).includes("$$x^2$$"));
  release();
  await normal.page.waitForFunction(() => document.querySelectorAll(".katex").length === 3);
  await normal.page.evaluate(() => document.fonts.ready);
  assert.ok(normal.requests.some((url) => /\/MathMarkdown-.*\.css$/.test(url)));
  assert.ok(normal.requests.some((url) => /\/KaTeX_.*\.woff2$/.test(url)));
  assert.match(await normal.page.locator(".katex").first().evaluate((node) => getComputedStyle(node).fontFamily), /KaTeX/);
  await normal.update("Streaming\n\n$$\nx^2", "streaming");
  await normal.page.waitForFunction(() => document.querySelectorAll(".katex").length === 1);
  await normal.update(formulaBody);
  await normal.page.waitForFunction(() => document.querySelectorAll(".katex").length === 3);
  assert.equal(normal.requests.filter((url) => /\/MathMarkdown-.*\.js$/.test(url)).length, 1);
  assert.deepEqual(normal.errors, []);
  console.log("PASS plain channel: no math requests; lazy fallback, formulas, CSS/fonts and streaming render correctly");
  await normal.context.close();

  const failed = await pageFixture(formulaBody, (route) => route.abort());
  await failed.page.goto(origin, { waitUntil: "networkidle" });
  await failed.page.locator(".markdown-body").filter({ hasText: "After formula." }).waitFor();
  assert.equal(await failed.page.locator(".katex").count(), 0);
  assert.ok((await failed.page.locator(".markdown-body").innerText()).includes("$$x^2$$"));
  await failed.update("Still readable $$y^2$$ after a failed chunk request.");
  await failed.page.getByText("Still readable $$y^2$$ after a failed chunk request.").waitFor();
  assert.deepEqual(failed.errors, []);
  console.log("PASS failed math chunk: message and subsequent updates remain readable");
  await failed.context.close();
} finally {
  await browser?.close();
  await new Promise((resolve, reject) => server.httpServer.close((error) => error ? reject(error) : resolve()));
}
