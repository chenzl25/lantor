// Production App against an isolated API: where back, forward and the in-app
// close buttons land. Desktop writes one browser history entry per view;
// mobile writes none and navigates in-app. Run npm run build first.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { chromium, webkit } from "playwright";

const id = n => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const now = new Date().toISOString();
const alphaId = id(1000), betaId = id(1001), dmId = id(1002);
const agent = { id: id(800), handle: "Hancock", display_name: "Hancock", role: "agent", status: "idle", runtime: "codex",
  model: "", reasoning_effort: "", service_tier: "", avatar: "H", description: "", launch_command: "", environment_variables: "",
  working_directory: "", workspace_exists: false, workspace_memory_path: "", workspace_memory_exists: false,
  workspace_entries: [], details_loaded: true, daily_budget_micros: 0, subscription_status: null };
const message = (n, channel_id, body, extra = {}) => ({ id: id(n), seq: n, channel_id, thread_root_id: null, sender_agent_id: null,
  sender_name: "Dylan", sender_role: "owner", body, is_task: false, thread_followed: false, delivery_state: "complete",
  stream_key: "", task_number: null, task_status: null, attachments: [], artifacts: [], created_at: now, updated_at: now, ...extra });
const alphaRoot = message(1, alphaId, "Alpha root");
const betaRoot = message(3, betaId, "Beta root");
const betaQuote = message(5, betaId, `Compare [[message:${alphaRoot.id}]] with [[thread:${alphaRoot.id}]]`);
const betaLocalQuote = message(7, betaId, `Recall [[message:${betaRoot.id}]]`);
const messages = [alphaRoot, message(2, alphaId, "Alpha reply", { thread_root_id: alphaRoot.id }),
  betaRoot, message(4, betaId, "Beta reply", { thread_root_id: betaRoot.id }), betaQuote, message(6, dmId, "Direct conversation"), betaLocalQuote];
const channels = [{ id: alphaId, name: "alpha", kind: "channel", dm_agent_id: null },
  { id: betaId, name: "beta", kind: "channel", dm_agent_id: null },
  { id: dmId, name: `dm:${agent.id}`, kind: "dm", dm_agent_id: agent.id }]
  .map(c => ({ ...c, description: "", unread_count: 0, github_unread_count: 0, github_review_synced_at: null }));
const state = { db_url: "synthetic://navigation-history", web_base_url: null, owner_profile: { display_name: "Dylan", avatar: "D", description: "" },
  channels, messages, channel_message_history: channels.map(c => ({ channel_id: c.id, before_seq: null, has_more: false })),
  agents: [agent], tasks: [], channel_members: [{ channel_id: alphaId, agent_id: agent.id }, { channel_id: betaId, agent_id: agent.id }],
  thread_activities: [], saved_messages: [], dismissed_inbox_items: {}, read_inbox_items: {}, artifacts: [], reminders: [],
  agent_runs: [], agent_work_items: [], agent_activities: [], decisions: [], supervisor: { pid: null, status: "stopped", updated_at: null },
  launch_agent: { label: "", plist_path: "", installed: false, loaded: false }, ui_event_cursor: 0 };

const eventClients = new Set();
let eventCursor = 0;
function publish(event) {
  eventCursor += 1;
  for (const client of eventClients) client.write(`id: ${eventCursor}\nevent: lantor\ndata: ${JSON.stringify(event)}\n\n`);
}

const api = createServer(async (req, res) => {
  const url = new URL(req.url, "http://localhost");
  if (!url.pathname.startsWith("/api/")) {
    const file = resolve("dist", url.pathname === "/" ? "index.html" : url.pathname.slice(1));
    if (!file.startsWith(resolve("dist") + "/")) { res.writeHead(404).end(); return; }
    try {
      const mime = { html: "text/html", js: "application/javascript", css: "text/css", png: "image/png", woff: "font/woff", woff2: "font/woff2" };
      res.writeHead(200, { "content-type": mime[file.split(".").pop()] || "application/octet-stream" }).end(await readFile(file));
    } catch { res.writeHead(404).end(); }
    return;
  }
  if (url.pathname === "/api/events") {
    res.writeHead(200, { "content-type": "text/event-stream" }); res.write(": ready\n\n");
    eventClients.add(res); res.on("close", () => eventClients.delete(res)); return;
  }
  let raw = "";
  // The browser drops in-flight requests on reload; reading one throws.
  try { for await (const chunk of req) raw += chunk; } catch { return; }
  const args = raw ? JSON.parse(raw) : {};
  let result = { ok: true };
  switch (url.pathname) {
    case "/api/bootstrap": result = state; break;
    case "/api/load_channel_previews": case "/api/load_activity_messages": case "/api/search_messages": result = []; break;
    case "/api/load_channel_messages": result = { messages: messages.filter(m => m.channel_id === args.channelId), next_before_seq: null, has_more: false }; break;
    case "/api/load_thread_page": result = { messages: messages.filter(m => m.id === args.threadRootId || m.thread_root_id === args.threadRootId), next_before_seq: null, has_more: false }; break;
    case "/api/load_message": result = messages.find(m => m.id === args.messageId); break;
    case "/api/load_ui_state": result = Object.fromEntries(args.scopes.map(scope => [scope, state[scope]])); break;
    case "/api/replay_ui_events": result = { cursor: eventCursor, replayGap: false, events: [] }; break;
    case "/api/open_dm_with_agent": result = dmId; break;
  }
  res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(result));
});
await new Promise(done => api.listen(0, "127.0.0.1", done));
const appUrl = `http://127.0.0.1:${api.address().port}`;

// What the user sees, plus the history entry the app wrote for it.
const view = page => page.evaluate(() => {
  const thread = document.querySelector(".thread")?.textContent ?? null;
  return {
    channel: document.querySelector(".conversation .channel-title h1")?.textContent ?? null,
    tab: document.querySelector(".tabs button.active")?.textContent.trim().toLowerCase() ?? null,
    thread: thread === null ? null : thread.includes("Alpha reply") ? "alpha" : thread.includes("Beta reply") ? "beta" : "other",
    agent: Boolean(document.querySelector(".agent-drawer")),
    modal: [...document.querySelectorAll("[role=dialog]")]
      .map(d => d.getAttribute("aria-label") ?? document.getElementById(d.getAttribute("aria-labelledby"))?.textContent).join(",") || null,
    home: document.querySelector("main.app")?.classList.contains("mobile-sidebar-open") ?? false,
    jumped: [...document.querySelectorAll(".conversation [data-jump-focused=true]")]
      .map(row => row.textContent.includes("Compare") ? "quote" : row.textContent.includes("Alpha root") ? "alpha"
        : row.textContent.includes("Beta root") ? "beta" : "other").join(",") || null,
    index: history.state?.index ?? null,
    entries: history.length,
    back: document.querySelector("[aria-label='Go back']")?.disabled === false,
    forward: document.querySelector("[aria-label='Go forward']")?.disabled === false,
  };
});

async function expectView(page, expected, label) {
  const deadline = Date.now() + 4000;
  let actual;
  while (Date.now() < deadline) {
    actual = await view(page);
    if (Object.entries(expected).every(([key, value]) => actual[key] === value)) return;
    await page.waitForTimeout(50);
  }
  assert.deepEqual(Object.fromEntries(Object.keys(expected).map(key => [key, actual[key]])), expected, label);
}

async function desktop(page, name) {
  await page.goto(appUrl, { waitUntil: "domcontentloaded" });
  const alphaRow = page.locator(`.conversation [data-message-id="${alphaRoot.id}"]`);
  await alphaRow.waitFor();
  const sidebarRow = label => page.locator(".sidebar .channel").filter({ hasText: label }).first();
  const goBackButton = page.getByRole("button", { name: "Go back", exact: true });
  const goForwardButton = page.getByRole("button", { name: "Go forward", exact: true });
  const search = page.getByRole("dialog", { name: "Search", exact: true });
  await expectView(page, { channel: "alpha", thread: null, index: 0, back: false, forward: false }, "start: one base entry");
  const entries = (await view(page)).entries;

  // Each view change is one entry.
  await alphaRow.hover(); await alphaRow.getByRole("button", { name: "View replies", exact: true }).click();
  await expectView(page, { channel: "alpha", thread: "alpha", index: 1, back: true }, "opening a thread");
  await sidebarRow("beta").click();
  await expectView(page, { channel: "beta", thread: "beta", index: 2 }, "a first visit opens the channel's thread in the same entry");
  await page.getByRole("button", { name: "Close thread", exact: true }).click();
  await expectView(page, { channel: "beta", thread: null, index: 2, forward: false }, "closing a thread replaces its entry");
  await page.locator(".tabs").getByRole("button", { name: "Tasks" }).click();
  await expectView(page, { tab: "tasks", index: 3 }, "switching tabs");
  await page.locator(".sidebar").getByRole("button", { name: "@Hancock’s profile", exact: true }).click();
  await expectView(page, { tab: "tasks", agent: true, index: 4 }, "opening an agent");
  await sidebarRow("Search").click();
  await expectView(page, { agent: true, modal: "Search", index: 5, entries: entries + 5 }, "opening a panel");

  // Browser back, Go back and Mod+[ walk the same entries.
  await page.goBack();
  await expectView(page, { agent: true, modal: null, index: 4, forward: true }, "back closes the panel");
  await goBackButton.click();
  await expectView(page, { tab: "tasks", agent: false, index: 3 }, "back closes the agent");
  await page.keyboard.press("ControlOrMeta+BracketLeft");
  await expectView(page, { channel: "beta", tab: "chat", thread: null, index: 2 }, "back lands on the replaced entry");
  await page.goBack();
  await expectView(page, { channel: "alpha", thread: "alpha", index: 1 }, "back returns to the other channel's thread");
  await goBackButton.click();
  await expectView(page, { channel: "alpha", thread: null, index: 0, back: false, forward: true }, "back to the base entry");
  await page.goForward();
  await expectView(page, { channel: "alpha", thread: "alpha", index: 1 }, "forward reopens the thread");
  await goForwardButton.click();
  await expectView(page, { channel: "beta", thread: null, index: 2 }, "forward to the replaced entry");
  await page.keyboard.press("ControlOrMeta+BracketRight");
  await expectView(page, { tab: "tasks", index: 3 }, "forward to the tab");
  await page.goForward();
  await expectView(page, { agent: true, modal: null, index: 4 }, "forward reopens the agent");
  await page.goForward();
  await expectView(page, { agent: true, modal: "Search", index: 5, forward: false, entries: entries + 5 }, "forward to the last entry");

  // Closing a panel or the agent (Close or Escape) steps back, so forward reopens it.
  await page.keyboard.press("Escape");
  await expectView(page, { agent: true, modal: null, index: 4, forward: true }, "Escape closes the panel by going back");
  await page.goForward();
  await expectView(page, { modal: "Search", index: 5 }, "forward reopens the panel");
  await search.getByRole("button", { name: "Close search", exact: true }).click();
  await expectView(page, { agent: true, modal: null, index: 4, forward: true }, "the panel's Close goes back");
  await page.locator(".agent-drawer").getByRole("button", { name: "Close", exact: true }).click();
  await expectView(page, { agent: false, tab: "tasks", index: 3, forward: true }, "the agent's Close goes back");
  await page.locator(".sidebar").getByRole("button", { name: "@Hancock’s profile", exact: true }).click();
  await expectView(page, { agent: true, index: 4, forward: false }, "reopening the agent");
  await page.keyboard.press("Escape");
  await expectView(page, { agent: false, tab: "tasks", index: 3, forward: true }, "Escape closes the agent by going back");

  // A new view drops the forward entries.
  await page.locator(".tabs").getByRole("button", { name: "Chat" }).click();
  await expectView(page, { tab: "chat", index: 4, forward: false, entries: entries + 4 }, "a new view after going back");

  // Reload keeps the position and adds nothing.
  await page.reload(); await page.locator(".conversation [data-message-id]").first().waitFor();
  await expectView(page, { index: 4, back: true, entries: entries + 4 }, "reload keeps the history position");
  await page.goBack();
  await expectView(page, { channel: "beta", tab: "tasks", index: 3 }, "back after reload");

  // Switching straight to another panel replaces the panel's entry.
  await sidebarRow("Activity").click();
  await expectView(page, { modal: "Activity", index: 4, entries: entries + 4 }, "opening a panel after going back");
  await page.keyboard.press("ControlOrMeta+k");
  await expectView(page, { modal: "Search", index: 4, entries: entries + 4 }, "Mod+K swaps the panel in place");
  await page.goBack();
  await expectView(page, { modal: null, tab: "tasks", index: 3 }, "back closes the swapped panel");
  console.log(`${name}: desktop back/forward/close/reload/panel swap passed`);
}

async function references(page, name) {
  await page.goto(appUrl, { waitUntil: "domcontentloaded" });
  await page.locator(`.conversation [data-message-id="${alphaRoot.id}"]`).waitFor();
  await page.locator(".sidebar .channel").filter({ hasText: "beta" }).first().click();
  await page.getByRole("button", { name: "Close thread", exact: true }).click();
  await expectView(page, { channel: "beta", thread: null, index: 1 }, "start in the quoting channel");
  const entries = (await view(page)).entries;
  const quote = page.locator(`.conversation [data-message-id="${betaQuote.id}"]`);

  // Opening a quote is one entry, and Back returns to the quoting message.
  await quote.locator(".message-reference-card.reference-message").click();
  await expectView(page, { channel: "alpha", thread: null, jumped: "alpha", index: 2, entries: entries + 1 }, "a quoted message is one entry");
  await page.goBack();
  await expectView(page, { channel: "beta", thread: null, jumped: "quote", index: 1 }, "one Back returns to the quote");
  await page.goForward();
  await expectView(page, { channel: "alpha", thread: null, index: 2, forward: false }, "Forward returns to the quoted message");
  await page.goBack();
  await expectView(page, { channel: "beta", index: 1 }, "back at the quote");
  await quote.locator(".message-reference-card.reference-thread").click();
  await expectView(page, { channel: "alpha", thread: "alpha", index: 2, entries: entries + 1 }, "a quoted thread is one entry");
  await page.goBack();
  await expectView(page, { channel: "beta", thread: null, index: 1 }, "one Back closes the quoted thread");

  // A quote of a message in the same view: Back changes nothing on screen, and
  // the next view still gets its own entry.
  const localQuote = page.locator(`.conversation [data-message-id="${betaLocalQuote.id}"]`);
  await localQuote.locator(".message-reference-card.reference-message").click();
  await expectView(page, { channel: "beta", thread: null, jumped: "beta", index: 2 }, "a quote in the same channel is one entry");
  await page.goBack();
  await expectView(page, { channel: "beta", thread: null, index: 1 }, "Back returns to the quote in the same channel");
  const betaRow = page.locator(`.conversation [data-message-id="${betaRoot.id}"]`);
  await betaRow.hover(); await betaRow.getByRole("button", { name: "View replies", exact: true }).click();
  await expectView(page, { channel: "beta", thread: "beta", index: 2 }, "the next view after that Back is its own entry");
  await page.goBack();
  await expectView(page, { channel: "beta", thread: null, index: 1 }, "Back closes that thread");
  console.log(`${name}: desktop quote navigation passed`);
}

async function mobile(page, name) {
  await page.goto(appUrl, { waitUntil: "domcontentloaded" });
  await page.locator(".sidebar .channel").filter({ hasText: "beta" }).first().waitFor();
  const sidebarRow = label => page.locator(".sidebar .channel").filter({ hasText: label }).first();
  const headerBack = page.locator(".conversation").getByRole("button", { name: "Back", exact: true });
  const bottomNav = () => page.getByRole("navigation", { name: "Primary mobile navigation" }).last();
  await expectView(page, { home: true, index: null }, "mobile opens on Home");
  const entries = (await view(page)).entries;

  // In-app navigation only; the browser history never grows.
  await sidebarRow("beta").click();
  await expectView(page, { channel: "beta", thread: null, home: false, index: null, entries }, "a channel");
  await page.locator(`.conversation [data-message-id="${betaRoot.id}"] .markdown-body`).click();
  await expectView(page, { channel: "beta", thread: "beta", index: null, entries }, "tapping a message opens its thread");
  await page.getByRole("button", { name: "Back to channel", exact: true }).click();
  await expectView(page, { channel: "beta", thread: null, home: false, entries }, "thread Back returns to the channel");
  await headerBack.click();
  await expectView(page, { home: true, entries }, "channel Back returns Home");
  await page.locator(".sidebar").getByRole("button", { name: "@Hancock’s profile", exact: true }).click();
  await expectView(page, { agent: true, home: false, entries }, "an agent from Home");
  await page.locator(".agent-drawer").getByRole("button", { name: "Back", exact: true }).click();
  await expectView(page, { agent: false, home: true, entries }, "agent Back returns Home when opened from Home");
  await page.locator(".sidebar").getByRole("button", { name: "Message @Hancock", exact: true }).click();
  await expectView(page, { channel: "Hancock", home: false, entries }, "the agent's DM");
  await page.locator(".conversation").getByRole("button", { name: "@Hancock’s profile", exact: true }).click();
  await expectView(page, { agent: true, home: false, entries }, "the agent from its DM");
  await page.locator(".agent-drawer").getByRole("button", { name: "Back", exact: true }).click();
  await expectView(page, { agent: false, channel: "Hancock", home: false, entries }, "agent Back returns to the channel it was opened from");
  await headerBack.click();
  await expectView(page, { home: true, entries }, "Home again");
  await sidebarRow("alpha").click();
  await page.locator(".tabs").getByRole("button", { name: "Tasks" }).click();
  await expectView(page, { channel: "alpha", tab: "tasks", home: false, entries }, "a tab");

  // The bottom bar swaps panels in place and Home closes them.
  await bottomNav().getByRole("button", { name: "Search", exact: true }).click();
  await expectView(page, { modal: "Search", entries }, "Search from the bottom bar");
  await bottomNav().getByRole("button", { name: /Activity/ }).click();
  await expectView(page, { modal: "Activity", entries }, "Activity replaces Search");
  await bottomNav().getByRole("button", { name: "Home", exact: true }).click();
  await expectView(page, { modal: null, home: true, index: null, entries }, "Home closes the panel");
  await sidebarRow("alpha").click();
  await expectView(page, { channel: "alpha", tab: "tasks", home: false, index: null, entries }, "the channel keeps its tab");
  console.log(`${name}: mobile in-app navigation passed`);
}

// A turn ends with progress updates still buffered, which land as a transition
// update, and then non-transition run and work-item updates. While the
// transition is pending, every sync render re-applies those later updates into
// a new data object, so the mobile history effect (it reruns on data) must
// not set state each run: that looped until React gave up (error #185).
async function mobileTurnEnd(page, name) {
  await page.goto(appUrl, { waitUntil: "domcontentloaded" });
  await page.locator(".sidebar .channel").filter({ hasText: "alpha" }).first().click();
  await expectView(page, { channel: "alpha", home: false, index: null }, "a channel before the turn ends");
  const at = () => new Date().toISOString();
  for (let turn = 0; turn < 40; turn += 1) {
    publish({ type: "activity_upsert", reason: "activity", activity: { id: id(5000 + turn), agent_id: agent.id,
      agent_handle: agent.handle, run_id: id(6000 + turn), kind: "tools", phase: "event", status: "active",
      title: "Running command", summary: "", detail: "", metadata: {}, created_at: at() } });
    await page.waitForTimeout(turn % 20);
    publish({ type: "work_item_upsert", reason: "work_item_finished", work_item: { id: id(7000 + turn), agent_id: agent.id,
      agent_handle: agent.handle, channel_id: alphaId, channel_name: "alpha", thread_root_id: null, source_message_id: null,
      task_id: null, task_number: null, source_kind: "mention", title: "Reply", status: "completed", run_id: id(6000 + turn),
      created_at: at(), updated_at: at(), completed_at: at() } });
  }
  await page.waitForTimeout(500);
  assert.equal(await page.locator(".fatal-shell").count(), 0, `${name}: a turn ending on a phone leaves the app running`);
  await expectView(page, { channel: "alpha", home: false, index: null }, "the channel after the turn ends");
  console.log(`${name}: mobile turn end passed`);
}

// WebKit logs a request the app starts while the page unloads for a reload as
// "Fetch API cannot load <url> due to access control checks.", even when the
// app handles the rejection, and Playwright reports that log as a page error.
const interruptedFetch = error => error.name.startsWith("Fetch API cannot load ")
  && error.message.includes("/api/") && error.message.endsWith(" due to access control checks.");

let browser;
try {
  for (const [name, engine] of Object.entries({ chromium, webkit })) {
    browser = await engine.launch();
    for (const [run, viewport] of [[desktop, { width: 1440, height: 960 }], [references, { width: 1440, height: 960 }], [mobile, { width: 390, height: 844 }], [mobileTurnEnd, { width: 393, height: 793 }]]) {
      const context = await browser.newContext({ serviceWorkers: "block", viewport });
      const page = await context.newPage(); page.setDefaultTimeout(8000);
      const errors = []; page.on("pageerror", error => { if (!interruptedFetch(error)) errors.push(error.message); });
      await run(page, name);
      assert.deepEqual(errors, [], `${name} ${run.name}: no page errors`);
      await context.close();
    }
    await browser.close(); browser = null;
  }
} finally {
  await browser?.close();
  api.close();
}
