// Production UI with isolated API/SSE: system noise, lazy Activity hydration,
// read state and navigation to the first unread agent reply. No live data.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile, mkdir } from "node:fs/promises";
import { resolve, join } from "node:path";
import { chromium, webkit } from "playwright";

const id = n => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const base = Date.now() - 90_000;
const at = n => new Date(base + n * 1000).toISOString();
const channelId = id(801), dmId = id(802), systemId = id(803), readChannelId = id(804);
const agent = { id: id(800), handle: "arch", display_name: "arch", role: "agent", status: "idle", runtime: "codex",
  model: "", reasoning_effort: "", service_tier: "", avatar: "A", description: "", launch_command: "", environment_variables: "",
  working_directory: "", workspace_exists: false, workspace_memory_path: "", workspace_memory_exists: false,
  workspace_entries: [], details_loaded: true, daily_budget_micros: 0, subscription_status: null };
const message = (n, role, body, extra = {}) => ({ id: id(n), seq: n, channel_id: channelId, thread_root_id: null,
  sender_agent_id: !["owner", "system"].includes(role) ? agent.id : null, sender_name: !["owner", "system"].includes(role) ? "arch" : role === "owner" ? "Dylan" : "Lantor",
  sender_role: role, body, is_task: false, thread_followed: true, delivery_state: "complete", stream_key: "",
  task_number: null, task_status: null, attachments: [], artifacts: [], created_at: at(n), updated_at: at(n), ...extra });
const root = message(10, "system", "Reminder thread root");
const reply = message(20, "Architecture steward", "Agent investigation complete", { thread_root_id: root.id });
const readRoot = message(60, "system", "Task thread root", { channel_id: readChannelId, is_task: true });
const readReply = message(61, "agent", "Previously read agent result", { channel_id: readChannelId, thread_root_id: readRoot.id });
const initialMessages = [root, reply,
  message(30, "system", "Reminder: @Dylan system mention", { thread_root_id: root.id }),
  message(40, "owner", "Owner follow-up", { thread_root_id: root.id }),
  message(45, "system", "System-only task notification", { channel_id: systemId, is_task: true }),
  message(50, "agent", "Agent direct response", { channel_id: dmId }),
  message(51, "system", "DM system notification", { channel_id: dmId }), readRoot, readReply,
  message(62, "system", "New reminder after read agent result", { channel_id: readChannelId, thread_root_id: readRoot.id })];
const initialChannels = [
  { id: channelId, name: "agent-results", unread_count: 2, agent_unread_count: 1 },
  { id: dmId, name: "dm:arch", kind: "dm", dm_agent_id: agent.id, unread_count: 2, agent_unread_count: 1 },
  { id: systemId, name: "system-only", unread_count: 1, agent_unread_count: 0 },
  { id: readChannelId, name: "read-results", unread_count: 1, agent_unread_count: 0 },
].map(c => ({ kind: "channel", dm_agent_id: null, description: "", github_unread_count: 0, github_review_synced_at: null, ...c }));
const initialActivities = [
  { thread_root_id: root.id, channel_id: channelId, reply_count: 3, unread_count: 2, agent_unread_count: 1,
    first_unread_agent_message_id: reply.id, latest_message_id: id(40), latest_activity_at: at(40) },
  { thread_root_id: readRoot.id, channel_id: readChannelId, reply_count: 2, unread_count: 1, agent_unread_count: 0,
    first_unread_agent_message_id: null, latest_message_id: id(62), latest_activity_at: at(62) },
];
const initialState = { db_url: "synthetic://activity", web_base_url: null, owner_profile: { display_name: "Dylan", avatar: "D", description: "" },
  channels: initialChannels, messages: [], channel_message_history: [], agents: [agent], channel_members: [],
  tasks: [{ id: id(900), number: 1, message_id: readRoot.id, channel_id: readChannelId, channel_name: "read-results",
    title: "Synthetic task card", status: "in_review", assignee_id: agent.id, assignee_name: "arch", created_at: at(60), updated_at: at(70), version: 1 }],
  reminders: [{ id: id(901), title: "Synthetic due reminder card", note: "System reminder note", status: "fired",
    channel_id: channelId, thread_root_id: root.id, message_id: id(30), due_at: at(30), fired_at: at(30) }],
  thread_activities: initialActivities, saved_messages: [], dismissed_inbox_items: {}, read_inbox_items: {}, artifacts: [],
  agent_runs: [], agent_work_items: [], agent_activities: [], supervisor: { pid: null, status: "stopped", updated_at: null },
  launch_agent: { label: "", plist_path: "", installed: false, loaded: false }, ui_event_cursor: 0 };
// Agent replies counted in the channel's unread totals that no receipt has covered yet.
let state, messages, cursor = 0, emulateThreadReceipts = false, receiptableReplies = 0;
const clients = new Set(), requests = [];
const until = async (check, message) => {
  for (const end = Date.now() + 5_000; Date.now() < end; await new Promise(done => setTimeout(done, 50))) {
    const value = check(); if (value) return value;
  }
  throw new Error(message);
};
const publish = event => { cursor++; for (const client of clients) client.write(`id: ${cursor}\nevent: lantor\ndata: ${JSON.stringify(event)}\n\n`); };
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
    res.writeHead(200, { "content-type": "text/event-stream" }); res.write(": ready\n\n"); clients.add(res); res.on("close", () => clients.delete(res)); return;
  }
  let raw = ""; for await (const chunk of req) raw += chunk;
  const args = raw ? JSON.parse(raw) : {}; requests.push({ path: url.pathname, args });
  let result = { ok: true };
  switch (url.pathname) {
    case "/api/bootstrap": result = { ...state, ui_event_cursor: cursor }; break;
    case "/api/load_channel_previews": result = []; break;
    case "/api/load_activity_messages": result = messages.filter(m => !["owner", "system"].includes(m.sender_role) || [root.id, readRoot.id].includes(m.id)); break;
    case "/api/load_message": result = messages.find(m => m.id === args.messageId); break;
    case "/api/load_channel_messages": result = { messages: [], next_before_seq: null, has_more: false }; break;
    case "/api/load_thread_messages": result = messages.filter(m => m.id === args.threadRootId || m.thread_root_id === args.threadRootId); break;
    case "/api/load_ui_state": result = Object.fromEntries(args.scopes.map(scope => [scope, state[scope]])); break;
    case "/api/replay_ui_events": result = { cursor, replayGap: false, events: [] }; break;
    case "/api/mark_inbox_items_read":
      for (const item of args.items) state.read_inbox_items[item.itemId] = item.dismissedUntil;
      if (emulateThreadReceipts) {
        // Like the backend: a thread receipt clears replies up to its cutoff and refreshes read state.
        const covered = a => args.items.some(item => item.itemId === `thread:${a.thread_root_id}`
          && Date.parse(item.dismissedUntil) >= Date.parse(a.latest_activity_at));
        // The channel totals drop the receipted replies too.
        for (const a of state.thread_activities.filter(covered)) {
          state.channels = state.channels.map(c => c.id === a.channel_id ? { ...c,
            unread_count: c.unread_count - receiptableReplies, agent_unread_count: c.agent_unread_count - receiptableReplies } : c);
          receiptableReplies = 0;
        }
        state.thread_activities = state.thread_activities.map(a => covered(a)
          ? { ...a, unread_count: 0, agent_unread_count: 0, first_unread_agent_message_id: null } : a);
        setTimeout(() => publish({ type: "refresh", reason: "owner_inbox_read" }), 0);
      }
      break;
    case "/api/mark_channel_read":
      state.channels = state.channels.map(c => c.id === args.channelId ? { ...c, unread_count: 0, agent_unread_count: 0 } : c);
      state.thread_activities = state.thread_activities.map(a => a.channel_id === args.channelId
        ? { ...a, unread_count: 0, agent_unread_count: 0, first_unread_agent_message_id: null } : a);
      break;
  }
  res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(result));
});
await new Promise(done => api.listen(0, "127.0.0.1", done));
let browser;
try {
  for (const [name, engine, width] of [["desktop", chromium, 1440], ["mobile", webkit, 390]]) {
    state = structuredClone(initialState); messages = structuredClone(initialMessages); requests.length = 0; emulateThreadReceipts = false;
    receiptableReplies = 0;
    browser = await engine.launch();
    const page = await browser.newPage({ serviceWorkers: "block", viewport: { width, height: 900 } });
    page.setDefaultTimeout(10_000);
    const errors = []; page.on("pageerror", error => errors.push(error.message));
    await page.goto(`http://127.0.0.1:${api.address().port}`, { waitUntil: "domcontentloaded" });
    const trigger = page.locator(width < 760 ? ".mobile-bottom-nav" : ".sidebar").getByRole("button", { name: /^Activity/ });
    await trigger.click();
    const dialog = page.getByRole("dialog", { name: "Activity", exact: true });
    const rows = dialog.locator(".activity-feed-row");
    await dialog.getByRole("button", { name: "Open Agent investigation complete", exact: true }).waitFor();
    assert.equal(await rows.count(), 3, "agent thread/DM/read result only after lazy hydration");
    assert.equal(await rows.filter({ hasText: "Previously read agent result" }).locator(".activity-feed-unread-dot").count(), 0);
    assert.equal(await dialog.getByRole("button", { name: "Tasks", exact: true }).count(), 0);
    assert.equal(await dialog.getByRole("button", { name: "Reminders", exact: true }).count(), 0);
    assert.equal(await rows.filter({ hasText: /Synthetic|Reminder|notification|Owner follow-up/ }).count(), 0);
    assert.equal(await rows.locator(".activity-feed-unread-dot").count(), 2);
    await dialog.getByRole("button", { name: "Mentions", exact: true }).click();
    await dialog.getByText("Nothing new", { exact: true }).waitFor();
    await dialog.getByRole("button", { name: "All", exact: true }).click();
    const target = rows.filter({ hasText: "Agent investigation complete" });
    if (width < 760) await dialog.getByRole("button", { name: "Mark all read", exact: true }).click();
    else await target.getByRole("button", { name: "Mark read", exact: true }).click();
    await target.locator(".activity-feed-unread-dot").waitFor({ state: "detached" });

    const notification = message(95, "system", "Reminder: @Dylan live noise", { thread_root_id: root.id });
    messages.push(notification);
    state.channels = state.channels.map(c => c.id === channelId ? { ...c, unread_count: 1 } : c);
    state.thread_activities = state.thread_activities.map(a => a.thread_root_id === root.id
      ? { ...a, unread_count: 1, latest_message_id: notification.id, latest_activity_at: notification.created_at } : a);
    publish({ type: "message_upsert", message: notification });
    await page.waitForTimeout(800);
    assert.equal(await target.locator(".activity-feed-unread-dot").count(), 0, "system event cannot revive a read agent result");
    assert.equal(await dialog.locator(".activity-feed-new-activity").count(), 0, "system event cannot move or add feed items");
    assert.equal(await rows.filter({ hasText: "live noise" }).count(), 0);

    const first = message(99, "agent", "First unread agent reply", { thread_root_id: root.id });
    const fresh = message(100, "agent", "Fresh agent result", { thread_root_id: root.id });
    messages.push(first, fresh);
    state.channels = state.channels.map(c => c.id === channelId ? { ...c, unread_count: 3, agent_unread_count: 2 } : c);
    state.thread_activities = state.thread_activities.map(a => a.thread_root_id === root.id
      ? { ...a, unread_count: 3, agent_unread_count: 2, first_unread_agent_message_id: first.id,
        latest_message_id: fresh.id, latest_activity_at: fresh.created_at } : a);
    publish({ type: "message_upsert", message: fresh });
    await dialog.getByRole("button", { name: "1 new activity", exact: true }).click();
    const latest = rows.filter({ hasText: "Fresh agent result" });
    await latest.getByText("2 new", { exact: true }).waitFor();
    if (process.env.LANTOR_UI_SCREENSHOTS) {
      await mkdir(process.env.LANTOR_UI_SCREENSHOTS, { recursive: true });
      await page.screenshot({ path: join(process.env.LANTOR_UI_SCREENSHOTS, `activity-agent-${name}.png`) });
    }
    await latest.getByRole("button", { name: "Open Fresh agent result", exact: true }).click();
    await dialog.waitFor({ state: "detached" });
    await page.locator(`.thread [data-message-id="${first.id}"]`).waitFor();
    assert.ok(requests.some(r => r.path === "/api/load_message" && r.args.messageId === first.id), "opens first unread agent reply absent from the initial snapshot");

    if (width >= 1101) {
      // An expanded thread hides the conversation, whose read watermark was the
      // only receipt for replies arriving in the open thread.
      await page.getByRole("button", { name: "Full width", exact: true }).click();
      await page.locator(".app.thread-expanded").waitFor();
      emulateThreadReceipts = true;
      // Unfollowed, the thread leaves Activity once read, so a channel entry
      // for the same replies would show up in its place.
      const unfollowed = { ...messages.find(m => m.id === root.id), thread_followed: false };
      messages = messages.map(m => m.id === root.id ? unfollowed : m);
      publish({ type: "message_upsert", message: unfollowed });
      const badge = trigger.locator(".unread-badge");
      const badgeValue = async () => await badge.count() ? Number(await badge.textContent()) : 0;
      await page.waitForTimeout(300);
      const before = await badgeValue();
      const late = message(110, "agent", "Late reply in the expanded thread", { thread_root_id: root.id });
      messages.push(late);
      // Like the backend, channel totals count thread replies above the watermark.
      state.channels = state.channels.map(c => c.id === channelId
        ? { ...c, unread_count: c.unread_count + 1, agent_unread_count: c.agent_unread_count + 1 } : c);
      receiptableReplies = 1;
      state.thread_activities = state.thread_activities.map(a => a.thread_root_id === root.id
        ? { ...a, reply_count: a.reply_count + 1, unread_count: 1, agent_unread_count: 1, first_unread_agent_message_id: late.id,
          latest_message_id: late.id, latest_activity_at: late.created_at } : a);
      const sent = requests.length;
      publish({ type: "message_upsert", message: late });
      await page.locator(`.thread [data-message-id="${late.id}"]`).waitFor();
      await until(() => requests.slice(sent).some(r => r.path === "/api/mark_inbox_items_read"
        && r.args.items.some(item => item.itemId === `thread:${root.id}` && item.dismissedUntil === late.created_at)),
        "expanded thread receipts its new reply, fenced to the snapshot's latest activity");
      for (const end = Date.now() + 5_000; await badgeValue() !== before && Date.now() < end;) await page.waitForTimeout(50);
      assert.equal(await badgeValue(), before, "Activity unread clears while the thread stays expanded");
      await trigger.click();
      await dialog.waitFor();
      assert.equal(await dialog.locator(".activity-feed-unread-dot").count(), before,
        "no unread channel entry replaces the read thread");
      await page.keyboard.press("Escape");
      await dialog.waitFor({ state: "detached" });
      assert.equal(requests.slice(sent).filter(r => r.path === "/api/mark_channel_read").length, 0, "hidden conversation is not marked read");
    }
    assert.equal(errors.length, 0, errors.join("\n"));
    console.log(`PASS ${name}: agent-only hydration, no system cards/mentions/unread, live reply, first-unread navigation`);
    await browser.close(); browser = null;
  }
} finally {
  await browser?.close(); for (const client of clients) client.end(); await new Promise(done => api.close(done));
}
