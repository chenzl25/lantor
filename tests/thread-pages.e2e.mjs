// Production App against an isolated API: a long thread opens with its newest
// replies and loads earlier ones on request. Run npm run build first.
// No live workspace data is read or mutated.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { chromium, devices, webkit } from "playwright";

const id = n => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const channelId = id(1000);
const start = Date.parse("2026-01-05T09:00:00.000Z");
let seq = 0;
const message = (body, extra = {}) => {
  seq += 1;
  const at = new Date(start + seq * 60_000).toISOString();
  return { id: id(seq), seq, channel_id: channelId, thread_root_id: null, sender_agent_id: null, sender_name: "Dylan",
    sender_role: "owner", body, is_task: false, thread_followed: false, delivery_state: "complete", stream_key: "",
    task_number: null, task_status: null, attachments: [], artifacts: [], created_at: at, updated_at: at, ...extra };
};
const longRoot = message("Long thread root");
const shortRoot = message("Short thread root");
const replies = [];
for (let index = 0; index < 230; index += 1) {
  replies.push(message(`Long reply ${index}`, { thread_root_id: longRoot.id, sender_role: index % 2 ? "agent" : "owner", sender_name: index % 2 ? "Agent" : "Dylan" }));
  if (index % 23 === 0) replies.push(message(`Short reply ${index / 23}`, { thread_root_id: shortRoot.id }));
}
const longReplies = replies.filter(m => m.thread_root_id === longRoot.id);
const shortReplies = replies.filter(m => m.thread_root_id === shortRoot.id);
const target = longReplies[5];
const pointer = message(`See [[message:${target.id}]]`);
const all = [longRoot, shortRoot, ...replies, pointer];
const activity = (root, rows) => ({ thread_root_id: root.id, channel_id: channelId, reply_count: rows.length, unread_count: 0,
  agent_unread_count: 0, first_unread_agent_message_id: null, latest_message_id: rows.at(-1).id, latest_activity_at: rows.at(-1).created_at });
const state = { db_url: "synthetic://thread-pages", web_base_url: null, owner_profile: { display_name: "Dylan", avatar: "D", description: "" },
  channels: [{ id: channelId, name: "paging", kind: "channel", dm_agent_id: null, description: "", unread_count: 0, github_unread_count: 0, github_review_synced_at: null }],
  // The bootstrap also carries one old reply, as Activity or a reference would, to check it stays hidden.
  messages: [longRoot, shortRoot, ...longReplies.slice(-2), target, ...shortReplies.slice(-2), pointer],
  channel_message_history: [{ channel_id: channelId, before_seq: null, has_more: false }],
  agents: [], tasks: [], channel_members: [], thread_activities: [activity(longRoot, longReplies), activity(shortRoot, shortReplies)],
  saved_messages: [], dismissed_inbox_items: {}, read_inbox_items: {}, artifacts: [], reminders: [], agent_runs: [],
  agent_work_items: [], agent_activities: [], supervisor: { pid: null, status: "stopped", updated_at: null },
  launch_agent: { label: "", plist_path: "", installed: false, loaded: false }, ui_event_cursor: 0 };

// The same rules as message_store::load_thread_page.
function threadPage({ threadRootId, beforeSeq, sinceSeq, limit = 50 }) {
  const below = all.filter(m => m.thread_root_id === threadRootId && m.seq < (beforeSeq ?? Infinity));
  const newest = below.slice(-Math.min(Math.max(limit, 1), 200));
  const from = newest.length ? Math.min(newest[0].seq, sinceSeq ?? Infinity) : Infinity;
  const page = below.filter(m => m.seq >= from);
  const oldest = page[0]?.seq ?? null;
  const hasMore = oldest !== null && all.some(m => m.thread_root_id === threadRootId && m.seq < oldest);
  return { messages: [all.find(m => m.id === threadRootId), ...page], next_before_seq: hasMore ? oldest : null, has_more: hasMore };
}

const dist = resolve("dist");
let requests = [];
// Earlier pages can be held so the test measures the viewport before they land.
let holdEarlier = false, heldEarlier = [];
const waitForHeldEarlier = async () => {
  const deadline = Date.now() + 8000;
  while (heldEarlier.length === 0) {
    if (Date.now() > deadline) throw new Error("scrolling up did not request earlier replies");
    await new Promise(done => setTimeout(done, 10));
  }
};
const releaseEarlier = () => { holdEarlier = false; for (const release of heldEarlier.splice(0)) release(); };
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
  requests.push({ path: url.pathname, args });
  let result = { ok: true };
  switch (url.pathname) {
    case "/api/bootstrap": result = state; break;
    case "/api/load_channel_previews": case "/api/load_activity_messages": case "/api/search_messages": result = []; break;
    case "/api/load_channel_messages": result = { messages: state.messages.filter(m => !m.thread_root_id), next_before_seq: null, has_more: false, thread_activities: state.thread_activities }; break;
    case "/api/load_thread_page":
      if (holdEarlier && args.beforeSeq !== undefined) await new Promise(release => heldEarlier.push(release));
      result = threadPage(args); break;
    // The whole-thread route kept for pages loaded before paging.
    case "/api/load_thread_messages": result = all.filter(m => m.id === args.threadRootId || m.thread_root_id === args.threadRootId); break;
    case "/api/load_message": result = all.find(m => m.id === args.messageId); break;
    case "/api/load_ui_state": result = Object.fromEntries(args.scopes.map(scope => [scope, state[scope]])); break;
    case "/api/replay_ui_events": result = { cursor: 0, replayGap: false, events: [] }; break;
  }
  res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(result));
});
await new Promise(done => api.listen(0, "127.0.0.1", done));
const origin = `http://127.0.0.1:${api.address().port}`;
const pageRequests = () => requests.filter(r => r.path === "/api/load_thread_page").map(r => r.args);

async function run(engine, name, contextOptions) {
  const browser = await engine.launch();
  try {
    const context = await browser.newContext({ serviceWorkers: "block", ...contextOptions });
    const page = await context.newPage(); page.setDefaultTimeout(8000);
    const errors = []; page.on("pageerror", error => errors.push(error.message));
    const mobile = Boolean(contextOptions.isMobile);
    const shownReplies = () => page.locator(".thread .reply-list article[data-message-id]").evaluateAll(rows => rows.map(row => row.dataset.messageId));
    const earlier = page.locator(".thread").getByRole("button", { name: /Show earlier replies|Loading…/ });
    const showChannel = async () => {
      if (mobile) await page.locator(".sidebar .channel").filter({ hasText: "paging" }).first().click();
      await page.locator(`.conversation [data-message-id="${pointer.id}"]`).waitFor();
    };
    const openThread = async root => {
      const row = page.locator(`.conversation [data-message-id="${root.id}"]`);
      if (mobile) await row.locator(".markdown-body").first().click();
      else { await row.hover(); await row.getByRole("button", { name: "View replies", exact: true }).click(); }
      await page.locator(`.thread [data-message-id="${root.id}"]`).waitFor();
    };
    const closeThread = async () => {
      await page.locator(".thread").getByRole("button", { name: mobile ? "Back to channel" : "Close thread", exact: true }).click();
      await page.locator(".thread .reply-list article").first().waitFor({ state: "detached" });
    };
    const ids = rows => rows.map(m => m.id);

    // A long thread opens with its 50 newest replies and the total count.
    requests = [];
    await page.goto(origin, { waitUntil: "domcontentloaded" });
    await showChannel();
    await openThread(longRoot);
    await page.waitForFunction(() => document.querySelectorAll(".thread .reply-list article[data-message-id]").length === 50);
    assert.deepEqual(await shownReplies(), ids(longReplies.slice(-50)), `${name}: the newest page, without the old reply already in memory`);
    assert.deepEqual(pageRequests(), [{ threadRootId: longRoot.id, limit: 50 }]);
    await page.locator(".thread .thread-replies-divider").getByText("230 replies", { exact: true }).waitFor();
    await earlier.waitFor();

    // Scrolling up loads the previous page without moving what is on screen.
    const anchorId = longReplies.at(-50).id;
    const anchorTop = () => page.locator(".thread .thread-scroll").evaluate((node, rowId) => {
      const row = node.querySelector(`[data-message-id="${rowId}"]`);
      return row.getBoundingClientRect().top - node.getBoundingClientRect().top;
    }, anchorId);
    holdEarlier = true;
    await page.locator(".thread .thread-scroll").evaluate((node, rowId) => {
      // A reader's upward wheel or swipe stops following the newest reply first.
      node.dispatchEvent(new WheelEvent("wheel", { deltaY: -100, bubbles: true }));
      const row = node.querySelector(`[data-message-id="${rowId}"]`);
      node.scrollTop += row.getBoundingClientRect().top - node.getBoundingClientRect().top - 40;
    }, anchorId);
    await waitForHeldEarlier();
    const before = await anchorTop();
    releaseEarlier();
    await page.waitForFunction(() => document.querySelectorAll(".thread .reply-list article[data-message-id]").length === 100);
    await page.evaluate(() => new Promise(done => requestAnimationFrame(() => requestAnimationFrame(done))));
    assert.ok(Math.abs(await anchorTop() - before) <= 2, `${name}: the reply on screen stays put while earlier replies load above it`);
    assert.deepEqual(await shownReplies(), ids(longReplies.slice(-100)));
    assert.deepEqual(pageRequests().at(-1), { threadRootId: longRoot.id, beforeSeq: longReplies.at(-50).seq, limit: 50 });

    // At the very top the root is on screen too; the oldest reply still stays put.
    const firstId = longReplies.at(-100).id;
    const firstTop = () => page.locator(".thread .thread-scroll").evaluate((node, rowId) =>
      node.querySelector(`[data-message-id="${rowId}"]`).getBoundingClientRect().top - node.getBoundingClientRect().top, firstId);
    holdEarlier = true;
    await page.locator(".thread .thread-scroll").evaluate(node => {
      node.dispatchEvent(new WheelEvent("wheel", { deltaY: -100, bubbles: true }));
      node.scrollTop = 0;
    });
    await waitForHeldEarlier();
    await page.evaluate(() => new Promise(done => requestAnimationFrame(() => requestAnimationFrame(done))));
    const topBefore = await firstTop();
    releaseEarlier();
    await page.waitForFunction(() => document.querySelectorAll(".thread .reply-list article[data-message-id]").length === 150);
    await page.evaluate(() => new Promise(done => requestAnimationFrame(() => requestAnimationFrame(done))));
    assert.ok(Math.abs(await firstTop() - topBefore) <= 2, `${name}: reaching the top does not push the replies being read down`);

    // The button loads the rest; the thread ends complete, in order, with no gap.
    // Scrolling the button into view can load the page before the click lands.
    for (let attempt = 0; (await shownReplies()).length < longReplies.length && attempt < 10; attempt += 1) {
      const count = (await shownReplies()).length;
      await earlier.click({ timeout: 2000 }).catch(() => undefined);
      await page.waitForFunction(n => document.querySelectorAll(".thread .reply-list article[data-message-id]").length > n, count);
    }
    assert.equal(await earlier.count(), 0, `${name}: no button once the first reply is loaded`);
    assert.deepEqual(await shownReplies(), ids(longReplies), `${name}: every reply once, in order`);

    // Reopening reloads the whole loaded run, so nothing that arrived meanwhile is skipped.
    await closeThread();
    await openThread(longRoot);
    await page.waitForFunction(() => document.querySelectorAll(".thread .reply-list article[data-message-id]").length === 230);
    assert.deepEqual(pageRequests().at(-1), { threadRootId: longRoot.id, sinceSeq: 0, limit: 50 });

    // A short thread loads complete and shows no button.
    await closeThread();
    await openThread(shortRoot);
    await page.waitForFunction(n => document.querySelectorAll(".thread .reply-list article[data-message-id]").length === n, shortReplies.length);
    assert.equal(await earlier.count(), 0);
    await closeThread();

    // Jumping to an old reply loads every reply from it to the newest.
    await page.reload({ waitUntil: "domcontentloaded" });
    await showChannel();
    requests = [];
    await page.locator(`.conversation [data-message-id="${pointer.id}"] .message-reference-card`).click();
    await page.waitForFunction(n => document.querySelectorAll(".thread .reply-list article[data-message-id]").length >= n, 225);
    assert.ok(pageRequests().some(args => args.sinceSeq === target.seq && !("beforeSeq" in args)), `${name}: one request reaches back to the target`);
    // Landing near the top of the run may also load the few replies before it.
    await page.locator(".thread").getByRole("button", { name: "Loading…" }).waitFor({ state: "detached" });
    const shown = await shownReplies();
    assert.ok(shown.length >= 225);
    assert.deepEqual(shown, ids(longReplies.slice(-shown.length)), `${name}: a contiguous run from at or before the target to the newest`);
    await page.waitForFunction(targetId => {
      const row = document.querySelector(`.thread [data-message-id="${targetId}"]`);
      const box = row?.getBoundingClientRect(), view = document.querySelector(".thread .thread-scroll")?.getBoundingClientRect();
      return box && view && box.bottom > view.top && box.top < view.bottom;
    }, target.id);

    assert.deepEqual(errors, []);
    await context.close();
    return browser.version();
  } finally {
    await browser.close();
  }
}

try {
  const versions = {};
  for (const [name, engine, options] of [
    ["chromium desktop", chromium, { viewport: { width: 1440, height: 900 } }],
    ["webkit desktop", webkit, { viewport: { width: 1440, height: 900 } }],
    ["webkit iPhone", webkit, devices["iPhone 15"]],
  ]) versions[name] = await run(engine, name, options);
  console.log(JSON.stringify({ versions, checks: "newest page with total, scroll-up load keeps position, load to start, reopen reload, short thread, jump to old reply" }));
} finally {
  api.close();
}
