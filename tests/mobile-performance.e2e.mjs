// Production mobile UI against an isolated API; never mutates the operator DB.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { chromium, webkit, devices } from "playwright";

const id = n => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const now = new Date().toISOString();
const channels = [1, 2].map(n => ({ id: id(n), name: `mobile-${n}`, description: "", kind: "channel", dm_agent_id: null,
  unread_count: 0, github_unread_count: 0, github_review_synced_at: null, latest_message_at: now }));
const roots = Array.from({ length: 80 }, (_, n) => ({ id: id(n + 10), seq: n + 10, channel_id: id(2), thread_root_id: null,
  sender_agent_id: null, sender_name: "Owner", sender_role: "owner", body: `Root ${n + 1}\n\nMobile history fixture.`,
  is_task: false, thread_followed: false, delivery_state: "complete", stream_key: "", task_number: null, task_status: null,
  attachments: [], artifacts: [], created_at: now, updated_at: now }));
const latest = roots.at(-1);
latest.attachments = [{ id: id(500), message_id: latest.id, original_name: "fixture.png", mime_type: "image/png", size_bytes: 1024000, storage_path: "/synthetic/fixture.png", created_at: now }];
const reply = { ...latest, id: id(900), seq: 900, thread_root_id: latest.id, attachments: [], body: "Lazy full reply" };
const activity = { thread_root_id: latest.id, channel_id: id(2), reply_count: 1, unread_count: 0, latest_message_id: reply.id, latest_activity_at: now };
const state = { db_url: "synthetic://mobile", web_base_url: null, owner_profile: { display_name: "Owner", avatar: "O", description: "" },
  channels, messages: [], channel_message_history: [{ channel_id: id(1), before_seq: null, has_more: false }], thread_activities: [],
  channel_members: [], agents: [], saved_messages: [], dismissed_inbox_items: {}, read_inbox_items: {}, artifacts: [], tasks: [], reminders: [],
  agent_schedules: [], agent_runs: [], agent_work_items: [], agent_activities: [], supervisor: { pid: null, status: "stopped", updated_at: null },
  launch_agent: { label: "", plist_path: "", installed: false, loaded: false }, ui_event_cursor: 0 };
const requests = [], clients = new Set();
let cursor = 0;
const publish = event => { for (const client of clients) client.write(`id: ${++cursor}\nevent: lantor\ndata: ${JSON.stringify(event)}\n\n`); };
const png = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jr1kAAAAASUVORK5CYII=", "base64");
const api = createServer(async (request, response) => {
  const url = new URL(request.url, "http://localhost");
  if (!url.pathname.startsWith("/api/")) {
    const file = new URL(`../dist/${url.pathname === "/" ? "index.html" : url.pathname.slice(1)}`, import.meta.url);
    if (!file.href.startsWith(new URL("../dist/", import.meta.url).href)) { response.writeHead(404).end(); return; }
    try { const bytes = await readFile(file); response.writeHead(200, { "content-type": ({ js: "application/javascript", css: "text/css", html: "text/html", png: "image/png", woff2: "font/woff2" })[file.pathname.split(".").pop()] ?? "application/octet-stream" }); response.end(bytes); }
    catch { response.writeHead(404).end(); } return;
  }
  if (url.pathname === "/api/events") { response.writeHead(200, { "content-type": "text/event-stream" }); response.write(": ready\n\n"); clients.add(response); request.on("close", () => clients.delete(response)); return; }
  const chunks = []; for await (const chunk of request) chunks.push(chunk);
  const args = chunks.length ? JSON.parse(Buffer.concat(chunks)) : {};
  requests.push({ path: url.pathname, args, time: Date.now(), search: url.search });
  if (url.pathname.startsWith("/api/attachments/")) { response.writeHead(200, { "content-type": "image/png" }); response.end(png); return; }
  let result = { ok: true };
  if (url.pathname === "/api/bootstrap") result = state;
  if (url.pathname === "/api/load_channel_previews") result = [];
  if (url.pathname === "/api/load_ui_state") result = Object.fromEntries(args.scopes.map(scope => [scope, scope === "thread_activities" ? [activity] : state[scope]]));
  if (url.pathname === "/api/load_channel_messages" || url.pathname === "/api/load_older_channel_messages") {
    const eligible = roots.filter(m => m.seq < (args.beforeSeq ?? Infinity));
    const page = eligible.slice(-args.limit);
    result = { messages: page, has_more: eligible.length > page.length, next_before_seq: page[0]?.seq ?? null, thread_activities: page.includes(latest) ? [activity] : [] };
  }
  if (url.pathname === "/api/load_thread_messages") result = [latest, reply];
  if (url.pathname === "/api/replay_ui_events") result = { cursor, replayGap: false, events: [] };
  response.writeHead(200, { "content-type": "application/json" }); response.end(JSON.stringify(result));
});
await new Promise(resolve => api.listen(0, "127.0.0.1", resolve));
try {
  for (const engine of [chromium, webkit]) {
    requests.length = 0; cursor = 0;
    const browser = await engine.launch({ headless: true });
    try {
      const context = await browser.newContext({ ...devices["iPhone 13"] });
      const page = await context.newPage(); page.setDefaultTimeout(12000);
      if (engine === chromium) await (await context.newCDPSession(page)).send("Emulation.setCPUThrottlingRate", { rate: 4 });
      const errors = []; page.on("pageerror", e => errors.push(e.message));
      // Track actual global registrations, then verify non-edge gestures never
      // install an active move listener. Native touch scrolling stays passive.
      await page.addInitScript(() => {
        const add = window.addEventListener, remove = window.removeEventListener;
        window.__moves = new Set(); window.__activeStarts = new Set();
        window.addEventListener = function(type, listener, options) {
          if (type === "touchmove" && options?.passive === false) window.__moves.add(listener);
          if (type === "touchstart" && options?.passive === false) window.__activeStarts.add(listener);
          return add.call(this, type, listener, options);
        };
        window.removeEventListener = function(type, listener, options) {
          if (type === "touchmove") window.__moves.delete(listener);
          if (type === "touchstart") window.__activeStarts.delete(listener);
          return remove.call(this, type, listener, options);
        };
      });
      await page.goto(`http://127.0.0.1:${api.address().port}`);
      await page.getByRole("button", { name: "mobile-2", exact: true }).click();
      await page.locator(`.message-list [data-message-id="${latest.id}"]`).waitFor();
      await page.waitForTimeout(800);
      const loads = requests.filter(r => r.path === "/api/load_channel_messages");
      assert.equal(loads.length, 1); assert.equal(loads[0].args.limit, 30); assert.equal(loads[0].args.rootsOnly, true);
      assert.equal(requests.filter(r => r.path === "/api/load_channel_previews").length, 0);
      assert.equal(requests.filter(r => r.path === "/api/load_thread_messages").length, 0);
      assert.equal(await page.locator(".message-list [data-message-id]").count(), 30);
      assert.equal(await page.locator('.message-attachments img').first().getAttribute("src"), `/api/attachments/${id(500)}?w=480`);
      assert.equal(await page.evaluate(() => window.__activeStarts.size), 0);
      assert.equal(await page.evaluate(() => window.__moves.size), 0);
      for (const x of [150, 5]) {
        await page.evaluate(x => {
          const target = document.querySelector(".message-list");
          const event = new Event("touchstart", { bubbles: true, cancelable: true });
          Object.defineProperty(event, "touches", { value: [{ clientX: x, clientY: 300 }] }); target.dispatchEvent(event);
        }, x);
        assert.equal(await page.evaluate(() => window.__moves.size), x === 5 ? 1 : 0);
        await page.evaluate(() => window.dispatchEvent(new Event("touchcancel")));
        assert.equal(await page.evaluate(() => window.__moves.size), 0);
      }
      await page.locator(`[data-message-id="${latest.id}"]`).getByRole("button", { name: "View 1 reply in thread", exact: true }).click();
      await page.getByText("Lazy full reply", { exact: true }).waitFor();
      await page.getByRole("button", { name: "Back to channel", exact: true }).click();
      await page.waitForTimeout(700);
      const mark = requests.length;
      for (let i = 0; i < 30; i++) { publish({ type: "refresh", reason: "message" }); await page.waitForTimeout(50); }
      await page.waitForTimeout(800);
      const refreshes = requests.slice(mark).filter(r => r.path === "/api/load_ui_state");
      assert.ok(refreshes.length >= 2 && refreshes.length <= 5, `burst should coalesce, got ${refreshes.length}`);
      for (let i = 1; i < refreshes.length; i++) assert.ok(refreshes[i].time - refreshes[i - 1].time >= 450);
      await page.locator(".message-list").evaluate(node => { node.scrollTop = 0; node.dispatchEvent(new Event("scroll")); });
      await page.waitForTimeout(800);
      const older = requests.filter(r => r.path === "/api/load_older_channel_messages");
      assert.equal(older.length, 1); assert.equal(older[0].args.limit, 30); assert.equal(older[0].args.rootsOnly, true);
      assert.deepEqual(errors, []);
      console.log(`${engine.name()}: mobile roots/detail, no previews, passive gesture, thumbnail URL, 2Hz refresh and history passed`);
    } finally { await browser.close(); }
  }
} finally { for (const client of clients) client.end(); await new Promise(resolve => api.close(resolve)); }
