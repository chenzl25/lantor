// Production UI against an isolated API; never reads or changes live inbox state.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile, mkdir } from "node:fs/promises";
import { resolve, join } from "node:path";
import { chromium } from "playwright";

const id = n => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const now = new Date().toISOString();
const earlier = new Date(Date.now() - 3_600_000).toISOString();
const channelId = id(1);
const message = (n, extra = {}) => ({
  id: id(n), seq: n, channel_id: channelId, thread_root_id: null,
  sender_agent_id: null, sender_name: "Test owner", sender_role: "owner",
  body: "Followed discussion", is_task: false, thread_followed: true,
  delivery_state: "complete", stream_key: "", task_number: null, task_status: null,
  attachments: [], artifacts: [], created_at: earlier, updated_at: earlier, ...extra,
});
const root = message(2);
const reply = message(3, { thread_root_id: root.id, body: "Waiting for an agent reply", created_at: now, updated_at: now });
const oldTasks = count => Array.from({ length: count }, (_, index) => ({
  id: id(100 + index), number: index + 1, message_id: root.id, channel_id: channelId,
  title: `Old review ${index + 1}`, status: "in_review", version: 1,
  channel_name: "activity-test", assignee_id: null, assignee_name: null,
  created_at: earlier, updated_at: earlier,
}));
const state = {
  db_url: "synthetic://activity-feed", web_base_url: null,
  owner_profile: { display_name: "Test owner", avatar: "T", description: "" },
  channels: [{ id: channelId, name: "activity-test", description: "", kind: "channel",
    dm_agent_id: null, unread_count: 0, github_unread_count: 0, github_review_synced_at: null }],
  messages: [root, reply], channel_message_history: [{ channel_id: channelId, before_seq: null, has_more: false }],
  agents: [], tasks: oldTasks(35), channel_members: [],
  thread_activities: [{ thread_root_id: root.id, channel_id: channelId, reply_count: 1,
    unread_count: 0, latest_message_id: reply.id, latest_activity_at: now }],
  saved_messages: [], dismissed_inbox_items: {}, read_inbox_items: {}, artifacts: [], reminders: [],
  agent_schedules: [], agent_runs: [], agent_work_items: [], agent_activities: [],
  supervisor: { pid: null, status: "stopped", updated_at: null },
  launch_agent: { label: "", plist_path: "", installed: false, loaded: false }, ui_event_cursor: 0,
};
function feedItems() {
  const tasks = state.tasks.map(task => ({
    id: `task:${task.id}`, dismissId: `task:${task.id}`, kind: "task",
    title: task.title, excerpt: "Unassigned", surface: "#activity-test", actor: "in review",
    timestamp: task.updated_at, unread: true, actorAgentId: null, actorRole: null,
    channelId, threadId: root.id, messageId: root.id, taskId: task.id, reminderId: null, replyCount: 0, newCount: 0,
  }));
  const threads = state.messages.filter(m => m.thread_root_id).map(m => ({
    id: `thread:${m.thread_root_id}`, dismissId: `thread:${m.thread_root_id}`, kind: "thread",
    title: m.body, excerpt: m.body, surface: "#activity-test", actor: m.sender_name,
    timestamp: m.created_at, unread: false, actorAgentId: null, actorRole: "owner",
    channelId, threadId: m.thread_root_id, messageId: m.id, taskId: null, reminderId: null, replyCount: 1, newCount: 0,
  }));
  return [...tasks, ...threads].filter(item => !state.dismissed_inbox_items[item.id])
    .map(item => ({ ...item, unread: item.unread && !state.read_inbox_items[item.id] }))
    .sort((a, b) => Date.parse(b.timestamp) - Date.parse(a.timestamp) || b.id.localeCompare(a.id));
}
let failNextFeed = false;
let heldFeed = null;
let inFlightFeeds = 0;
let maxInFlightFeeds = 0;
const requests = [];
const clients = new Set();
let activityLoads = 0;
const api = createServer(async (req, res) => {
  const url = new URL(req.url, "http://localhost");
  if (!url.pathname.startsWith("/api/")) {
    const file = resolve("dist", url.pathname === "/" ? "index.html" : url.pathname.slice(1));
    if (!file.startsWith(resolve("dist") + "/")) { res.writeHead(404).end(); return; }
    try {
      const mime = { html: "text/html", js: "application/javascript", css: "text/css", woff2: "font/woff2" };
      res.writeHead(200, { "content-type": mime[file.split(".").pop()] || "application/octet-stream" }).end(await readFile(file));
    } catch { res.writeHead(404).end(); }
    return;
  }
  if (url.pathname === "/api/events") {
    res.writeHead(200, { "content-type": "text/event-stream" }); res.write(": ready\n\n");
    clients.add(res); res.on("close", () => clients.delete(res)); return;
  }
  let raw = ""; for await (const chunk of req) raw += chunk;
  const args = raw ? JSON.parse(raw) : {};
  requests.push({ path: url.pathname, args });
  let result = { ok: true };
  switch (url.pathname) {
    case "/api/bootstrap": result = state; break;
    case "/api/load_activity_messages": throw new Error("Activity must not hydrate full messages");
    case "/api/load_activity_counts": {
      const items = feedItems(); result = { total: items.length, unread: items.filter(i => i.unread).length }; break;
    }
    case "/api/load_activity_feed": {
      activityLoads++;
      inFlightFeeds++; maxInFlightFeeds = Math.max(maxInFlightFeeds, inFlightFeeds);
      const request = args.request;
      let items = feedItems().filter(i => request.filter === "all" || (request.filter === "unread" ? i.unread : i.kind === request.filter));
      const cursor = request.after ?? request.before;
      const compare = i => Date.parse(i.timestamp) - Date.parse(cursor.timestamp) || i.id.localeCompare(cursor.id);
      if (cursor) items = items.filter(i => request.before ? compare(i) > 0 : compare(i) < 0);
      if (request.before) items.reverse();
      const more = items.length > 30;
      items = items.slice(0, 30);
      if (request.before) items.reverse();
      const toCursor = i => i ? { id: i.id, timestamp: i.timestamp } : null;
      result = { items,
        nextCursor: (request.before || more) ? toCursor(items.at(-1)) : null,
        previousCursor: (request.before ? more : request.after) ? toCursor(items[0]) : null,
      };
      if (heldFeed && !heldFeed.started) { heldFeed.started = true; await heldFeed.promise; }
      inFlightFeeds--;
      if (failNextFeed) { failNextFeed = false; res.writeHead(500, { "content-type": "application/json" }).end(JSON.stringify({ error: "Fixture query failure" })); return; }
      break;
    }
    case "/api/mark_inbox_items_read":
      for (const item of args.items) state.read_inbox_items[item.itemId] = item.dismissedUntil;
      break;
    case "/api/dismiss_inbox_items":
      for (const item of args.items) state.dismissed_inbox_items[item.itemId] = item.dismissedUntil;
      break;
    case "/api/load_channel_previews": result = []; break;
    case "/api/load_channel_messages": result = { messages: state.messages, next_before_seq: null, has_more: false }; break;
    case "/api/load_thread_messages": result = state.messages; break;
    case "/api/load_ui_state": result = Object.fromEntries(args.scopes.map(scope => [scope, state[scope]])); break;
    case "/api/replay_ui_events": result = { cursor: 0, replayGap: false, events: [] }; break;
  }
  res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(result));
});
await new Promise(done => api.listen(0, "127.0.0.1", done));
let browser;
try {
  browser = await chromium.launch();
  for (const width of [1440, 390]) {
    const context = await browser.newContext({ serviceWorkers: "block", viewport: { width, height: 960 } });
    const page = await context.newPage(); page.setDefaultTimeout(5000);
    const errors = []; page.on("pageerror", error => errors.push(error.message));
    for (const [taskCount, readThreadCount] of [[35, 0], [125, 0], [35, 125]]) {
      state.tasks = oldTasks(taskCount);
      state.read_inbox_items = {};
      state.dismissed_inbox_items = {};
      state.messages = [root, reply];
      for (let index = 0; index < readThreadCount; index++) {
        const otherRoot = message(1000 + index * 2);
        state.messages.push(otherRoot, message(1001 + index * 2, {
          thread_root_id: otherRoot.id, body: `Another read thread ${index}`,
          created_at: new Date(Date.parse(now) - 1000).toISOString(),
        }));
      }
      const before = activityLoads;
      await page.goto(`http://127.0.0.1:${api.address().port}`, { waitUntil: "domcontentloaded" });
      const trigger = page.locator(width > 760 ? ".sidebar" : ".mobile-bottom-nav").getByRole("button", { name: /Activity/ });
      await trigger.click();
      const dialog = page.getByRole("dialog", { name: "Activity", exact: true });
      await dialog.waitFor();
      for (let i = 0; i < 100 && activityLoads === before; i++) await page.waitForTimeout(20);
      assert.ok(activityLoads > before, "Activity loads its messages");
      await dialog.locator(".activity-feed-row").first().waitFor();
      const ownerRow = dialog.locator(".activity-feed-row").filter({ hasText: reply.body });
      await dialog.getByRole("button", { name: "Threads", exact: true }).click();
      await ownerRow.waitFor();
      assert.equal(await ownerRow.count(), 1, "followed owner reply is independently fetched in Threads");
      await dialog.getByRole("button", { name: "All", exact: true }).click();
      await ownerRow.waitFor();
      assert.equal(await ownerRow.count(), 1, "All shows the latest owner reply without loading older pages");
      assert.ok((await dialog.locator(".activity-feed-row").first().innerText()).includes(reply.body), "All is newest first regardless of unread state");
      assert.equal(await ownerRow.locator('[aria-label="Unread"]').count(), 0, "self-sent messages do not become unread");
      if (process.env.LANTOR_UI_SCREENSHOTS) {
        await mkdir(process.env.LANTOR_UI_SCREENSHOTS, { recursive: true });
        await page.screenshot({ path: join(process.env.LANTOR_UI_SCREENSHOTS, `activity-all-${width}-${taskCount}-${readThreadCount}.png`) });
      }
      await dialog.getByRole("button", { name: "Unread", exact: true }).click();
      await dialog.locator(".activity-feed-row").first().waitFor();
      assert.equal(await ownerRow.count(), 0, "Unread still excludes the owner's read thread");
      assert.equal(await dialog.locator(".activity-feed-row").count(), 30, "Unread still includes old review tasks");
      const firstPageTitle = await dialog.locator(".activity-feed-row h3").first().innerText();
      await dialog.getByRole("button", { name: "Next page", exact: true }).click();
      await dialog.locator(".activity-feed-row").first().waitFor();
      assert.ok(await dialog.locator(".activity-feed-row").count() <= 30, "next page replaces, never appends DOM");
      assert.notEqual(await dialog.locator(".activity-feed-row h3").first().innerText(), firstPageTitle);
      const footerBox = await dialog.locator(".activity-feed-pagination").boundingBox();
      assert.ok(footerBox && footerBox.y >= 0 && footerBox.y + footerBox.height <= 960, "pagination remains inside viewport");
      await dialog.getByRole("button", { name: "Previous page", exact: true }).click();
      await dialog.locator(".activity-feed-row").first().waitFor();
      assert.equal(await dialog.locator(".activity-feed-row h3").first().innerText(), firstPageTitle);
      await dialog.getByRole("button", { name: "All", exact: true }).click();
      await ownerRow.waitFor();
      assert.equal(await ownerRow.count(), 1, "switching filters restores the latest thread");
      assert.ok(await dialog.evaluate(e => e.scrollWidth <= e.clientWidth + 1), "Activity has no horizontal overflow");
      await page.keyboard.press("Escape");
      await dialog.waitFor({ state: "detached" });
    }
    await page.locator(width > 760 ? ".sidebar" : ".mobile-bottom-nav").getByRole("button", { name: /Activity/ }).click();
    const dialog = page.getByRole("dialog", { name: "Activity", exact: true });
    await dialog.locator(".activity-feed-row").first().waitFor();
    let release;
    heldFeed = { started: false, promise: new Promise(resolve => { release = resolve; }) };
    await dialog.getByRole("button", { name: "Threads", exact: true }).click();
    for (let i = 0; i < 100 && !heldFeed.started; i++) await page.waitForTimeout(20);
    assert.ok(heldFeed.started);
    await dialog.getByRole("button", { name: "Unread", exact: true }).click();
    await dialog.getByRole("button", { name: "Tasks", exact: true }).click();
    release(); heldFeed = null;
    await dialog.locator(".activity-feed-row").first().waitFor();
    assert.ok((await dialog.locator(".activity-feed-row h3").first().innerText()).includes("Old review"), "late Threads response cannot populate Tasks");
    assert.equal(maxInFlightFeeds, 1, "rapid filter changes keep only one request in flight");
    failNextFeed = true;
    await dialog.getByRole("button", { name: "Latest activity", exact: true }).click();
    await dialog.getByRole("alert").waitFor();
    assert.equal(await dialog.locator(".activity-feed-row").count(), 0, "failed page does not expose stale rows as current");
    await dialog.getByRole("button", { name: "Retry loading activity", exact: true }).click();
    await dialog.locator(".activity-feed-row").first().waitFor();
    await dialog.getByRole("button", { name: "Mark page read", exact: true }).click();
    await page.waitForFunction(() => !document.querySelector(".activity-feed-body[aria-busy=true]") && document.querySelectorAll(".activity-feed-row").length > 0);
    assert.equal(Object.keys(state.read_inbox_items).length, 30, "bulk read covers only the current page");
    await dialog.getByRole("button", { name: "Dismiss page", exact: true }).click();
    await page.waitForFunction(() => !document.querySelector(".activity-feed-body[aria-busy=true]") && document.querySelectorAll(".activity-feed-row").length === 5);
    assert.equal(Object.keys(state.dismissed_inbox_items).length, 30, "bulk dismissal covers only the current page");
    await dialog.getByRole("button", { name: "Threads", exact: true }).click();
    await dialog.locator(".activity-feed-row").first().waitFor();
    await dialog.getByRole("button", { name: "Next page", exact: true }).click();
    await dialog.locator(".activity-feed-row").first().waitFor();
    await dialog.getByRole("button", { name: "Close activity", exact: true }).click();
    await dialog.waitFor({ state: "detached" });
    await page.locator(width > 760 ? ".sidebar" : ".mobile-bottom-nav").getByRole("button", { name: /Activity/ }).click();
    await dialog.locator(".activity-feed-row").first().waitFor();
    assert.ok((await dialog.locator(".activity-feed-row h3").first().innerText()).includes(reply.body), "reopening starts at latest page");
    assert.ok(requests.some(r => r.path === "/api/load_activity_counts"), "badge has an independent count query");
    assert.equal(requests.some(r => r.path === "/api/load_activity_messages"), false);
    assert.deepEqual(errors, []);
    await context.close();
    console.log(`PASS ${width}px: All/Threads owner reply, Unread semantics, more than 120 read/unread items`);
  }
} finally {
  await browser?.close();
  for (const client of clients) client.end();
  api.closeAllConnections();
  await new Promise(done => api.close(done));
}
