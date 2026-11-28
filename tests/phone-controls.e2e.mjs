// Production App against an isolated API, on a phone: the bottom bar badges
// leave their icons visible, no form control is small enough for iOS to zoom
// the page, a followed thread stays on its latest reply while the reply box
// grows, the layout covers what the keyboard leaves visible, a tapped
// thread message offers Quote, Copy link and Save, a swipe from the left edge
// drags the thread back and finishes or snaps back like a native one,
// pages slide in and out like a navigation stack, an opened channel's
// messages rise into place once they sit at the latest message, and neither
// the thread nor the channel pans sideways.
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
// A reply links a pull request under a label wider than the screen.
const linkReply = replies[3];
linkReply.body = "Background: [org/repo#101 (keeps the toolbar on one line at 320 px with every button at 44 pt)](https://github.com/org/repo/pull/101)";
// A second channel long enough to scroll, which loads only when opened.
const notesId = id(1001);
const notes = Array.from({ length: 20 }, (_, index) => message(100 + index, { channel_id: notesId,
  ...(index % 2 ? { sender_agent_id: agent.id, sender_name: "Hancock", sender_role: "agent" } : {}), body: `Note ${index + 1}` }));
const channel = (id, name) => ({ id, name, kind: "channel", dm_agent_id: null, description: "", unread_count: 0,
  github_unread_count: 0, github_review_synced_at: null });
const channels = [channel(channelId, "ui-review"), channel(notesId, "ui-notes")];
const notification = (threadId, repository) => ({ thread_id: String(threadId), repository, subject_type: "PullRequest",
  title: `Pull request ${threadId}`, number: threadId, url: `https://github.com/${repository}/pull/${threadId}`, reason: "review_requested",
  unread: true, updated_at: now, thread_root_id: null, channel_id: null });
const githubInbox = { enabled: true, account_login: "octo", synced_at: now, error: null,
  items: [notification(101, "org/repo"), notification(102, "org/docs"), notification(103, "org/repo")] };
const freshState = () => ({ db_url: "synthetic://phone-controls", web_base_url: null, owner_profile: { display_name: "Dylan", avatar: "D", description: "" },
  channels, messages: [root, ...replies, ...notes], channel_message_history: [{ channel_id: channelId, before_seq: null, has_more: false }],
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
// The second channel's first page waits until the test releases it.
let notesLoaded = Promise.resolve(), releaseNotes = () => {};
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
    case "/api/load_channel_messages":
      if (args.channelId === notesId) await notesLoaded;
      result = { messages: state.messages.filter(m => m.channel_id === args.channelId), next_before_seq: null, has_more: false }; break;
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

// These engines show no on-screen keyboard. Stand in for Safari's: the visual
// viewport shrinks by the keyboard and pans down to show the focused field,
// while the layout viewport keeps its size.
function fakeKeyboard() {
  const viewport = new EventTarget();
  let keyboard = 0, pan = 0;
  Object.defineProperties(viewport, {
    width: { get: () => window.innerWidth }, height: { get: () => window.innerHeight - keyboard },
    offsetLeft: { get: () => 0 }, offsetTop: { get: () => pan }, pageLeft: { get: () => 0 },
    pageTop: { get: () => pan + window.scrollY }, scale: { get: () => 1 },
  });
  Object.defineProperty(window, "visualViewport", { configurable: true, get: () => viewport });
  window.setKeyboard = (height, panned) => {
    keyboard = height; pan = panned;
    viewport.dispatchEvent(new Event("resize"));
    viewport.dispatchEvent(new Event("scroll"));
  };
}
// Touches the app's edge-swipe handlers see; each call is one task, so
// consecutive calls get increasing timestamps.
const touches = (page, selector, steps) => page.evaluate(([selector, steps]) => {
  const target = document.querySelector(selector);
  for (const [type, x] of steps) {
    const event = new Event(type, { bubbles: true, cancelable: true });
    Object.defineProperty(event, "touches", { value: type === "touchend" ? [] : [{ clientX: x, clientY: 400 }] });
    target.dispatchEvent(event);
  }
}, [selector, steps]);
const left = (page, selector) => page.locator(selector).first().evaluate(element => Math.round(element.getBoundingClientRect().left) || 0);
const swipeSettled = page => page.waitForFunction(() => !document.querySelector(".app").dataset.mobileSwipe);
const box = (page, selector) => page.locator(selector).first().evaluate(element => {
  const rect = element.getBoundingClientRect();
  return { top: Math.round(rect.top), bottom: Math.round(rect.bottom) };
});

let browser;
try {
  for (const [name, engine] of Object.entries({ chromium, webkit })) {
    state = freshState(); requests = [];
    notesLoaded = new Promise(done => { releaseNotes = done; });
    browser = await engine.launch();
    const context = await browser.newContext({ viewport: { width: 393, height: 852 }, isMobile: true, hasTouch: true, serviceWorkers: "block" });
    await context.addInitScript(fakeKeyboard);
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

    //    The bar and Home are opaque, so nothing is blurred behind them while
    //    the list scrolls under the bar or Home slides.
    const blur = selector => page.locator(selector).first().evaluate(element => getComputedStyle(element).backdropFilter);
    assert.equal(await blur(".mobile-bottom-nav"), "none", `${name}: the bottom bar blurs nothing behind it`);
    assert.equal(await blur(".sidebar"), "none", `${name}: Home blurs nothing behind it`);

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

    // 3. The thread's replies are asked for when the finger lands on its reply
    //    summary, and opening it on release takes that request.
    const pageRequests = () => requests.filter(request => request.path === "/api/load_thread_page").length;
    const pagesBefore = pageRequests();
    // The list stays hidden for a moment after the Chat tab returns, until its
    // reading position is restored.
    await rootRow.locator(".thread-reply-summary").waitFor({ state: "visible" });
    const summaryBox = await rootRow.locator(".thread-reply-summary").boundingBox();
    await page.mouse.move(summaryBox.x + summaryBox.width / 2, summaryBox.y + summaryBox.height / 2);
    await page.mouse.down();
    for (let tries = 0; tries < 100 && pageRequests() === pagesBefore; tries += 1) await new Promise(done => setTimeout(done, 20));
    assert.equal(pageRequests(), pagesBefore + 1, `${name}: the replies are asked for when the finger lands`);
    assert.equal(await page.locator(".thread").count(), 0, `${name}: the thread opens only on release`);
    await page.mouse.up();
    //    A followed thread stays on its latest reply while the reply box grows
    //    and while the mention list is open. WebKit does not keep it there itself.
    const thread = page.locator(".thread");
    await thread.locator(`[data-message-id="${replies.at(-1).id}"]`).waitFor();
    assert.equal(pageRequests(), pagesBefore + 1, `${name}: opening the thread reuses the request`);
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

    // 4. With the keyboard up the layout covers what stays visible: the headers
    //    at its top, the reply boxes right on the keyboard, the latest reply in view.
    await replyBox.tap();
    await page.evaluate(() => window.setKeyboard(336, 336));
    await page.waitForFunction(() => Math.round(document.querySelector(".thread").getBoundingClientRect().top) === 336)
      .catch(async () => assert.fail(`${name}: the thread stays under the keyboard ${JSON.stringify(await box(page, ".thread"))}`));
    assert.deepEqual(await box(page, ".thread"), { top: 336, bottom: 852 }, `${name}: the thread fills the visible part`);
    assert.equal((await box(page, ".thread .reply-composer")).bottom, 852, `${name}: the reply box sits on the keyboard`);
    assert.equal((await box(page, ".conversation .topbar")).top, 336, `${name}: the channel header stays visible`);
    assert.equal((await box(page, ".conversation .composer")).bottom, 852, `${name}: the channel composer sits on the keyboard`);
    await waitForThreadBottom(page, `${name}, keyboard up`);
    await page.evaluate(() => window.setKeyboard(0, 0));
    await replyBox.evaluate(element => element.blur());
    await page.waitForFunction(() => Math.round(document.querySelector(".thread").getBoundingClientRect().bottom) === 852);
    assert.deepEqual(await box(page, ".thread"), { top: 0, bottom: 852 }, `${name}: the thread fills the screen again`);
    await waitForThreadBottom(page, `${name}, keyboard down`);

    // 5. Tapping a thread message lists its actions; tapping it again puts them away.
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

    // 6. A swipe from the left edge drags the thread back over the channel,
    //    which slides in from a third of the way back.
    const inThread = ".thread .thread-scroll";
    await touches(page, inThread, [["touchstart", 5]]);
    await page.waitForTimeout(120); await touches(page, inThread, [["touchmove", 50]]);
    await page.waitForTimeout(120); await touches(page, inThread, [["touchmove", 95]]);
    await page.waitForFunction(() => Math.round(document.querySelector(".thread").getBoundingClientRect().left) === 90);
    const underneath = await left(page, ".conversation");
    assert.ok(underneath < -60 && underneath > -0.3 * 393, `${name}: the channel slides in under the thread, at ${underneath}`);
    //    A short drag released after the finger stopped snaps back.
    await page.waitForTimeout(150); await touches(page, inThread, [["touchend", 95]]);
    await swipeSettled(page);
    assert.equal(await left(page, ".thread"), 0, `${name}: a short slow drag snaps back`);
    //    However far the finger got, a flick back towards the edge cancels.
    await touches(page, inThread, [["touchstart", 5]]);
    for (const x of [80, 160, 240]) { await page.waitForTimeout(40); await touches(page, inThread, [["touchmove", x]]); }
    await page.waitForTimeout(150);
    for (const x of [220, 190, 160]) await touches(page, inThread, [["touchmove", x]]);
    await touches(page, inThread, [["touchend", 160]]);
    await swipeSettled(page);
    assert.equal(await left(page, ".thread"), 0, `${name}: a flick back keeps the thread open`);
    //    A quick short flick finishes: the thread slides out, then closes.
    await touches(page, inThread, [["touchstart", 5]]);
    await touches(page, inThread, [["touchmove", 30]]);
    await touches(page, inThread, [["touchmove", 60]]);
    await touches(page, inThread, [["touchend", 60]]);
    assert.equal(await page.locator(".thread").count(), 1, `${name}: the thread slides out instead of vanishing`);
    await page.waitForFunction(() => (document.querySelector(".thread")?.getBoundingClientRect().left ?? 393) > 200);
    await page.locator(".thread").waitFor({ state: "detached" });
    await swipeSettled(page);
    assert.equal(await left(page, ".conversation"), 0, `${name}: the channel is back in place`);
    //    In a channel, the same swipe goes Home.
    const inChannel = ".conversation .message-list";
    await touches(page, inChannel, [["touchstart", 5]]);
    for (const x of [100, 200, 260]) { await page.waitForTimeout(40); await touches(page, inChannel, [["touchmove", x]]); }
    await page.waitForTimeout(150); await touches(page, inChannel, [["touchend", 260]]);
    await page.waitForFunction(() => document.querySelector(".app").classList.contains("mobile-sidebar-open"));
    await swipeSettled(page);
    assert.equal(await left(page, ".sidebar"), 0, `${name}: Home is in place`);

    // 7. A channel slides in over Home, which waits a third of the way back,
    //    and a thread slides in over the channel. Back slides each one out
    //    before it closes.
    const third = -Math.round(0.3 * 393);
    const animating = selector => page.locator(selector).first().evaluate(element => element.getAnimations().length > 0);
    const settledAt = (selector, x) => page.waitForFunction(([selector, x]) => {
      const element = document.querySelector(selector);
      return element && (Math.round(element.getBoundingClientRect().left) || 0) === x && element.getAnimations().length === 0;
    }, [selector, x]).catch(async () => assert.fail(`${name}: ${selector} did not settle at ${x}, it is at ${await left(page, selector)}`));
    await page.locator(".sidebar .channel-block").getByText("ui-review", { exact: true }).tap();
    assert.ok(await animating(".conversation"), `${name}: the channel slides in`);
    await settledAt(".conversation", 0);
    await settledAt(".sidebar", third);
    await rootRow.locator(".thread-reply-summary").tap();
    assert.ok(await animating(".thread"), `${name}: the thread slides in`);
    await settledAt(".thread", 0);
    await settledAt(".conversation", third);
    await thread.getByRole("button", { name: "Back to channel", exact: true }).tap();
    assert.equal(await page.locator(".thread").count(), 1, `${name}: Back slides the thread out before it closes`);
    await page.locator(".thread").waitFor({ state: "detached" });
    await swipeSettled(page);
    await settledAt(".conversation", 0);
    await page.locator(".conversation").getByRole("button", { name: "Back", exact: true }).tap();
    assert.ok(await animating(".sidebar"), `${name}: Home slides back`);
    await settledAt(".sidebar", 0);
    //    With reduced motion nothing slides: Back closes the thread at once.
    await page.emulateMedia({ reducedMotion: "reduce" });
    await page.locator(".sidebar .channel-block").getByText("ui-review", { exact: true }).tap();
    assert.equal(await animating(".conversation"), false, `${name}: no slide with reduced motion`);
    await rootRow.locator(".thread-reply-summary").tap();
    assert.equal(await animating(".thread"), false, `${name}: no slide in with reduced motion`);
    await thread.getByRole("button", { name: "Back to channel", exact: true }).tap();
    await page.locator(".thread").waitFor({ state: "detached", timeout: 200 });
    await page.emulateMedia({ reducedMotion: null });

    // 8. A channel opening for the first time says it is on its way only
    //    after a pause, since a loaded one is placed within a frame or two.
    //    Its messages then rise into place from the reply box, already at the
    //    latest message: the scroller moves, so its scroll position does not.
    const opened = () => page.waitForFunction(() => {
      const list = document.querySelector(".conversation .message-list");
      if (!list || list.hasAttribute("data-restoring")) return null;
      return { animations: list.getAnimations().map(animation => animation.animationName),
        fromLatest: Math.round(list.scrollHeight - list.scrollTop - list.clientHeight) };
    }).then(handle => handle.jsonValue());
    await page.locator(".conversation").getByRole("button", { name: "Back", exact: true }).tap();
    await settledAt(".sidebar", 0);
    await page.locator(".sidebar .channel-block").getByText("ui-notes", { exact: true }).tap();
    const notice = page.locator(".message-list-restoring");
    await notice.waitFor();
    assert.equal(await notice.evaluate(element => getComputedStyle(element).opacity), "0", `${name}: no notice at once`);
    await page.waitForFunction(() => getComputedStyle(document.querySelector(".message-list-restoring")).opacity === "1");
    releaseNotes();
    const rising = await opened();
    assert.deepEqual(rising.animations, ["message-list-rise"], `${name}: the messages rise into place`);
    assert.ok(rising.fromLatest <= 2, `${name}: the list rises already at the latest message, ${rising.fromLatest}px away`);
    await page.waitForFunction(() => document.querySelector(".conversation .message-list").getAnimations().length === 0);
    const list = page.locator(".conversation .message-list");
    assert.equal(await list.evaluate(element => getComputedStyle(element).transform), "none", `${name}: the list comes to rest in place`);
    assert.ok(await list.evaluate(element => element.scrollHeight - element.scrollTop - element.clientHeight <= 2), `${name}: the list rests at the latest message`);
    assert.ok(await list.evaluate(element => element.scrollHeight > element.clientHeight + 100), `${name}: the opened channel scrolls`);
    //    With reduced motion the messages appear in place.
    await page.emulateMedia({ reducedMotion: "reduce" });
    await page.locator(".conversation").getByRole("button", { name: "Back", exact: true }).tap();
    await page.locator(".sidebar .channel-block").getByText("ui-review", { exact: true }).tap();
    assert.deepEqual((await opened()).animations, [], `${name}: no rise with reduced motion`);
    await page.emulateMedia({ reducedMotion: null });

    // 9. A long GitHub link label wraps inside its message, so nothing in the
    //    thread runs past the screen. Whatever still would is clipped: neither
    //    the thread nor the channel pans sideways under a horizontal scroll.
    await rootRow.locator(".thread-reply-summary").tap();
    await settledAt(".thread", 0);
    const linkRow = thread.locator(`[data-message-id="${linkReply.id}"]`);
    await linkRow.evaluate(row => { const scroll = row.closest(".thread-scroll"); scroll.scrollTop += row.getBoundingClientRect().top - scroll.getBoundingClientRect().top - 80; });
    const fit = await linkRow.locator("a.github-reference").evaluate(link => {
      const scroll = link.closest(".thread-scroll");
      return { overflow: Math.round(link.getBoundingClientRect().right - scroll.getBoundingClientRect().right),
        lines: link.getClientRects().length, scrollWidth: scroll.scrollWidth, clientWidth: scroll.clientWidth };
    });
    assert.ok(fit.overflow <= 0 && fit.lines > 1, `${name}: the long link wraps inside the thread ${JSON.stringify(fit)}`);
    assert.ok(fit.scrollWidth <= fit.clientWidth, `${name}: nothing in the thread is wider than it ${JSON.stringify(fit)}`);
    // A diagonal scroll up over a block wider and taller than the screen:
    //    once it has moved the list up, see how far it moved it sideways.
    const pannedBy = async selector => {
      const scroller = page.locator(selector);
      // The list may follow its latest message as the block grows it; wait
      // until it holds still for a few frames.
      const top = await scroller.evaluate(element => new Promise(done => {
        const wide = document.createElement("div");
        wide.className = "wide-probe"; wide.style.cssText = "width: 900px; height: 2000px";
        element.firstElementChild.append(wide);
        let last = -1, still = 0;
        const check = () => {
          still = element.scrollTop === last ? still + 1 : 0; last = element.scrollTop;
          if (still >= 3) done(last); else requestAnimationFrame(check);
        };
        check();
      }));
      const box = await scroller.boundingBox();
      await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
      await page.mouse.wheel(300, -300);
      await scroller.evaluate((element, top) => new Promise(done => {
        const check = () => element.scrollTop <= top - 100 ? done() : requestAnimationFrame(check);
        check();
      }), top);
      return scroller.evaluate(element => {
        const left = element.scrollLeft;
        element.querySelector(".wide-probe").remove();
        return left;
      });
    };
    // Mobile WebKit takes no wheel input; the rule is the same in both engines.
    if (name === "chromium") assert.equal(await pannedBy(".thread .thread-scroll"), 0, `${name}: the thread does not pan sideways`);
    await thread.getByRole("button", { name: "Back to channel", exact: true }).tap();
    await page.locator(".thread").waitFor({ state: "detached" });
    await settledAt(".conversation", 0);
    if (name === "chromium") assert.equal(await pannedBy(".conversation .message-list"), 0, `${name}: the channel does not pan sideways`);
    assert.deepEqual(errors, [], `${name}: no page errors`);
    await browser.close(); browser = null;
  }

  // Desktop keeps its hover actions and context menu; a click only highlights the message.
  state = freshState(); notesLoaded = Promise.resolve();
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
  assert.notEqual(await page.locator(".sidebar").evaluate(element => getComputedStyle(element).backdropFilter), "none", "the desktop sidebar keeps its frosted look");
  assert.equal(await page.locator(".conversation .message-list").evaluate(element => getComputedStyle(element).animationName), "none", "desktop channels open without the phone's rise");
  console.log("phone controls: ok");
} finally {
  await browser?.close();
  await new Promise(done => { api.closeAllConnections?.(); api.close(done); });
}
