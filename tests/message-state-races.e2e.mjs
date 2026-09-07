// Production UI and real SSE against an isolated API. Never uses live workspace data.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { chromium, webkit } from "playwright";

const id = n => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const now = new Date().toISOString(), channelId = id(1), agentId = id(2);
let seq = 10;
const message = (body, extra = {}) => ({ id: id(++seq), seq, channel_id: channelId, thread_root_id: null,
  sender_agent_id: null, sender_name: "Test owner", sender_role: "owner", body, is_task: false,
  thread_followed: true, delivery_state: "complete", stream_key: "", task_number: null, task_status: null,
  attachments: [], artifacts: [], created_at: now, updated_at: now, ...extra });
const root = message("Open race test thread");
const agent = { id: agentId, handle: "RaceAgent", display_name: "Race Agent", role: "agent", status: "running",
  runtime: "codex", model: "", reasoning_effort: "", service_tier: "", avatar: "R", description: "",
  launch_command: "", environment_variables: "", working_directory: "", workspace_exists: false,
  workspace_memory_path: "", workspace_memory_exists: false, workspace_entries: [], details_loaded: true,
  daily_budget_micros: 0, subscription_status: null };
const run = (status, extra = {}) => ({ id: id(3), agent_id: agentId, work_item_id: null, command: "fixture",
  working_directory: "", status, pid: null, exit_code: null, log: "", started_at: now,
  stopped_at: status === "running" ? null : now, input_tokens: 0, output_tokens: 0, cost_micros: 0, ...extra });
const state = { db_url: "synthetic://message-state-races", web_base_url: null,
  owner_profile: { display_name: "Test owner", avatar: "T", description: "" },
  channels: [{ id: channelId, name: "race-test", description: "", kind: "channel", dm_agent_id: null,
    unread_count: 0, github_unread_count: 0, github_review_synced_at: null }],
  messages: [root, message("Existing reply", { thread_root_id: root.id }), message("Agent status fixture", { sender_agent_id: agentId, sender_name: agent.display_name, sender_role: "agent" })],
  channel_message_history: [{ channel_id: channelId, before_seq: null, has_more: false }],
  agents: [agent], agent_runs: [run("running")], thread_activities: [], channel_members: [], saved_messages: [],
  dismissed_inbox_items: {}, read_inbox_items: {}, tasks: [], artifacts: [], reminders: [], agent_schedules: [],
  agent_work_items: [], agent_activities: [], supervisor: { pid: null, status: "stopped", updated_at: null },
  launch_agent: { label: "", plist_path: "", installed: false, loaded: false }, ui_event_cursor: 0 };
const clients = new Set(), events = [], requests = [], pendingSends = [];
let heldAgentRead = null;
const publish = event => {
  const delivery = { cursor: events.length + 1, event: JSON.stringify(event) };
  events.push(delivery);
  for (const client of clients) client.write(`id: ${delivery.cursor}\nevent: lantor\ndata: ${delivery.event}\n\n`);
};
const api = createServer(async (req, res) => {
  const url = new URL(req.url, "http://localhost");
  if (!url.pathname.startsWith("/api/")) {
    const file = resolve("dist", url.pathname === "/" ? "index.html" : url.pathname.slice(1));
    if (!file.startsWith(resolve("dist") + "/")) { res.writeHead(404).end(); return; }
    try {
      const mime = { html: "text/html", js: "application/javascript", css: "text/css", woff2: "font/woff2", svg: "image/svg+xml" };
      res.writeHead(200, { "content-type": mime[file.split(".").pop()] || "application/octet-stream" }).end(await readFile(file));
    } catch { res.writeHead(404).end(); }
    return;
  }
  if (url.pathname === "/api/events") {
    res.writeHead(200, { "content-type": "text/event-stream" }); res.write(": ready\n\n");
    clients.add(res); res.on("close", () => clients.delete(res)); return;
  }
  const chunks = []; for await (const chunk of req) chunks.push(chunk);
  const raw = Buffer.concat(chunks);
  let args, files = [];
  if (req.headers["content-type"]?.startsWith("multipart/form-data")) {
    const form = await new Response(raw, { headers: { "content-type": req.headers["content-type"] } }).formData();
    args = JSON.parse(form.get("request")); files = form.getAll("attachments");
  } else args = raw.length ? JSON.parse(raw.toString()) : {};
  requests.push({ path: url.pathname, args });
  let result = { ok: true };
  switch (url.pathname) {
    case "/api/bootstrap": result = { ...state, ui_event_cursor: events.length }; break;
    case "/api/load_channel_previews": case "/api/load_activity_messages": result = []; break;
    case "/api/load_channel_messages": result = { messages: state.messages, next_before_seq: null, has_more: false }; break;
    case "/api/load_thread_messages": result = state.messages.filter(m => m.id === args.threadRootId || m.thread_root_id === args.threadRootId); break;
    case "/api/load_message": result = state.messages.find(m => m.id === args.messageId); break;
    case "/api/load_ui_state": result = Object.fromEntries(args.scopes.map(scope => [scope, state[scope]])); break;
    case "/api/replay_ui_events": result = { cursor: events.length, replayGap: false, events: events.filter(e => e.cursor > args.cursor) }; break;
    case "/api/send_message": {
      const row = message(args.body, { ...(args.messageId ? { id: args.messageId } : {}), thread_root_id: args.threadRootId ?? null });
      row.attachments = files.map((file, i) => ({ id: id(100 + i), message_id: row.id, original_name: file.name,
        mime_type: file.type, size_bytes: file.size, storage_path: "", created_at: now }));
      state.messages.push(row);
      publish({ type: "message_upsert", reason: "message", message: row });
      const fail = await new Promise(release => pendingSends.push({ row, release }));
      res.writeHead(fail ? 500 : 200, { "content-type": "application/json" }).end(JSON.stringify(fail ? { error: "Response lost after commit" } : row));
      return;
    }
  }
  const payload = JSON.stringify(result);
  if (url.pathname === "/api/load_ui_state" && args.scopes.includes("agents") && heldAgentRead) {
    heldAgentRead.started = true;
    await heldAgentRead.promise;
  }
  res.writeHead(200, { "content-type": "application/json" }).end(payload);
});
await new Promise(done => api.listen(0, "127.0.0.1", done));
const count = path => requests.filter(r => r.path === `/api/${path}`).length;
const agentReads = () => requests.filter(r => r.path === "/api/load_ui_state" && r.args.scopes.includes("agents")).length;
let browser;
try {
  const engine = process.env.LANTOR_RACE_ENGINE === "webkit" ? webkit : chromium;
  browser = await engine.launch();
  const mobile = process.env.LANTOR_RACE_MOBILE === "1";
  const context = await browser.newContext({ serviceWorkers: "block", viewport: mobile ? { width: 390, height: 844 } : { width: 1440, height: 1000 } });
  // randomUUID is unavailable on non-localhost HTTP mobile clients; getRandomValues remains available.
  await context.addInitScript(() => Object.defineProperty(crypto, "randomUUID", { value: undefined }));
  // Hold the ephemeral flush like a suspended tab. Terminal lifecycle events
  // must clear progress even while a prior running/usage patch is buffered.
  await context.addInitScript(() => {
    const raf = window.requestAnimationFrame.bind(window), timeout = window.setTimeout.bind(window);
    const cancelRaf = window.cancelAnimationFrame.bind(window), cancelTimeout = window.clearTimeout.bind(window);
    const pending = new Map(); let next = -1;
    const hold = callback => { const id = next--; pending.set(id, callback); return id; };
    window.__progressFlushGate = { held: false, resume() {
      this.held = false;
      const callbacks = [...pending.values()]; pending.clear();
      for (const callback of callbacks) callback();
    } };
    window.requestAnimationFrame = callback => window.__progressFlushGate.held
      ? hold(() => callback(performance.now())) : raf(callback);
    window.setTimeout = (callback, delay, ...args) => window.__progressFlushGate.held && delay === 80
      ? hold(() => callback(...args)) : timeout(callback, delay, ...args);
    window.cancelAnimationFrame = id => { if (!pending.delete(id)) cancelRaf(id); };
    window.clearTimeout = id => { if (!pending.delete(id)) cancelTimeout(id); };
  });
  const page = await context.newPage(); page.setDefaultTimeout(5000);
  const errors = []; page.on("pageerror", error => errors.push(error.message));
  await page.goto(`http://127.0.0.1:${api.address().port}`, { waitUntil: "domcontentloaded" });
  await page.locator(`.conversation [data-message-id="${root.id}"]`).waitFor();
  const until = async predicate => {
    for (let i = 0; i < 100 && !predicate(); i++) await page.waitForTimeout(25);
    assert.ok(predicate(), "fixture condition reached");
  };
  await until(() => clients.size > 0);
  if (process.env.LANTOR_RACE_CASE !== "status") {
    const send = async (surface, text) => {
      const before = pendingSends.length;
      const panel = page.locator(surface);
      await panel.locator("textarea").fill(text);
      await panel.getByRole("button", { name: surface === ".thread" ? "Send reply" : "Send message", exact: true }).click();
      await until(() => pendingSends.length === before + 1);
      const pending = pendingSends.at(-1);
      await page.locator(`${surface} [data-message-id="${pending.row.id}"]`).waitFor();
      await page.waitForTimeout(150);
      return pending;
    };
    const bodyCount = async (surface, text) => page.locator(`${surface} .markdown-body`).filter({ hasText: text }).count();
    const sent = await send(".conversation", "Root response held");
    assert.equal(await bodyCount(".conversation", sent.row.body), 1, "root must have one row before HTTP response");
    assert.match(sent.row.id, /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/, "client supplies a UUID even over HTTP");
    sent.release(false); await page.waitForTimeout(100);
    const rootRow = page.locator(`.conversation [data-message-id="${root.id}"]`);
    if (mobile) await rootRow.locator(".markdown-body").click();
    else { await rootRow.hover(); await rootRow.getByRole("button", { name: "View thread replies", exact: true }).click(); }
    await page.locator(".thread textarea").waitFor();
    const reply = await send(".thread", "Reply response held");
    assert.equal(await bodyCount(".thread", reply.row.body), 1, "reply must have one row before HTTP response");
    // A second real send with identical text remains a distinct message.
    const second = await send(".thread", reply.row.body);
    assert.notEqual(reply.row.id, second.row.id);
    assert.equal(await bodyCount(".thread", reply.row.body), 2, "identical real sends are not content-deduplicated");
    second.release(false); reply.release(false); await page.waitForTimeout(100);
    const lost = await send(".thread", "Committed reply with failed response");
    await page.locator(".thread textarea").fill("New draft stays here");
    lost.release(true); await page.waitForTimeout(200);
    assert.equal(await bodyCount(".thread", lost.row.body), 1, "a committed row survives a failed HTTP response");
    assert.equal(await page.locator(".thread textarea").inputValue(), "New draft stays here");
    assert.equal(count("bootstrap"), 1, "message reconciliation never bootstraps");
    await page.locator('.thread input[type="file"]').setInputFiles({ name: "race.txt", mimeType: "text/plain", buffer: Buffer.from("attachment fixture") });
    const attachment = await send(".thread", "Attachment response held");
    assert.equal(attachment.row.attachments.length, 1);
    assert.equal(await bodyCount(".thread", attachment.row.body), 1);
    assert.equal(await page.locator(`.thread [data-message-id="${attachment.row.id}"]`).getByText("race.txt", { exact: true }).count(), 1);
    // Recover a fresh snapshot while the send response remains in flight.
    const refreshed = page.waitForResponse(response => new URL(response.url()).pathname === "/api/bootstrap");
    publish({ type: "refresh", reason: "event_replay_gap" });
    await refreshed; await page.waitForTimeout(250);
    attachment.release(false); await page.waitForTimeout(150);
    assert.equal(await bodyCount(".thread", attachment.row.body), 1, "snapshot cannot resurrect the optimistic copy");
    console.log("PASS: root/reply event-before-response, UUID fallback, identical sends, committed-response failure, attachment/snapshot");
  }
  const bootstrapBeforeStatus = count("bootstrap");
  if (process.env.LANTOR_RACE_CASE !== "messages") {
    const avatar = page.locator(".conversation .message-agent-avatar-trigger .agent-avatar");
    const expectStatus = async status => {
      await page.waitForFunction(status => document.querySelector(".conversation .message-agent-avatar-trigger .agent-avatar")?.classList.contains(`status-${status}`), status);
    };
    assert.ok((await avatar.getAttribute("class")).includes("status-running"));
    agent.status = "idle"; state.agent_runs = [run("exited")];
    publish({ type: "agent_run_upsert", reason: "codex_turn_finished", run: state.agent_runs[0] });
    await expectStatus("idle");
    // A prior collection read must not put the old busy state back after completion.
    let release;
    heldAgentRead = { started: false, promise: new Promise(done => { release = done; }) };
    agent.status = "running";
    publish({ type: "agent_run_upsert", reason: "run_running", run: run("running", { id: id(4) }) });
    await until(() => heldAgentRead.started);
    agent.status = "idle";
    publish({ type: "agent_run_upsert", reason: "claude_turn_finished", run: run("exited", { id: id(4) }) });
    await page.waitForTimeout(50); heldAgentRead = null; release();
    await expectStatus("idle"); await page.waitForTimeout(250); await expectStatus("idle");
    // An old run finishing is not proof of idleness: the next run may already be queued.
    agent.status = "queued";
    publish({ type: "agent_run_upsert", reason: "run_finished", run: run("exited") });
    await expectStatus("queued");
    agent.status = "running";
    publish({ type: "agent_run_upsert", reason: "run_running", run: run("running", { id: id(5) }) });
    await expectStatus("running");
    const beforeUsage = agentReads();
    for (let i = 0; i < 10; i++) publish({ type: "agent_run_upsert", reason: "run_usage", run: run("running", { id: id(5), input_tokens: i }) });
    await page.waitForTimeout(250);
    assert.equal(agentReads(), beforeUsage, "usage-only events do not fetch profiles");
    agent.status = "idle";
    publish({ type: "agent_run_upsert", reason: "run_failed", run: run("failed", { id: id(5) }) });
    await expectStatus("idle");
    assert.equal(count("bootstrap"), bootstrapBeforeStatus, "lifecycle synchronization never bootstraps");
    console.log("PASS: completion, stale in-flight profile read, queued successor, start/failure, usage-only batching");

    for (const surface of [".conversation", ".thread"]) {
      if (mobile && surface === ".conversation" && await page.locator(".thread textarea").isVisible()) {
        await page.getByRole("button", { name: "Back to channel", exact: true }).click();
      }
      if (surface === ".thread" && !await page.locator(".thread textarea").isVisible()) {
        const rootRow = page.locator(`.conversation [data-message-id="${root.id}"]`);
        if (mobile) await rootRow.locator(".markdown-body").click();
        else { await rootRow.hover(); await rootRow.getByRole("button", { name: "View thread replies", exact: true }).click(); }
        await page.locator(".thread textarea").waitFor();
      }
      for (const terminal of ["exited", "unknown", "failed"]) {
        const runId = id(++seq), threadRootId = surface === ".thread" ? root.id : null;
        const liveRun = run("running", { id: runId });
        const item = { id: id(++seq), agent_id: agentId, agent_handle: agent.handle, channel_id: channelId,
          channel_name: "race-test", thread_root_id: threadRootId, source_message_id: root.id,
          task_id: null, task_number: null, source_kind: "mention", title: "Progress fixture", context: "",
          status: "running", run_id: runId, created_at: now, updated_at: now, completed_at: null };
        const placeholder = message("", { sender_agent_id: agentId, sender_name: agent.display_name, sender_role: "agent",
          delivery_state: "streaming", stream_key: `${runId}:pending`, thread_root_id: threadRootId });
        agent.status = "running";
        state.agent_runs.push(liveRun); state.agent_work_items.push(item); state.messages.push(placeholder);
        publish({ type: "agent_run_upsert", reason: "run_running", run: liveRun });
        publish({ type: "work_item_upsert", work_item: item });
        publish({ type: "message_upsert", message: placeholder });
        await page.locator(`${surface} .activity-progress-summary[data-state="working"]`).waitFor();
        await page.waitForTimeout(150);
        await page.evaluate(() => { window.__progressFlushGate.held = true; });
        publish({ type: "agent_run_upsert", reason: "run_usage", run: { ...liveRun, input_tokens: 10 } });
        agent.status = "idle";
        Object.assign(liveRun, { status: terminal, stopped_at: new Date().toISOString() });
        publish({ type: "agent_run_upsert", reason: "run_finished", run: liveRun });
        // Neither a final activity nor a completed message/work-item event is
        // sent: failures/restarts can leave all three missing in persisted data.
        await page.waitForFunction(selector => !document.querySelector(selector), `${surface} .activity-progress-dock`, { polling: 20 });
        await page.evaluate(() => window.__progressFlushGate.resume());
        await page.waitForTimeout(150);
        assert.equal(await page.locator(`${surface} .activity-progress-dock`).count(), 0, `${terminal} cannot be overwritten by buffered running state`);
        item.status = "done"; item.updated_at = new Date().toISOString();
        publish({ type: "work_item_upsert", work_item: item });
      }
    }
    // An old run may fall outside the bootstrap's 30-row history. Idle profiles
    // still suppress its empty stream after a reload, using the sender's ID.
    state.agent_runs = []; state.agent_work_items = [];
    await page.reload({ waitUntil: "domcontentloaded" });
    await page.locator(".conversation textarea").waitFor({ state: "attached" });
    await page.waitForTimeout(200);
    assert.equal(await page.locator(".activity-progress-dock").count(), 0, "compacted history does not revive orphan streams");
    console.log("PASS: channel/thread terminal progress, suspended flush, no terminal activity/message, compacted history reload");
  }
  assert.deepEqual(errors, []);
} finally {
  for (const pending of pendingSends) pending.release(false);
  if (browser) await browser.close();
  for (const client of clients) client.end();
  api.closeAllConnections();
  await new Promise(done => api.close(done));
}
