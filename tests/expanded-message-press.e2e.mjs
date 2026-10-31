// Production App against an isolated API: press text in an expanded long message.
// Run npm run build first. No live workspace data is read or mutated.
//
// Chromium kills the renderer (Aw, Snap! error code 5) when a message row switches
// content-visibility while it handles a mousedown in that row. Show more leaves
// focus on its button inside the row, and the press moves focus out, so row
// containment must not depend on :focus-within.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { chromium } from "playwright";

const id = n => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const now = new Date().toISOString(), channelId = id(1000);
const longBody = Array.from({ length: 40 }, (_, n) => `Paragraph ${n} of a long message that collapses behind Show more.`).join("\n\n");
const message = (n, extra = {}) => ({ id: id(n), seq: n, channel_id: channelId, thread_root_id: null, sender_agent_id: null,
  sender_name: "Dylan", sender_role: "owner", body: `Discussion ${n}`, is_task: false, thread_followed: false,
  delivery_state: "complete", stream_key: "", task_number: null, task_status: null, attachments: [], artifacts: [], created_at: now, updated_at: now, ...extra });
const channelLong = message(1, { body: longBody }), root = message(2), threadLong = message(3, { thread_root_id: root.id, body: longBody });
const channels = [{ id: channelId, name: "ui-review", kind: "channel", dm_agent_id: null, description: "", unread_count: 0, github_unread_count: 0, github_review_synced_at: null }];
const state = { db_url: "synthetic://expanded-message-press", web_base_url: null, owner_profile: { display_name: "Dylan", avatar: "D", description: "" },
  channels, messages: [channelLong, root, threadLong], channel_message_history: [{ channel_id: channelId, before_seq: null, has_more: false }],
  agents: [], tasks: [], channel_members: [], thread_activities: [], saved_messages: [], dismissed_inbox_items: {}, read_inbox_items: {},
  artifacts: [], reminders: [], agent_runs: [], agent_work_items: [], agent_activities: [], supervisor: { pid: null, status: "stopped", updated_at: null },
  launch_agent: { label: "", plist_path: "", installed: false, loaded: false }, ui_event_cursor: 0 };

const dist = resolve("dist");
const api = createServer(async (req, res) => {
  const url = new URL(req.url, "http://localhost");
  if (!url.pathname.startsWith("/api/")) {
    const file = resolve(dist, url.pathname === "/" ? "index.html" : url.pathname.slice(1));
    if (!file.startsWith(dist + "/")) { res.writeHead(404).end(); return; }
    try { const mime = { html: "text/html", js: "application/javascript", css: "text/css", png: "image/png", woff: "font/woff", woff2: "font/woff2" };
      res.writeHead(200, { "content-type": mime[file.split(".").pop()] || "application/octet-stream" }).end(await readFile(file));
    } catch { res.writeHead(404).end(); }
    return;
  }
  if (url.pathname === "/api/events") { res.writeHead(200, { "content-type": "text/event-stream" }); res.write(": ready\n\n"); return; }
  let raw = ""; for await (const chunk of req) raw += chunk;
  const args = raw ? JSON.parse(raw) : {};
  let result = { ok: true };
  switch (url.pathname) {
    case "/api/bootstrap": result = state; break;
    case "/api/load_channel_previews": case "/api/load_activity_messages": case "/api/search_messages": result = []; break;
    case "/api/load_channel_messages": result = { messages: state.messages.filter(m => m.channel_id === args.channelId && !m.thread_root_id), next_before_seq: null, has_more: false }; break;
    case "/api/load_thread_messages": result = state.messages.filter(m => m.id === args.threadRootId || m.thread_root_id === args.threadRootId); break;
    case "/api/load_ui_state": result = Object.fromEntries(args.scopes.map(scope => [scope, state[scope]])); break;
    case "/api/replay_ui_events": result = { cursor: 0, replayGap: false, events: [] }; break;
  }
  res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(result));
});
await new Promise(done => api.listen(0, "127.0.0.1", done));
let browser;
try {
  browser = await chromium.launch();
  for (const surface of ["channel", "thread"]) {
    const context = await browser.newContext({ serviceWorkers: "block", viewport: { width: 1440, height: 900 } });
    const page = await context.newPage(); page.setDefaultTimeout(8000);
    let crashed = false; page.on("crash", () => { crashed = true; });
    const errors = []; page.on("pageerror", error => errors.push(error.message));
    await page.goto(`http://127.0.0.1:${api.address().port}`, { waitUntil: "domcontentloaded" });
    if (surface === "thread") {
      const rootRow = page.locator(`.conversation [data-message-id="${root.id}"]`);
      await rootRow.hover();
      await rootRow.getByRole("button", { name: "View replies", exact: true }).click();
    }
    const row = page.locator(`${surface === "channel" ? ".conversation" : ".thread"} [data-message-id="${surface === "channel" ? channelLong.id : threadLong.id}"]`);
    await row.getByRole("button", { name: "Show more", exact: true }).click();
    await row.locator(".message-long-preview.collapsed").waitFor({ state: "detached" });
    assert.ok(await row.evaluate(node => node.contains(document.activeElement)), `${surface}: Show less keeps focus in the row`);
    const containment = await row.evaluate(node => CSS.supports("selector(:has(*))") && CSS.supports("overflow-clip-margin", "100px") ? getComputedStyle(node).contentVisibility : "unsupported");
    assert.equal(containment, "auto", `${surface}: a focused row keeps content-visibility: auto`);
    await page.waitForTimeout(500);
    // Scroll back up to the start of the expanded message, as a reader does.
    const paragraph = row.locator(".markdown-body p").first();
    await paragraph.scrollIntoViewIfNeeded();
    const text = await paragraph.boundingBox();
    await Promise.race([
      (async () => { await page.mouse.move(text.x + text.width * 0.3, text.y + text.height / 2); await page.mouse.down(); await page.mouse.up(); })(),
      new Promise(done => setTimeout(done, 3000)),
    ]);
    await page.waitForTimeout(500);
    assert.equal(crashed, false, `${surface}: pressing text in an expanded message keeps the renderer alive`);
    assert.equal(await row.evaluate(node => node.contains(document.activeElement)), false, `${surface}: the press moved focus out of the row`);
    assert.deepEqual(errors, []);
    await context.close();
  }
  console.log(JSON.stringify({ engine: "chromium", version: browser.version(), checks: "channel and thread: expand, press text, renderer alive" }));
} finally {
  await browser?.close();
  api.close();
}
