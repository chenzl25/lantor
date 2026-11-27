// Production App against an isolated API, on a phone: iOS reloads a Home
// Screen app it discarded in the background. The workspace the phone showed
// last is drawn at once, before the bootstrap answers, with the same thread
// open, and the bootstrap then replaces it. A Safari tab and a desktop keep
// no copy; a copy
// older than a week or from another build is not shown; an expired session
// clears the copy and the screen. Run npm run build first. No live workspace
// data is read or mutated.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { chromium, webkit } from "playwright";

const id = n => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const now = new Date().toISOString(), channelId = id(1000);
const agent = { id: id(800), handle: "Hancock", display_name: "Hancock", role: "agent", status: "idle", runtime: "codex",
  model: "gpt-6-sol", reasoning_effort: "high", service_tier: "", avatar: "H", description: "", launch_command: "", environment_variables: "",
  working_directory: "", workspace_exists: false, workspace_memory_path: "", workspace_memory_exists: false,
  workspace_entries: [], details_loaded: true, daily_budget_micros: 0, subscription_status: null };
const message = (n, extra = {}) => ({ id: id(n), seq: n, channel_id: channelId, thread_root_id: null, sender_agent_id: null,
  sender_name: "Dylan", sender_role: "owner", body: `Discussion ${n}`, is_task: false, thread_followed: false,
  delivery_state: "complete", stream_key: "", task_number: null, task_status: null, attachments: [], artifacts: [], created_at: now, updated_at: now, ...extra });
const root = message(1, { body: "Toolbar review" });
// Enough replies that the thread scrolls on a phone. Senders alternate so each row keeps its header.
const replies = Array.from({ length: 14 }, (_, index) => message(2 + index, { thread_root_id: root.id,
  ...(index % 2 ? { sender_agent_id: agent.id, sender_name: "Hancock", sender_role: "agent" } : {}),
  body: `Reply ${index + 1}: the toolbar keeps one line at 320 px and every button is 44 pt.` }));
const target = replies[11];
const channels = [{ id: channelId, name: "ui-review", kind: "channel", dm_agent_id: null, description: "", unread_count: 0,
  github_unread_count: 0, github_review_synced_at: null }];
const notification = (threadId, repository) => ({ thread_id: String(threadId), repository, subject_type: "PullRequest",
  title: `Pull request ${threadId}`, number: threadId, url: `https://github.com/${repository}/pull/${threadId}`, reason: "review_requested",
  unread: true, updated_at: now, thread_root_id: null, channel_id: null });
const githubInbox = { enabled: true, account_login: "octo", synced_at: now, error: null,
  items: [notification(101, "org/repo"), notification(102, "org/docs"), notification(103, "org/repo")] };
const freshState = () => ({ db_url: "synthetic://resume-cache", web_base_url: null, owner_profile: { display_name: "Dylan", avatar: "D", description: "" },
  channels, messages: [root, ...replies], channel_message_history: [{ channel_id: channelId, before_seq: null, has_more: false }],
  agents: [agent], channel_members: [{ channel_id: channelId, agent_id: agent.id }],
  // A task in review puts a count on Needs you and a status menu in the Tasks tab.
  tasks: [{ id: id(900), number: 1, message_id: root.id, channel_id: channelId, title: "Review the toolbar", status: "in_review", version: 1,
    channel_name: "ui-review", assignee_id: agent.id, assignee_name: "Hancock", created_at: now, updated_at: now }],
  thread_activities: [{ thread_root_id: root.id, channel_id: channelId, unread_count: 0, reply_count: replies.length,
    latest_message_id: replies.at(-1).id, latest_activity_at: now }],
  saved_messages: [], dismissed_inbox_items: {}, read_inbox_items: {}, artifacts: [], reminders: [], decisions: [],
  github_inbox: structuredClone(githubInbox), agent_runs: [], agent_work_items: [], agent_activities: [],
  supervisor: { pid: null, status: "stopped", updated_at: null },
  launch_agent: { label: "", plist_path: "", installed: false, loaded: false }, ui_event_cursor: 0 });
let state = freshState(), holdBootstrap = false, releaseBootstrap = () => {}, sessionExpired = false;
const threadMessages = rootId => state.messages.filter(m => m.id === rootId || m.thread_root_id === rootId);
const api = createServer(async (req, res) => {
  const url = new URL(req.url, "http://localhost");
  if (!url.pathname.startsWith("/api/")) {
    const file = resolve("dist", url.pathname === "/" ? "index.html" : url.pathname.slice(1));
    if (!file.startsWith(resolve("dist") + "/")) { res.writeHead(404).end(); return; }
    try { const mime = { html: "text/html", js: "application/javascript", css: "text/css", png: "image/png", woff: "font/woff", woff2: "font/woff2" };
      res.writeHead(200, { "content-type": mime[file.split(".").pop()] || "application/octet-stream" }).end(await readFile(file));
    } catch { res.writeHead(404).end(); }
    return;
  }
  // Cloudflare Access answers an expired session's AJAX with 401.
  if (sessionExpired) { res.writeHead(401).end(); return; }
  if (url.pathname === "/api/events") { res.writeHead(200, { "content-type": "text/event-stream" }); res.write(": ready\n\n"); return; }
  let raw = "";
  try { for await (const chunk of req) raw += chunk; }
  catch (error) { if (req.aborted && error.code === "ECONNRESET") return; throw error; }
  const args = raw ? JSON.parse(raw) : {};
  if (url.pathname === "/api/bootstrap" && holdBootstrap) await new Promise(done => { releaseBootstrap = done; });
  let result = { ok: true };
  switch (url.pathname) {
    case "/api/bootstrap": result = state; break;
    case "/api/load_channel_previews": case "/api/load_activity_messages": case "/api/search_messages":
    case "/api/search_channel_wikis": case "/api/load_github_references": result = []; break;
    case "/api/load_channel_messages": result = { messages: state.messages.filter(m => m.channel_id === args.channelId && !m.thread_root_id), next_before_seq: null, has_more: false, thread_activities: state.thread_activities }; break;
    case "/api/load_thread_messages": result = threadMessages(args.threadRootId); break;
    case "/api/load_thread_page": result = { messages: threadMessages(args.threadRootId), next_before_seq: null, has_more: false }; break;
    case "/api/load_ui_state": result = Object.fromEntries(args.scopes.map(scope => [scope, state[scope] ?? []])); break;
    case "/api/replay_ui_events": result = { cursor: 0, replayGap: false, events: [] }; break;
    case "/api/load_github_inbox": case "/api/refresh_github_inbox": result = state.github_inbox; break;
    case "/api/check_runtime": result = { runtime: args.runtime, command: args.runtime, available: true, detail: "" }; break;
  }
  res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(result));
});
await new Promise(done => api.listen(0, "127.0.0.1", done));
const origin = `http://127.0.0.1:${api.address().port}`;

// The page's stored copy, read the way the app opens its database.
const stored = page => page.evaluate(() => new Promise(done => {
  const request = indexedDB.open("lantor-resume-cache", 1);
  request.onupgradeneeded = () => request.result.createObjectStore("snapshots");
  request.onerror = () => done(null);
  request.onsuccess = () => {
    const database = request.result;
    const get = database.transaction("snapshots").objectStore("snapshots").get("phone");
    get.onsuccess = () => { database.close(); done(get.result ? { savedAt: get.result.savedAt, version: get.result.version, view: get.result.view, messages: get.result.data.messages.map(m => m.id) } : null); };
  };
}));
const editStored = (page, change) => page.evaluate(change => new Promise(done => {
  const request = indexedDB.open("lantor-resume-cache", 1);
  request.onsuccess = () => {
    const database = request.result;
    const store = database.transaction("snapshots", "readwrite").objectStore("snapshots");
    const get = store.get("phone");
    get.onsuccess = () => { store.put({ ...get.result, ...change }, "phone").onsuccess = () => { database.close(); done(); }; };
  };
}), change);
const savedOnHide = async page => {
  await page.evaluate(() => window.dispatchEvent(new Event("pagehide")));
  for (let tries = 0; tries < 100; tries += 1) {
    const copy = await stored(page);
    if (copy) return copy;
    await page.waitForTimeout(20);
  }
  assert.fail("the phone kept no copy when the page was hidden");
};
const openThread = async page => {
  await page.locator(".sidebar .channel-block").getByText("ui-review", { exact: true }).tap();
  const summary = page.locator(`.conversation [data-message-id="${root.id}"] .thread-reply-summary`);
  await summary.waitFor({ state: "visible" });
  await summary.tap();
  await page.locator(`.thread [data-message-id="${replies.at(-1).id}"]`).waitFor();
};

let browser;
try {
  for (const [name, engine] of Object.entries({ chromium, webkit })) {
    state = freshState(); holdBootstrap = false; sessionExpired = false;
    browser = await engine.launch();
    const phone = { viewport: { width: 393, height: 852 }, isMobile: true, hasTouch: true, serviceWorkers: "block" };
    // The same phone in a Safari tab keeps no copy: only the Home Screen app does.
    const tab = await (await browser.newContext(phone)).newPage();
    await tab.goto(origin, { waitUntil: "domcontentloaded" });
    await tab.locator(".mobile-bottom-nav").waitFor();
    await openThread(tab);
    await tab.evaluate(() => window.dispatchEvent(new Event("pagehide")));
    await tab.waitForTimeout(300);
    assert.equal(await stored(tab), null, `${name}: a Safari tab keeps no copy`);
    await tab.context().close();
    const context = await browser.newContext(phone);
    // iOS marks a page opened from the Home Screen as standalone.
    await context.addInitScript(() => Object.defineProperty(navigator, "standalone", { value: true }));
    const page = await context.newPage(); page.setDefaultTimeout(8000);
    const errors = []; page.on("pageerror", error => errors.push(error.message));
    await page.goto(origin, { waitUntil: "domcontentloaded" });
    await page.locator(".mobile-bottom-nav").waitFor();
    await openThread(page);

    // 1. Hiding the page keeps the open thread, its channel and the view.
    const copy = await savedOnHide(page);
    assert.deepEqual(copy.view, { activeChannelId: channelId, activeThreadId: root.id, threadOpen: true, homeOpen: false }, `${name}: the view is kept`);
    assert.ok(copy.messages.includes(root.id) && copy.messages.includes(replies.at(-1).id), `${name}: the thread is kept`);

    // 2. A reload draws the copy with the thread open before the bootstrap
    //    answers; the bootstrap then replaces it.
    const removed = replies[5];
    state.messages = state.messages.filter(m => m.id !== removed.id);
    holdBootstrap = true;
    await page.reload({ waitUntil: "domcontentloaded" });
    await page.locator(`.thread [data-message-id="${replies.at(-1).id}"]`).waitFor()
      .catch(() => assert.fail(`${name}: the thread is not on screen while the bootstrap is pending`));
    assert.equal(await page.locator(`.thread [data-message-id="${removed.id}"]`).count(), 1, `${name}: the copy is what the phone showed`);
    holdBootstrap = false; releaseBootstrap();
    await page.locator(`.thread [data-message-id="${removed.id}"]`).waitFor({ state: "detached" })
      .catch(() => assert.fail(`${name}: the bootstrap did not replace the copy`));

    // 3. A copy older than a week, or from another build, is not shown and is deleted.
    for (const change of [{ savedAt: Date.now() - 8 * 24 * 60 * 60 * 1000 }, { version: "another-build" }]) {
      await savedOnHide(page);
      await editStored(page, change);
      holdBootstrap = true;
      await page.reload({ waitUntil: "domcontentloaded" });
      await page.waitForTimeout(500);
      assert.equal(await page.locator(".thread, .conversation [data-message-id]").count(), 0, `${name}: ${JSON.stringify(change)} is not shown`);
      assert.equal(await stored(page), null, `${name}: ${JSON.stringify(change)} is deleted`);
      holdBootstrap = false; releaseBootstrap();
      await page.locator(".mobile-bottom-nav").waitFor();
      if (!(await page.locator(".thread").count())) await openThread(page);
    }

    // 4. An expired session clears the copy, and the screen that showed it.
    await savedOnHide(page);
    sessionExpired = true;
    await page.reload({ waitUntil: "domcontentloaded" });
    await page.waitForFunction(() => !document.querySelector(".thread"))
      .catch(() => assert.fail(`${name}: the copy stays on screen after the session expired`));
    for (let tries = 0; tries < 100 && await stored(page); tries += 1) await page.waitForTimeout(20);
    assert.equal(await stored(page), null, `${name}: the copy is cleared`);
    sessionExpired = false;
    assert.deepEqual(errors, [], `${name}: no page errors`);
    await browser.close(); browser = null;
  }

  // A desktop keeps no copy.
  state = freshState();
  browser = await chromium.launch();
  const page = await (await browser.newContext({ viewport: { width: 1280, height: 820 }, serviceWorkers: "block" })).newPage();
  await page.goto(origin, { waitUntil: "domcontentloaded" });
  await page.locator(`.conversation [data-message-id="${root.id}"]`).waitFor();
  await page.evaluate(() => window.dispatchEvent(new Event("pagehide")));
  await page.waitForTimeout(300);
  assert.equal(await stored(page), null, "a desktop keeps no copy");
  console.log("resume cache: ok");
} finally {
  await browser?.close();
  await new Promise(done => { api.closeAllConnections?.(); api.close(done); });
}
