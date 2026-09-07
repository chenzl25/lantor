// Production App against an isolated API: panel state, task workflows and app modals.
// Run npm run build first. No live workspace data is read or mutated.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile, mkdir } from "node:fs/promises";
import { resolve, join } from "node:path";
import { chromium, webkit } from "playwright";
const id = n => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const now = new Date().toISOString(), channelId = id(1000), dmId = id(1001);
const agent = { id: id(800), handle: "Hancock", display_name: "Hancock", role: "agent", status: "idle", runtime: "codex",
  model: "", reasoning_effort: "", service_tier: "", avatar: "H", description: "", launch_command: "", environment_variables: "",
  working_directory: "", workspace_exists: false, workspace_memory_path: "", workspace_memory_exists: false,
  workspace_entries: [], details_loaded: true, daily_budget_micros: 0, subscription_status: null };
const message = (n, extra = {}) => ({ id: id(n), seq: n, channel_id: channelId, thread_root_id: null, sender_agent_id: null,
  sender_name: "Dylan", sender_role: "owner", body: `Discussion ${n}`, is_task: false, thread_followed: false,
  delivery_state: "complete", stream_key: "", task_number: null, task_status: null, attachments: [], artifacts: [], created_at: now, updated_at: now, ...extra });
const root = message(1), reply = message(2, { thread_root_id: root.id, body: "Thread reply" });
const channels = [{ id: channelId, name: "ui-review", kind: "channel", dm_agent_id: null },
  { id: dmId, name: `dm:${agent.id}`, kind: "dm", dm_agent_id: agent.id }].map(c => ({ ...c, description: "", unread_count: 0, github_unread_count: 0, github_review_synced_at: null }));
const initialTasks = ["in_review", "in_review", "in_progress", "todo", "done"].map((status, index) => ({ id: id(900 + index), number: index + 1,
  message_id: root.id, channel_id: channelId, title: index === 0 ? "Web: agent_runs / agent_work_items 按需加载，保留完整任务标题以及所有验收条件和状态变化说明" : `Review the ${status.replace(/_/g, " ")} workflow`,
  status, version: 1, channel_name: "ui-review", assignee_id: index === 3 ? null : agent.id, assignee_name: index === 3 ? null : "Hancock", created_at: now, updated_at: now }));
const state = { db_url: "synthetic://ui-panels", web_base_url: null, owner_profile: { display_name: "Dylan", avatar: "D", description: "" },
  channels, messages: [root, reply, message(3, { channel_id: dmId, body: "Direct conversation" })],
  channel_message_history: channels.map(c => ({ channel_id: c.id, before_seq: null, has_more: false })),
  agents: [agent], tasks: structuredClone(initialTasks), channel_members: [{ channel_id: channelId, agent_id: agent.id }],
  thread_activities: [], saved_messages: [], dismissed_inbox_items: {}, read_inbox_items: {}, artifacts: [], reminders: [], agent_schedules: [],
  agent_runs: [], agent_work_items: [], agent_activities: [], supervisor: { pid: null, status: "stopped", updated_at: null },
  launch_agent: { label: "", plist_path: "", installed: false, loaded: false }, ui_event_cursor: 0 };
let cursor = 0;
const clients = new Set(), requests = [];
const publish = event => { cursor++; for (const client of clients) client.write(`id: ${cursor}\nevent: lantor\ndata: ${JSON.stringify(event)}\n\n`); };
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
  if (url.pathname === "/api/events") {
    res.writeHead(200, { "content-type": "text/event-stream" }); res.write(": ready\n\n"); clients.add(res); res.on("close", () => clients.delete(res)); return;
  }
  let raw = ""; for await (const chunk of req) raw += chunk;
  const args = raw ? JSON.parse(raw) : {}; requests.push({ path: url.pathname, args });
  let result = { ok: true };
  switch (url.pathname) {
    case "/api/bootstrap": result = { ...state, ui_event_cursor: cursor }; break;
    case "/api/load_channel_previews": case "/api/load_activity_messages": case "/api/search_messages": result = []; break;
    case "/api/load_channel_messages": result = { messages: state.messages.filter(m => m.channel_id === args.channelId), next_before_seq: null, has_more: false }; break;
    case "/api/load_thread_messages": result = state.messages.filter(m => m.id === args.threadRootId || m.thread_root_id === args.threadRootId); break;
    case "/api/load_ui_state": result = Object.fromEntries(args.scopes.map(scope => [scope, state[scope]])); break;
    case "/api/replay_ui_events": result = { cursor, replayGap: false, events: [] }; break;
    case "/api/open_dm_with_agent": result = dmId; break;
    case "/api/update_task_status": state.tasks = state.tasks.map(t => t.id === args.taskId ? { ...t, status: args.status } : t); publish({ type: "refresh", reason: "task_status_updated" }); break;
    case "/api/update_task_title": state.tasks = state.tasks.map(t => t.id === args.taskId ? { ...t, title: args.title } : t); publish({ type: "refresh", reason: "task_title_updated" }); break;
    case "/api/claim_task": state.tasks = state.tasks.map(t => t.id === args.taskId ? { ...t, assignee_id: args.agentId } : t); publish({ type: "refresh", reason: "task_claimed" }); break;
  }
  res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(result));
});
await new Promise(done => api.listen(0, "127.0.0.1", done));
let browser;
try {
  for (const [name, engine] of Object.entries({ chromium, webkit })) {
    state.tasks = structuredClone(initialTasks);
    state.messages = state.messages.map(message => message.id === reply.id ? reply : message);
    browser = await engine.launch();
    const context = await browser.newContext({ serviceWorkers: "block", viewport: { width: 1440, height: 960 } });
    const page = await context.newPage(); page.setDefaultTimeout(8000);
    const errors = []; page.on("pageerror", error => errors.push(error.message));
    await page.goto(`http://127.0.0.1:${api.address().port}`, { waitUntil: "domcontentloaded" });
    const row = page.locator(`.conversation [data-message-id="${root.id}"]`);
    await row.waitFor(); assert.equal(await page.locator(".thread").count(), 0, "initial channel has no empty thread column");
    const openThread = async () => {
      if (page.viewportSize().width <= 760) await row.locator(".markdown-body").click();
      else { await row.hover(); await row.getByRole("button", { name: "View thread replies", exact: true }).click(); }
      await page.locator(".thread").waitFor();
    };
    await openThread();
    await page.getByRole("button", { name: "Open DM with @Hancock", exact: true }).click();
    await page.locator(".dm-conversation").waitFor();
    await page.locator('.sidebar .channel').filter({ hasText: "ui-review" }).click();
    await page.getByRole("button", { name: "Close thread panel" }).waitFor();
    await page.getByRole("button", { name: "Close thread panel" }).click(); await page.locator(".thread").waitFor({ state: "detached" });
    await page.reload(); await row.waitFor(); assert.equal(await page.locator(".thread").count(), 0, "closing is remembered across reload");
    await page.getByRole("button", { name: "Tasks", exact: true }).click();
    assert.equal(await page.locator(".task-queue-section").first().getAttribute("aria-label"), "In review tasks");
    assert.equal(await page.getByRole("button", { name: "Done 1", exact: true }).getAttribute("aria-expanded"), "false");
    assert.equal(await page.locator(".task-row").count(), 4);
    const capture = async label => { if (process.env.LANTOR_UI_SCREENSHOTS) { await mkdir(process.env.LANTOR_UI_SCREENSHOTS, { recursive: true }); await page.screenshot({ path: join(process.env.LANTOR_UI_SCREENSHOTS, `${label}-${name}.png`) }); } };
    for (const width of [1440, 1024, 390]) {
      await page.setViewportSize({ width, height: 960 });
      await page.waitForTimeout(150);
      assert.ok(await page.getByRole("textbox", { name: "Task #1 title", exact: true }).evaluate(e => e.scrollHeight <= e.clientHeight + 1 && e.scrollWidth <= e.clientWidth + 1), "long title is fully visible");
      assert.ok(await page.locator(".task-board").evaluate(e => e.scrollWidth <= e.clientWidth + 1), "task board has no horizontal overflow");
      await capture(`tasks-${width}-light`);
    }
    await page.setViewportSize({ width: 1440, height: 960 });
    await page.getByRole("button", { name: "In review tasks", exact: true }).click(); assert.equal(await page.locator(".task-row").count(), 2);
    await page.getByRole("button", { name: "Unassigned tasks", exact: true }).click(); assert.equal(await page.locator(".task-row").count(), 1);
    await page.getByRole("button", { name: "Total tasks", exact: true }).click();
    await page.getByRole("button", { name: "Done 1", exact: true }).click();
    await page.getByRole("combobox", { name: "Task #5 status", exact: true }).selectOption("in_progress");
    await page.waitForFunction(() => document.querySelector('select[aria-label="Task #5 status"]')?.value === "in_progress");
    await page.getByRole("button", { name: "Assign task #4", exact: true }).click();
    await page.getByRole("option", { name: /Hancock/ }).click();
    await page.waitForTimeout(200);
    assert.ok(requests.some(r => r.path === "/api/claim_task" && r.args.taskId === id(903) && r.args.agentId === agent.id));
    const title = page.getByRole("textbox", { name: "Task #1 title", exact: true });
    const edited = "Edited long task title remains editable"; await title.fill(edited); await title.press("Enter");
    await page.waitForTimeout(200);
    assert.ok(requests.some(r => r.path === "/api/update_task_title" && r.args.title === edited));
    await page.getByRole("button", { name: "Chat", exact: true }).click();
    await page.getByRole("button", { name: "Open DM with @Hancock", exact: true }).click();
    await page.locator(".dm-conversation").waitFor();
    assert.equal(await page.locator(".conversation .tabs").count(), 0);
    const composer = page.getByRole("button", { name: "Send message", exact: true });
    assert.ok(await composer.evaluate(e => { const r = e.getBoundingClientRect(); return r.top > 0 && r.bottom <= innerHeight; }), "DM composer stays in view");
    await page.getByRole("button", { name: "Open settings", exact: true }).focus();
    await page.keyboard.press("Enter");
    const settings = page.getByRole("dialog", { name: "Settings", exact: true }); await settings.waitFor();
    await settings.getByRole("button", { name: "Dark", exact: true }).click();
    await page.evaluate(() => window.dispatchEvent(new CustomEvent("lantor-ui-error", { detail: "Could not open attachment: permission denied" })));
    await page.getByRole("alert").filter({ hasText: "Could not open attachment" }).waitFor();
    await page.waitForTimeout(200);
    await capture("settings-dark-error");
    await page.getByRole("button", { name: "Dismiss notification" }).click();
    await page.keyboard.press("Control+k");
    const search = page.getByRole("dialog", { name: "Search", exact: true }); await search.waitFor();
    await page.keyboard.press("Escape"); await search.waitFor({ state: "detached" }); assert.equal(await settings.count(), 1, "one Escape closes only the top app dialog");
    await page.keyboard.press("Escape"); await settings.waitFor({ state: "detached" });
    assert.equal(await page.evaluate(() => document.activeElement?.getAttribute("aria-label")), "Open settings");
    for (const [button, label] of [["Activity", "Activity"], ["Saved", "Saved messages"]]) {
      const trigger = page.locator(".sidebar").getByRole("button", { name: new RegExp(`^${button}`) });
      await trigger.focus(); await page.keyboard.press("Enter"); const dialog = page.getByRole("dialog", { name: label, exact: true }); await dialog.waitFor();
      await page.keyboard.press("Tab"); assert.ok(await dialog.evaluate(e => e.contains(document.activeElement)));
      await page.keyboard.press("Escape"); await dialog.waitFor({ state: "detached" });
      assert.equal(await trigger.evaluate(e => e === document.activeElement), true, `${label} returns focus`);
    }
    await page.locator('.sidebar .channel').filter({ hasText: "ui-review" }).click();
    await page.getByRole("button", { name: "Tasks", exact: true }).click(); await capture("tasks-desktop-dark");
    await page.getByRole("button", { name: "Chat", exact: true }).click();
    await page.setViewportSize({ width: 390, height: 844 });
    await openThread();
    await capture("thread-mobile-before-reference");
    await page.getByRole("button", { name: "Thread actions", exact: true }).click();
    await capture("thread-mobile-menu");
    await page.getByRole("menuitem", { name: "Reference thread", exact: true }).click();
    await page.waitForTimeout(200);
    assert.equal(await page.locator(".thread").count(), 1, "reference kind must not become another fullscreen panel");
    const preview = page.locator(".reply-composer .message-reference-stack");
    const referenceGeometry = await preview.evaluate(e => {
      const rect = e.getBoundingClientRect(), composer = e.closest(".reply-composer").getBoundingClientRect();
      const token = document.querySelector(".reply-composer .composer-reference-token");
      return { height: rect.height, withinComposer: rect.top >= composer.top && rect.bottom <= composer.bottom,
        tokenPosition: getComputedStyle(token).position, tokenDisplay: getComputedStyle(token).display };
    });
    assert.ok(referenceGeometry.height < 150 && referenceGeometry.withinComposer, "preview stays inside the reply composer");
    assert.equal(referenceGeometry.tokenPosition, "static"); assert.equal(referenceGeometry.tokenDisplay, "inline");
    assert.ok(await page.locator(".thread-scroll").evaluate(e => e.clientHeight > 200), "conversation stays visible after inserting reference");
    assert.ok((await page.locator(".reply-composer textarea").inputValue()).includes(`[[thread:${root.id}]]`));
    await capture("thread-mobile-referenced");
    await page.locator(".reply-composer textarea").evaluate(e => { e.focus(); e.setSelectionRange(e.value.length, e.value.length); });
    await page.keyboard.insertText("Keep this unsaved reply");
    await preview.getByRole("button", { name: "Remove thread reference", exact: true }).click();
    await preview.waitFor({ state: "detached" });
    assert.equal((await page.locator(".reply-composer textarea").inputValue()).trim(), "Keep this unsaved reply");
    await page.locator(".reply-composer textarea").fill("");
    await page.locator(".reply-composer textarea").blur();
    await page.getByRole("button", { name: "Back to channel", exact: true }).click();
    await page.locator(".thread").waitFor({ state: "detached" });
    const channelComposer = page.locator(".conversation .composer textarea");
    await channelComposer.fill(`Context [[thread:${root.id}]] and [[message:${reply.id}]]`);
    assert.equal(await page.locator(".thread").count(), 0, "a channel draft reference must not create a thread panel");
    await page.getByRole("button", { name: "Remove thread reference", exact: true }).click();
    assert.equal(await channelComposer.inputValue(), `Context  and [[message:${reply.id}]]`);
    await channelComposer.fill(""); await channelComposer.blur();
    const referencedReply = { ...reply, body: `Attached [[thread:${root.id}]]` };
    state.messages = state.messages.map(message => message.id === reply.id ? referencedReply : message);
    publish({ type: "message_upsert", message: referencedReply });
    await openThread();
    const inline = page.locator(".reply-list .message-reference-card.reference-thread"); await inline.waitFor();
    assert.equal(await page.locator(".thread").count(), 1);
    assert.ok(await inline.evaluate(e => e.getBoundingClientRect().height < 80 && getComputedStyle(e).position !== "fixed"));
    assert.deepEqual(errors, []);
    console.log(`${name}: empty/restored/closed/mobile thread, DM composer, wrapping task titles at 1440/1024/390, status groups/filters/edit/reopen, app modal layering/history/focus, mobile thread reference insert/remove/paste/inline layout passed`);
    await browser.close(); browser = null;
  }
} finally { await browser?.close(); for (const client of clients) client.end(); api.closeAllConnections(); await new Promise(done => api.close(done)); }
