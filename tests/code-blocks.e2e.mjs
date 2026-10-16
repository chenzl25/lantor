// Production-bundle regression for highlighted code blocks: all API calls are
// mocked; no user data is read or changed. Run after `npm run build` with
// `npm run test:code-blocks`.
import assert from "node:assert/strict";
import { chromium } from "playwright";
import { build, preview } from "vite";

const graph = await build({ logLevel: "silent", build: { write: false } });
const chunks = new Map(graph.output.filter((output) => output.type === "chunk").map((chunk) => [chunk.fileName, chunk]));
const eager = new Set();
function visitChunk(name) {
  if (eager.has(name)) return;
  eager.add(name);
  chunks.get(name)?.imports.forEach(visitChunk);
}
for (const chunk of chunks.values()) if (chunk.isEntry) visitChunk(chunk.fileName);
const shikiModule = /\/node_modules\/(?:shiki|@shikijs|oniguruma-to-es|regex|regex-recursion|regex-utilities)\//;
assert.ok([...chunks.values()].some((chunk) => chunk.moduleIds.some((id) => shikiModule.test(id))));
for (const name of eager) {
  assert.ok(!chunks.get(name)?.moduleIds.some((id) => shikiModule.test(id)), `highlighter dependency in eager chunk ${name}`);
}
console.log("PASS bundle graph: Shiki and its grammars are exclusively lazy");

const channelId = "00000000-0000-4000-8000-000000000001";
const rustCode = "pub fn matches(&self) -> Result<bool> {\n    // comment\n\n    Ok(true)\n}";
const diffCode = "@@ -10,3 +10,3 @@ impl Strategy\n     let merge = true;\n-    let delete = false;\n+    let delete = !filters.is_empty();\n     merge";
const longCode = Array.from({ length: 45 }, (_, index) => `line ${index + 1}`).join("\n");
const codeBody = [
  "Key code:",
  `\`\`\`rust path=core/src/file_selection/benefit.rs#L80-L84\n${rustCode}\n\`\`\``,
  `\`\`\`diff rust path=https://github.com/o/r/blob/abc1234/core/src/strategy.rs\n${diffCode}\n\`\`\``,
  `\`\`\`text\n${longCode}\n\`\`\``,
  "After code.",
].join("\n\n");
const plainBody = "Plain message with `inline` code and an untagged block.\n\n```\nno language\n```";
const message = (body, deliveryState = "complete") => ({
  id: "00000000-0000-4000-8000-000000000002", seq: 1,
  channel_id: channelId, thread_root_id: null, sender_agent_id: null,
  sender_name: "Test owner", sender_role: "owner", body, is_task: false,
  thread_followed: false, delivery_state: deliveryState, stream_key: "",
  task_number: null, task_status: null, attachments: [], artifacts: [],
  created_at: "2026-01-01T00:00:00Z", updated_at: "2026-01-01T00:00:00Z",
});
const bootstrap = (body) => ({
  db_url: "synthetic://code-blocks", web_base_url: null,
  owner_profile: { display_name: "Test owner", avatar: "T", description: "" },
  channels: [{ id: channelId, name: "code-test", description: "", kind: "channel",
    dm_agent_id: null, unread_count: 0, github_unread_count: 0, github_review_synced_at: null }],
  messages: [message(body)], channel_message_history: [{ channel_id: channelId, before_seq: null, has_more: false }],
  thread_activities: [], channel_members: [], agents: [], saved_messages: [],
  dismissed_inbox_items: {}, read_inbox_items: {}, artifacts: [], tasks: [], reminders: [],
  agent_runs: [], agent_work_items: [], agent_activities: [],
  supervisor: { pid: null, status: "stopped", updated_at: null },
  launch_agent: { label: "", plist_path: "", installed: false, loaded: false }, ui_event_cursor: 0,
});
const highlighterResource = (url) => /\/code-highlighter-[^/]+\.js$/.test(new URL(url).pathname);
const server = await preview({ preview: { host: "127.0.0.1", port: 0, strictPort: true } });
const origin = `http://127.0.0.1:${server.httpServer.address().port}`;
let browser;
try {
  browser = await chromium.launch({ headless: true });
  async function pageFixture(body, onHighlighterRequest) {
    const context = await browser.newContext({ serviceWorkers: "block", viewport: { width: 1100, height: 1400 } });
    const page = await context.newPage();
    const requests = [];
    const errors = [];
    page.on("request", (request) => requests.push(request.url()));
    page.on("pageerror", (error) => errors.push(error.message));
    await page.addInitScript(() => {
      window.__codeEventSources = [];
      window.EventSource = class extends EventTarget {
        constructor() { super(); window.__codeEventSources.push(this); }
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
      if (highlighterResource(url.href) && onHighlighterRequest) return onHighlighterRequest(route);
      return route.continue();
    });
    async function update(body, deliveryState = "complete") {
      await page.evaluate((payload) => {
        for (const source of window.__codeEventSources) {
          source.dispatchEvent(new MessageEvent("lantor", { data: JSON.stringify(payload) }));
        }
      }, { type: "message_upsert", message: message(body, deliveryState) });
    }
    return { context, page, requests, errors, update };
  }

  const normal = await pageFixture(plainBody);
  await normal.page.goto(origin, { waitUntil: "networkidle" });
  await normal.page.locator(".markdown-body").filter({ hasText: "Plain message" }).waitFor();
  assert.deepEqual(normal.requests.filter(highlighterResource), [], "untagged code must not load the highlighter");

  // Streaming fences stay plain and expanded; completion highlights and collapses them.
  await normal.update(codeBody, "streaming");
  await normal.page.locator(".markdown-body").filter({ hasText: "After code." }).waitFor();
  assert.equal(await normal.page.locator("pre.is-highlighted").count(), 0);
  assert.equal(await normal.page.locator(".code-block-expand").count(), 0);
  await normal.update(codeBody);
  await normal.page.locator("pre.is-highlighted").nth(1).waitFor();
  const blocks = normal.page.locator(".markdown-body .code-block-shell");
  assert.equal(await blocks.count(), 3);

  const rust = blocks.nth(0);
  assert.equal(await rust.locator(".code-block-title").innerText(), "core/src/file_selection/benefit.rs");
  assert.equal(await rust.locator(".code-block-range").innerText(), "L80-L84");
  assert.equal((await rust.locator("pre").innerText()).replace(/\n$/, ""), rustCode, "rendered rows preserve the source text");
  assert.deepEqual(
    await rust.locator(".code-gutter").evaluateAll((nodes) => nodes.map((node) => getComputedStyle(node, "::before").content)),
    ['"80"', '"81"', '"82"', '"83"', '"84"'],
  );
  const keywordColor = await rust.locator("span", { hasText: /^fn$/ }).first().evaluate((node) => getComputedStyle(node).color);
  const plainColor = await rust.locator("pre").evaluate((node) => getComputedStyle(node).color);
  assert.notEqual(keywordColor, plainColor, "keywords use a syntax token color");
  const lineHeight = await rust.locator(".code-line").first().evaluate((node) => node.getBoundingClientRect().height);
  const emptyLine = await rust.locator(".code-line").nth(2).evaluate((node) => node.getBoundingClientRect().height);
  assert.ok(Math.abs(lineHeight - emptyLine) < 0.5, "empty rows keep their line height");
  const codeHeight = await rust.locator("pre code").evaluate((node) => node.getBoundingClientRect().height);
  assert.ok(Math.abs(codeHeight - lineHeight * 5) < 1, "rows do not add blank lines");

  const diff = blocks.nth(1);
  assert.equal(await diff.locator("a.code-block-title").getAttribute("href"), "https://github.com/o/r/blob/abc1234/core/src/strategy.rs");
  assert.deepEqual(await diff.locator(".code-line").evaluateAll((nodes) => nodes.map((node) => node.className)), [
    "code-line diff-hunk", "code-line diff-context", "code-line diff-del", "code-line diff-add", "code-line diff-context",
  ]);
  assert.deepEqual(
    await diff.locator(".code-gutter").evaluateAll((nodes) => nodes.map((node) => [
      getComputedStyle(node, "::before").content, getComputedStyle(node, "::after").content,
    ])),
    [['""', '""'], ['"10"', '"10"'], ['"11"', '""'], ['""', '"11"'], ['"12"', '"12"']],
  );
  assert.ok(await diff.locator(".diff-add span[style*='--shiki-token-keyword']").count() > 0, "diff body is highlighted as Rust");
  const [addBackground, contextBackground] = await Promise.all([
    diff.locator(".diff-add").evaluate((node) => getComputedStyle(node).backgroundColor),
    diff.locator(".diff-context").first().evaluate((node) => getComputedStyle(node).backgroundColor),
  ]);
  assert.notEqual(addBackground, contextBackground);

  const codeTheme = () => rust.locator("pre").evaluate((node) => {
    const keyword = node.querySelector("span[style*='--shiki-token-keyword']");
    const [r, g, b] = getComputedStyle(node).backgroundColor.match(/\d+/g).map(Number);
    return { luminance: (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255, keyword: getComputedStyle(keyword).color };
  });
  const setTheme = (theme) => normal.page.evaluate((value) => { document.documentElement.dataset.theme = value; }, theme);
  await setTheme("light");
  const lightCode = await codeTheme();
  await setTheme("dark");
  const darkCode = await codeTheme();
  await setTheme("light");
  assert.ok(lightCode.luminance > 0.9, `light theme code surface is light (${lightCode.luminance})`);
  assert.ok(darkCode.luminance < 0.25, `dark theme code surface is dark (${darkCode.luminance})`);
  assert.notEqual(lightCode.keyword, darkCode.keyword, "syntax palette follows the theme");

  const long = blocks.nth(2);
  const expand = long.getByRole("button", { name: "Show all 45 lines" });
  const collapsedHeight = await long.locator("pre code").evaluate((node) => node.getBoundingClientRect().height);
  const longLine = await long.locator("pre code").evaluate((node) => parseFloat(getComputedStyle(node).lineHeight));
  assert.ok(Math.abs(collapsedHeight - longLine * 20) < 1, `collapsed to 20 lines (${collapsedHeight})`);
  await expand.click();
  await long.getByRole("button", { name: "Show less" }).waitFor();
  const expandedHeight = await long.locator("pre code").evaluate((node) => node.getBoundingClientRect().height);
  assert.ok(Math.abs(expandedHeight - longLine * 45) < 1, `expanded to 45 lines (${expandedHeight})`);
  assert.equal(normal.requests.filter(highlighterResource).length, 1);
  assert.deepEqual(normal.errors, []);
  if (process.env.CODE_BLOCKS_SCREENSHOT) {
    await long.getByRole("button", { name: "Show less" }).click();
    const showMore = normal.page.getByRole("button", { name: "Show more" });
    if (await showMore.count()) await showMore.click();
    await normal.page.screenshot({ path: process.env.CODE_BLOCKS_SCREENSHOT, fullPage: true });
    await setTheme("dark");
    await normal.page.mouse.move(0, 0);
    await normal.page.waitForTimeout(500);
    await normal.page.screenshot({ path: process.env.CODE_BLOCKS_SCREENSHOT.replace(/(\.png)?$/, "-dark.png"), fullPage: true });
  }
  console.log("PASS code blocks: lazy highlighting, header/line numbers, diff rows, theme palette, collapse and streaming");
  await normal.context.close();

  const failed = await pageFixture(codeBody, (route) => route.abort());
  await failed.page.goto(origin, { waitUntil: "networkidle" });
  await failed.page.locator(".markdown-body").filter({ hasText: "After code." }).waitFor();
  assert.equal(await failed.page.locator("pre.is-highlighted").count(), 0);
  assert.equal((await failed.page.locator(".code-block-shell").first().locator("pre").innerText()).replace(/\n$/, ""), rustCode);
  assert.equal(await failed.page.locator(".code-line.diff-add").count(), 1, "diff rows do not depend on the highlighter");
  assert.deepEqual(failed.errors, []);
  console.log("PASS failed highlighter chunk: code stays readable as plain text");
  await failed.context.close();
} finally {
  await browser?.close();
  await new Promise((resolve, reject) => server.httpServer.close((error) => error ? reject(error) : resolve()));
}
