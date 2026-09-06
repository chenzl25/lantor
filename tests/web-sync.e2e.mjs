// Production UI + real EventSource, isolated synthetic HTTP API. No user data.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { chromium } from "playwright";
import { readFile } from "node:fs/promises";

const id = (n) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const channelId = id(1);
const now = new Date().toISOString();
let seq = 1;
const message = (body, extra = {}) => ({ id: id(++seq), seq, channel_id: channelId, thread_root_id: null,
  sender_agent_id: null, sender_name: "Test owner", sender_role: "owner", body, is_task: false,
  thread_followed: false, delivery_state: "complete", stream_key: "", task_number: null, task_status: null,
  attachments: [], artifacts: [], created_at: now, updated_at: now, ...extra });
const root = message("Sync task", { is_task: true, task_number: 1, task_status: "todo" });
const state = {
  db_url: "synthetic://web-sync", web_base_url: null,
  owner_profile: { display_name: "Test owner", avatar: "T", description: "" },
  channels: [{ id: channelId, name: "sync-test", description: "", kind: "channel", dm_agent_id: null,
    unread_count: 1, github_unread_count: 0, github_review_synced_at: null }],
  messages: [root], channel_message_history: [{ channel_id: channelId, before_seq: null, has_more: false }],
  thread_activities: [], channel_members: [], agents: [], saved_messages: [], dismissed_inbox_items: {},
  read_inbox_items: {}, artifacts: [], reminders: [], agent_schedules: [], agent_runs: [], agent_work_items: [], agent_activities: [],
  tasks: [{ id: id(900), number: 1, message_id: root.id, channel_id: channelId, title: "Sync task", status: "todo",
    version: 1, channel_name: "sync-test", assignee_id: null, assignee_name: null, created_at: now, updated_at: now }],
  supervisor: { pid: null, status: "stopped", updated_at: null },
  launch_agent: { label: "", plist_path: "", installed: false, loaded: false }, ui_event_cursor: 0,
};
const events = [];
const clients = new Set();
const requests = [];
let disconnected = false;
let holdTaskPatch = null;
let taskPatchHeld = false;
const sse = (delivery) => `id: ${delivery.cursor}\nevent: lantor\ndata: ${delivery.event}\n\n`;
function publish(event) {
  const delivery = { cursor: events.length + 1, event: JSON.stringify(event) };
  events.push(delivery);
  for (const client of clients) client.write(sse(delivery));
  return delivery;
}
function incoming(body) {
  const row = message(body, { sender_role: "system" });
  state.messages.push(row);
  state.channels[0].unread_count++;
  publish({ type: "message_upsert", message: row });
  return row;
}
const api = createServer(async (request, response) => {
  const url = new URL(request.url, "http://localhost");
  if (!url.pathname.startsWith("/api/")) {
    const file = new URL(`../dist/${url.pathname === "/" ? "index.html" : url.pathname.slice(1)}`, import.meta.url);
    if (!file.href.startsWith(new URL("../dist/", import.meta.url).href)) { response.writeHead(404); response.end(); return; }
    try {
      const bytes = await readFile(file);
      const mime = { js: "application/javascript", css: "text/css", html: "text/html", png: "image/png", woff: "font/woff", woff2: "font/woff2" };
      response.writeHead(200, { "content-type": mime[file.pathname.split(".").pop()] ?? "application/octet-stream" });
      response.end(bytes);
    } catch { response.writeHead(404); response.end(); }
    return;
  }
  requests.push({ path: url.pathname, time: Date.now(), cursor: url.searchParams.get("cursor") });
  if (url.pathname === "/api/events") {
    if (disconnected) { response.writeHead(503); response.end(); return; }
    response.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
    response.write(": ready\n\n");
    for (const event of events) if (event.cursor > Number(url.searchParams.get("cursor") ?? 0)) response.write(sse(event));
    clients.add(response);
    response.on("close", () => clients.delete(response));
    return;
  }
  let raw = "";
  for await (const chunk of request) raw += chunk;
  const args = raw ? JSON.parse(raw) : {};
  let result = { ok: true };
  switch (url.pathname) {
    case "/api/bootstrap": result = { ...state, ui_event_cursor: events.length }; break;
    case "/api/load_channel_previews": case "/api/load_activity_messages": result = []; break;
    case "/api/load_channel_messages": result = { messages: state.messages, next_before_seq: null, has_more: false }; break;
    case "/api/load_message": result = state.messages.find((row) => row.id === args.messageId); break;
    case "/api/load_ui_state": result = Object.fromEntries(args.scopes.map((scope) => [scope, state[scope]])); break;
    case "/api/replay_ui_events": result = { cursor: events.length, replayGap: false, events: events.filter((event) => event.cursor > args.cursor) }; break;
    case "/api/send_message": {
      const row = message(args.body);
      state.messages.push(row);
      publish({ type: "message_upsert", message: row });
      result = row;
      break;
    }
    case "/api/mark_channel_read": state.channels[0].unread_count = 0; publish({ type: "refresh", reason: "channel_read" }); break;
    case "/api/update_task_status": state.tasks[0].status = args.status; root.task_status = args.status; publish({ type: "refresh", reason: "task_status_updated" }); break;
  }
  const payload = JSON.stringify(result);
  if (url.pathname === "/api/load_ui_state" && args.scopes.includes("tasks") && holdTaskPatch) {
    taskPatchHeld = true;
    await holdTaskPatch;
  }
  response.writeHead(200, { "content-type": "application/json" });
  response.end(payload);
});
await new Promise((resolve) => api.listen(0, "127.0.0.1", resolve));
let browser;
const count = (path) => requests.filter((request) => request.path === `/api/${path}`).length;
try {
  browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto(`http://127.0.0.1:${api.address().port}`, { waitUntil: "domcontentloaded" });
  await page.locator(".markdown-body").filter({ hasText: "Sync task" }).waitFor();
  for (let i = 0; i < 50 && count("mark_channel_read") === 0; i++) await page.waitForTimeout(100);
  assert.equal(count("mark_channel_read"), 1);
  await page.waitForTimeout(700);
  assert.equal(count("bootstrap"), 1);
  await page.locator("textarea").first().fill("Sent without bootstrap");
  await page.getByRole("button", { name: "Send message", exact: true }).click();
  await page.locator(".markdown-body").filter({ hasText: "Sent without bootstrap" }).waitFor();
  await page.getByRole("button", { name: "Tasks", exact: true }).click();
  await page.locator('[aria-label="Task #1 status"] button[data-state="done"]').click();
  await page.locator('[aria-label="Task #1 status"] button[data-state="done"].active').waitFor();
  // Hold an older task read across a newer SSE invalidation. The newer status
  // must survive, and the single-flight queue must drain its follow-up read.
  let releaseTaskPatch;
  holdTaskPatch = new Promise((resolve) => { releaseTaskPatch = resolve; });
  await page.locator('[aria-label="Task #1 status"] button[data-state="todo"]').click();
  for (let i = 0; i < 50 && !taskPatchHeld; i++) await page.waitForTimeout(100);
  assert.ok(taskPatchHeld);
  await page.locator('[aria-label="Task #1 status"] button[data-state="in_progress"]').click();
  await page.waitForTimeout(100);
  holdTaskPatch = null;
  releaseTaskPatch();
  await page.locator('[aria-label="Task #1 status"] button[data-state="in_progress"].active').waitFor();
  await page.getByRole("button", { name: "Chat", exact: true }).click();
  const reads = count("mark_channel_read");
  for (let i = 0; i < 3; i++) { incoming(`Burst ${i}`); await page.waitForTimeout(50); }
  await page.waitForTimeout(1000);
  assert.equal(count("mark_channel_read") - reads, 1, "read bursts are debounced");
  assert.equal(count("bootstrap"), 1, "send/read/task updates must not bootstrap");
  assert.equal(await page.locator(".markdown-body").filter({ hasText: "Sent without bootstrap" }).count(), 1);

  // Exercise the app's actual visibility/page lifecycle handlers with a suspended clock.
  await page.clock.install();
  await page.evaluate(() => {
    Object.defineProperty(document, "visibilityState", { configurable: true, get: () => "hidden" });
    document.dispatchEvent(new Event("visibilitychange"));
    window.dispatchEvent(new Event("pagehide"));
  });
  await page.waitForTimeout(100);
  const replayBefore = count("replay_ui_events");
  incoming("Arrived while hidden");
  await page.clock.runFor(65_000);
  assert.equal(count("replay_ui_events"), replayBefore, "hidden tabs do not poll");
  assert.equal(count("bootstrap"), 1);
  const foregroundStart = Date.now();
  await page.evaluate(() => {
    Object.defineProperty(document, "visibilityState", { configurable: true, get: () => "visible" });
    document.dispatchEvent(new Event("visibilitychange"));
    window.dispatchEvent(new Event("pageshow"));
    window.dispatchEvent(new Event("focus"));
  });
  await page.clock.runFor(500);
  await page.locator(".markdown-body").filter({ hasText: "Arrived while hidden" }).waitFor({ timeout: 5000 });
  assert.ok(Date.now() - foregroundStart < 5000);
  assert.equal(count("replay_ui_events") - replayBefore, 1, "foreground events coalesce");
  await page.clock.resume();

  // Real SSE disconnect and failed HTTP reconnects for 10 seconds.
  disconnected = true;
  const failedAt = Date.now();
  for (const client of clients) client.end();
  clients.clear();
  incoming("Arrived offline once");
  await page.waitForTimeout(10_000);
  const retries = requests.filter((request) => request.path === "/api/events" && request.time >= failedAt).map((request) => request.time - failedAt);
  assert.ok(retries.length >= 3 && retries.length <= 4, `backoff retry times: ${retries}`);
  const intervals = retries.map((value, index) => value - (retries[index - 1] ?? 0));
  assert.ok(intervals[1] > intervals[0] && intervals[2] > intervals[1], `increasing retries: ${intervals}`);
  disconnected = false;
  await page.evaluate(() => window.dispatchEvent(new Event("online")));
  await page.locator(".markdown-body").filter({ hasText: "Arrived offline once" }).waitFor({ timeout: 5000 });
  await page.waitForTimeout(500);
  assert.equal(await page.locator(".markdown-body").filter({ hasText: "Arrived offline once" }).count(), 1);
  assert.equal(count("bootstrap"), 1, "reconnect replays without full refresh");
  const unchangedScopes = count("load_ui_state");
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await page.waitForTimeout(500);
  assert.equal(count("load_ui_state"), unchangedScopes, "up-to-date foreground does not reload collections");
  const normalBootstrapRequests = count("bootstrap");
  const recovered = page.waitForResponse((response) => new URL(response.url()).pathname === "/api/bootstrap");
  const gapEvent = publish({ type: "refresh", reason: "event_replay_gap" });
  await recovered;
  for (const client of clients) client.write(sse(gapEvent));
  await page.waitForTimeout(500);
  assert.equal(count("bootstrap"), normalBootstrapRequests + 1, "a replay gap recovers once, including duplicate delivery");
  assert.deepEqual(errors, []);
  console.log(JSON.stringify({ normalBootstrapRequests, gapRecoveryRequests: count("bootstrap") - normalBootstrapRequests, replayRequests: count("replay_ui_events"), readBurstRequests: 1,
    retryIntervalsMs: intervals, checks: "send, read, task status, stale collection response, unchanged foreground, hidden 65s, foreground under 5s, real SSE offline 10s, no missing/duplicate messages, one snapshot for replay gap" }));
} finally {
  await browser?.close();
  for (const client of clients) client.destroy();
  api.closeAllConnections();
  await new Promise((resolve) => api.close(resolve));
}
