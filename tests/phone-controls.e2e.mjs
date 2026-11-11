// Production App against an isolated API, on a phone: the bottom bar badges
// leave their icons visible, no form control is small enough for iOS to zoom
// the page, a followed thread stays on its latest reply while the reply box
// grows, and a tapped thread message offers Quote, Copy link and Save.
// Run npm run build first. No live workspace data is read or mutated.
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
const freshState = () => ({ db_url: "synthetic://phone-controls", web_base_url: null, owner_profile: { display_name: "Dylan", avatar: "D", description: "" },
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
let state = freshState(), requests = [];
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
  if (url.pathname === "/api/events") { res.writeHead(200, { "content-type": "text/event-stream" }); res.write(": ready\n\n"); return; }
  let raw = "";
  try { for await (const chunk of req) raw += chunk; }
  catch (error) { if (req.aborted && error.code === "ECONNRESET") return; throw error; }
  const args = raw ? JSON.parse(raw) : {}; requests.push({ path: url.pathname, args });
  let result = { ok: true };
  switch (url.pathname) {
    case "/api/bootstrap": result = state; break;
    case "/api/load_channel_previews": case "/api/load_activity_messages": case "/api/search_messages":
    case "/api/search_channel_wikis": case "/api/load_github_references": result = []; break;
    case "/api/load_channel_messages": result = { messages: state.messages.filter(m => m.channel_id === args.channelId), next_before_seq: null, has_more: false }; break;
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

// iOS zooms the page for a focused control under 16px.
const smallControls = page => page.evaluate(() => [...document.querySelectorAll('input:not([type="checkbox"]):not([type="radio"]):not([type="file"]):not([type="hidden"]), textarea, select')]
  .filter(control => control.getBoundingClientRect().width > 0 && parseFloat(getComputedStyle(control).fontSize) < 16)
  .map(control => `${control.tagName.toLowerCase()}[${control.getAttribute("aria-label") || control.placeholder || control.className}] ${getComputedStyle(control).fontSize}`));
// How far the thread is from its latest reply, and how much of that reply the reply box hides.
const threadBottom = page => page.evaluate(() => {
  const scroll = document.querySelector(".thread-scroll");
  const last = [...scroll.querySelectorAll("[data-message-id]")].at(-1).getBoundingClientRect();
  return { fromBottom: Math.round(scroll.scrollHeight - scroll.scrollTop - scroll.clientHeight), hidden: Math.round(last.bottom - scroll.getBoundingClientRect().bottom) };
});
const waitForThreadBottom = async (page, when) => {
  await page.waitForFunction(() => { const scroll = document.querySelector(".thread-scroll"); return scroll.scrollHeight - scroll.scrollTop - scroll.clientHeight <= 2; }, null, { timeout: 3000 })
    .catch(async () => assert.fail(`${when}: the thread left its latest reply ${JSON.stringify(await threadBottom(page))}`));
  assert.ok((await threadBottom(page)).hidden <= 0, `${when}: the latest reply stays in view`);
};

let browser;
try {
  for (const [name, engine] of Object.entries({ chromium, webkit })) {
    state = freshState(); requests = [];
    browser = await engine.launch();
    const context = await browser.newContext({ viewport: { width: 393, height: 852 }, isMobile: true, hasTouch: true, serviceWorkers: "block" });
    const page = await context.newPage(); page.setDefaultTimeout(8000);
    const errors = []; page.on("pageerror", error => errors.push(error.message));
    await page.goto(origin, { waitUntil: "domcontentloaded" });
    const bar = page.locator(".mobile-bottom-nav");
    await bar.waitFor();

    // 1. A badge sits on the corner of its icon instead of over it.
    const badges = await bar.locator("button").evaluateAll(buttons => buttons.flatMap(button => {
      const icon = button.querySelector("svg")?.getBoundingClientRect(), badge = button.querySelector(".unread-badge")?.getBoundingClientRect();
      if (!icon || !badge) return [];
      const overlapX = Math.max(0, Math.min(icon.right, badge.right) - Math.max(icon.left, badge.left));
      const overlapY = Math.max(0, Math.min(icon.bottom, badge.bottom) - Math.max(icon.top, badge.top));
      return [{ label: button.textContent.trim(), covered: overlapX * overlapY / (icon.width * icon.height) }];
    }));
    assert.deepEqual(badges.map(badge => badge.label), ["1Needs you", "3GitHub"], `${name}: both counts show`);
    for (const badge of badges) assert.ok(badge.covered < 0.3, `${name}: the ${badge.label} badge covers ${Math.round(badge.covered * 100)}% of its icon`);

    // 2. No dialog or tab has a form control under 16px.
    const openDialog = async (trigger, title) => { await trigger.tap(); const dialog = page.getByRole("dialog", { name: title, exact: true }); await dialog.waitFor(); return dialog; };
    const closeDialog = async dialog => { await page.keyboard.press("Escape"); await dialog.waitFor({ state: "detached" }); };
    const newChannel = await openDialog(page.getByRole("button", { name: "New channel", exact: true }), "New channel");
    assert.equal(await newChannel.locator("input:not([type=checkbox])").count() > 0, true);
    assert.deepEqual(await smallControls(page), [], `${name}: New channel`);
    await closeDialog(newChannel);
    const newAgent = await openDialog(page.getByRole("button", { name: "New agent", exact: true }), "New agent");
    assert.equal(await newAgent.locator("textarea").count() > 0, true);
    assert.deepEqual(await smallControls(page), [], `${name}: New agent`);
    await closeDialog(newAgent);
    const search = await openDialog(bar.getByRole("button", { name: "Search", exact: true }), "Search");
    assert.equal(await search.locator("select").count() > 0, true);
    assert.deepEqual(await smallControls(page), [], `${name}: Search`);
    await closeDialog(search);
    await bar.getByRole("button", { name: /GitHub/ }).tap();
    await page.locator("select.github-inbox-repository").waitFor();
    assert.deepEqual(await smallControls(page), [], `${name}: GitHub`);
    await bar.getByRole("button", { name: "Home", exact: true }).tap();
    await page.locator(".sidebar .channel-block").getByText("ui-review", { exact: true }).tap();
    const rootRow = page.locator(`.conversation [data-message-id="${root.id}"]`);
    await rootRow.waitFor();
    await page.locator(".tabs button").filter({ hasText: "Tasks" }).tap();
    await page.getByRole("combobox", { name: "Task #1 status", exact: true }).waitFor();
    assert.deepEqual(await smallControls(page), [], `${name}: Tasks`);
    await page.locator(".tabs button").filter({ hasText: "Chat" }).tap();

    // 3. A followed thread stays on its latest reply while the reply box grows
    //    and while the mention list is open. WebKit does not keep it there itself.
    await rootRow.locator(".thread-reply-summary").tap();
    const thread = page.locator(".thread");
    await thread.locator(`[data-message-id="${replies.at(-1).id}"]`).waitFor();
    await waitForThreadBottom(page, `${name}, thread opened`);
    const replyBox = thread.locator(".reply-composer textarea");
    const height = () => replyBox.evaluate(element => element.getBoundingClientRect().height);
    await replyBox.tap();
    const oneLine = await height();
    for (const line of ["one", "two", "three", "four", "five"]) { await page.keyboard.type(line); await page.keyboard.press("Shift+Enter"); }
    await page.waitForFunction(([selector, before]) => document.querySelector(selector).getBoundingClientRect().height > before + 40, [".thread .reply-composer textarea", oneLine]);
    await waitForThreadBottom(page, `${name}, five-line reply`);
    await replyBox.fill("");
    await page.keyboard.type("@");
    await page.locator(".mention-picker").waitFor();
    await waitForThreadBottom(page, `${name}, mention list open`);
    await replyBox.fill("");
    await page.locator(".mention-picker").waitFor({ state: "detached" });
    await replyBox.evaluate(element => element.blur());

    // 4. Tapping a thread message lists its actions; tapping it again puts them away.
    const row = thread.locator(`[data-message-id="${target.id}"]`);
    const actions = row.locator(".message-tap-actions");
    assert.equal(await page.locator(".message-tap-actions").count(), 0, `${name}: no actions until a message is tapped`);
    await row.locator(".markdown-body").tap();
    await actions.waitFor();
    assert.deepEqual(await actions.getByRole("button").allTextContents(), ["Quote", "Copy link", "Save"]);
    for (const box of await actions.getByRole("button").evaluateAll(buttons => buttons.map(button => button.getBoundingClientRect().height))) {
      assert.ok(box >= 44, `${name}: an action is ${box}px tall`);
    }
    await actions.getByRole("button", { name: "Save", exact: true }).tap();
    await actions.getByRole("button", { name: "Saved", exact: true }).waitFor();
    await row.locator(".mobile-message-save-tag").waitFor();
    assert.deepEqual(requests.filter(request => request.path === "/api/set_message_saved").map(request => request.args), [{ messageId: target.id, saved: true }]);
    await actions.getByRole("button", { name: "Quote", exact: true }).tap();
    await page.waitForFunction(([selector, token]) => document.querySelector(selector).value.includes(token), [".thread .reply-composer textarea", `[[message:${target.id}]]`]);
    await actions.getByRole("button", { name: "Copy link", exact: true }).tap();
    await actions.getByRole("button", { name: "Copied", exact: true }).waitFor();
    assert.equal(await actions.count(), 1, `${name}: using an action keeps the actions open`);
    await row.locator(".markdown-body").tap();
    await actions.waitFor({ state: "detached" });
    // Another message takes the actions over.
    const other = thread.locator(`[data-message-id="${replies[12].id}"]`);
    await other.locator(".markdown-body").tap();
    await other.locator(".message-tap-actions").waitFor();
    assert.equal(await page.locator(".message-tap-actions").count(), 1);
    assert.deepEqual(errors, [], `${name}: no page errors`);
    await browser.close(); browser = null;
  }

  // Desktop keeps its hover actions and context menu; a click only highlights the message.
  state = freshState();
  browser = await chromium.launch();
  const page = await (await browser.newContext({ viewport: { width: 1280, height: 820 }, serviceWorkers: "block" })).newPage();
  page.setDefaultTimeout(8000);
  await page.goto(origin, { waitUntil: "domcontentloaded" });
  const rootRow = page.locator(`.conversation [data-message-id="${root.id}"]`);
  await rootRow.waitFor();
  if (!(await page.locator(".thread").count())) { await rootRow.hover(); await rootRow.getByRole("button", { name: "View replies", exact: true }).click(); }
  const row = page.locator(`.thread [data-message-id="${target.id}"]`);
  await row.locator(".markdown-body").click();
  await page.locator(`.thread [data-message-id="${target.id}"].tap-focused`).waitFor();
  assert.equal(await page.locator(".message-tap-actions").count(), 0, "desktop shows no tap actions");
  console.log("phone controls: ok");
} finally {
  await browser?.close();
  await new Promise(done => { api.closeAllConnections?.(); api.close(done); });
}
