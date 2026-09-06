// Actual App + EventSource + production React Profiler, with a synthetic API.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { build } from "vite";
import { chromium } from "playwright";
const directory = await mkdtemp(join(tmpdir(), "lantor-stream-test-"));
const id = (n) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const channelId = id(1000), now = "2026-09-06T12:00:00Z";
const message = (n, extra = {}) => ({ id: id(n), seq: n, channel_id: channelId, thread_root_id: null,
  sender_agent_id: null, sender_name: n % 2 ? "Owner" : "Test agent", sender_role: n % 2 ? "owner" : "agent", body: `Message ${n}.\n\nA paragraph of stable conversation context.`,
  is_task: false, thread_followed: false, delivery_state: "complete", stream_key: "", task_number: null, task_status: null,
  attachments: [], artifacts: [], created_at: now, updated_at: now, ...extra });
const state = {
  db_url: "synthetic://stream", web_base_url: null,
  owner_profile: { display_name: "Owner", avatar: "O", description: "" },
  channels: [{ id: channelId, name: "stream-test", description: "", kind: "channel", dm_agent_id: null, unread_count: 0, github_unread_count: 0, github_review_synced_at: null }],
  messages: [...Array.from({ length: 50 }, (_, i) => message(i + 1)), ...Array.from({ length: 20 }, (_, i) => message(i + 101, { thread_root_id: id(1) }))],
  channel_message_history: [{ channel_id: channelId, before_seq: null, has_more: false }],
  thread_activities: [], channel_members: [], agents: [], saved_messages: [], dismissed_inbox_items: {}, read_inbox_items: {}, artifacts: [], tasks: [], reminders: [], agent_schedules: [], agent_runs: [], agent_work_items: [], agent_activities: [],
  supervisor: { pid: null, status: "stopped", updated_at: null }, launch_agent: { label: "", plist_path: "", installed: false, loaded: false }, ui_event_cursor: 0,
};
const clients = new Set(), events = [], requests = [];
const sse = (entry) => `id: ${entry.cursor}\nevent: lantor\ndata: ${entry.event}\n\n`;
function publish(event) {
  const entry = { cursor: events.length + 1, event: JSON.stringify(event) };
  events.push(entry);
  for (const client of clients) client.write(sse(entry));
  return entry;
}
let staleBootstrap = false;
const api = createServer(async (request, response) => {
  const url = new URL(request.url, "http://localhost");
  if (!url.pathname.startsWith("/api/")) {
    const file = resolve(directory, url.pathname === "/" ? "index.html" : url.pathname.slice(1));
    if (!file.startsWith(`${directory}/`)) { response.writeHead(404).end(); return; }
    try {
      const mime = { js: "application/javascript", css: "text/css", html: "text/html", png: "image/png", woff: "font/woff", woff2: "font/woff2" };
      response.writeHead(200, { "content-type": mime[file.split(".").pop()] ?? "application/octet-stream" }).end(await readFile(file));
    } catch { response.writeHead(404).end(); }
    return;
  }
  requests.push(url.pathname);
  if (url.pathname === "/api/events") {
    response.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
    response.write(": ready\n\n");
    for (const entry of events) if (entry.cursor > Number(url.searchParams.get("cursor"))) response.write(sse(entry));
    clients.add(response); response.on("close", () => clients.delete(response)); return;
  }
  let raw = "";
  for await (const chunk of request) raw += chunk;
  const args = raw ? JSON.parse(raw) : {};
  let result = { ok: true };
  switch (url.pathname) {
    case "/api/bootstrap": result = { ...state, ui_event_cursor: events.length, messages: staleBootstrap ? state.messages.map((m) => m.delivery_state === "streaming" ? { ...m, body: "" } : m) : state.messages }; break;
    case "/api/load_channel_previews": case "/api/load_activity_messages": result = []; break;
    case "/api/load_channel_messages": result = { messages: state.messages, next_before_seq: null, has_more: false }; break;
    case "/api/load_message": result = state.messages.find((m) => m.id === args.messageId); break;
    case "/api/load_ui_state": result = Object.fromEntries(args.scopes.map((scope) => [scope, state[scope]])); break;
    case "/api/replay_ui_events": result = { cursor: events.length, replayGap: false, events: events.filter((e) => e.cursor > args.cursor) }; break;
  }
  response.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(result));
});
let browser;
try {
  await build({ mode: "bench", logLevel: "silent", build: { outDir: directory }, plugins: [{
    name: "stream-profiler", enforce: "pre",
    transform(source, path) {
      if (path.endsWith("/MessageRow.tsx")) {
        assert.ok(source.includes("  return <>\n"));
        return 'import { Profiler } from "react";\n' + source
          .replace("  return <>\n", '  return <Profiler id={`${variant}:${message.id}`} onRender={(id) => window.__streamProbe.rows.push(id)}>\n')
          .replace("  </>;\n});", "  </Profiler>;\n});");
      }
      if (path.endsWith("/streaming-message-store.ts")) return source + "\nwindow.__streamProbe.store = streamingMessages;";
      const probes = path.endsWith("/main.tsx") ? [["function App() {", "app"]]
        : path.endsWith("/Conversation.tsx") ? [["}: ConversationProps) {", "conversation"], ["}: ConversationComposerProps) {", "composer"]]
        : path.endsWith("/ThreadPanel.tsx") ? [["}: ThreadPanelProps) {", "thread"]]
        : path.endsWith("/Sidebar.tsx") ? [["}: SidebarProps) {", "sidebar"]] : [];
      for (const [anchor, name] of probes) {
        assert.ok(source.includes(anchor), `missing ${anchor}`);
        source = source.replace(anchor, anchor + `\nwindow.__streamProbe.renders.${name} += 1;`);
      }
      return probes.length ? source : null;
    },
  }] });
  await new Promise((done) => api.listen(0, "127.0.0.1", done));
  browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({ viewport: { width: 1700, height: 1000 } });
  page.setDefaultTimeout(15000);
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.addInitScript(() => {
    window.__streamProbe = { rows: [], writes: [], renders: { app: 0, conversation: 0, thread: 0, sidebar: 0, composer: 0 } };
    const descriptor = Object.getOwnPropertyDescriptor(Element.prototype, "scrollTop");
    Object.defineProperty(Element.prototype, "scrollTop", { ...descriptor, set(value) {
      if (this.matches(".message-list,.thread-scroll")) window.__streamProbe.writes.push({ surface: this.className, value });
      descriptor.set.call(this, value);
    } });
  });
  await page.goto(`http://127.0.0.1:${api.address().port}`, { waitUntil: "domcontentloaded" });
  await page.locator(`.conversation [data-message-id="${id(1)}"] .thread-reply-summary`).click();
  await page.locator(`.thread [data-message-id="${id(120)}"]`).waitFor();
  await page.waitForTimeout(1000);
  const reset = async () => { await page.waitForTimeout(120); await page.evaluate(() => { window.__streamProbe.rows = []; window.__streamProbe.writes = []; for (const key of Object.keys(window.__streamProbe.renders)) window.__streamProbe.renders[key] = 0; }); };
  const stats = () => page.evaluate(() => ({ rows: [...new Set(window.__streamProbe.rows)], renders: window.__streamProbe.renders, writes: window.__streamProbe.writes.length }));
  const container = (thread) => page.locator(thread ? ".thread-scroll" : ".message-list");
  const bottomDistance = (thread) => container(thread).evaluate((node) => node.scrollHeight - node.clientHeight - node.scrollTop);
  const bodyText = (row, thread) => page.locator(`${thread ? ".thread" : ".conversation"} [data-message-id="${row.id}"] .markdown-body`).innerText();
  async function chunk(row, append, updateServer = true) {
    const nextBody = row.body + append;
    if (updateServer) row.body = nextBody;
    const entry = publish({ type: "message_delta", message_id: row.id, append, body_length: Array.from(nextBody).length, delivery_state: "streaming" });
    await page.waitForTimeout(85);
    return entry;
  }
  const stream = message(200, { body: "Start", sender_role: "agent", stream_key: `${id(8000)}:response`, delivery_state: "streaming" });
  state.messages.push(stream); publish({ type: "message_upsert", message: stream });
  await page.locator(`.conversation [data-message-id="${stream.id}"]`).waitFor();
  await container(false).evaluate((node) => { node.scrollTop = node.scrollHeight; });
  await reset();
  for (let n = 0; n < 8; n++) await chunk(stream, "x");
  const sameLine = await stats();
  assert.deepEqual(sameLine.rows, [`channel:${stream.id}`]);
  assert.deepEqual(sameLine.renders, { app: 0, conversation: 0, thread: 0, sidebar: 0, composer: 0 });
  assert.equal(sameLine.writes, 0, "same-height tokens require no scroll writes");

  const cdp = await page.context().newCDPSession(page);
  const trace = [];
  cdp.on("Tracing.dataCollected", ({ value }) => trace.push(...value));
  const traceOptions = { categories: "devtools.timeline,disabled-by-default-devtools.timeline,disabled-by-default-devtools.timeline.stack", transferMode: "ReportEvents" };
  await cdp.send("Tracing.start", traceOptions);
  await page.evaluate(() => {
    const probe = document.createElement("div");
    document.body.append(probe);
    probe.style.width = "17px";
    void probe.offsetWidth;
    probe.remove();
  });
  const controlEnded = new Promise((done) => cdp.once("Tracing.tracingComplete", done));
  await cdp.send("Tracing.end"); await controlEnded;
  assert.ok(trace.some((event) => event.name === "Layout" && event.args?.beginData?.stackTrace?.length), "positive control confirms forced-layout stack capture");
  trace.length = 0;
  await cdp.send("Tracing.start", traceOptions);
  await reset();
  for (let n = 0; n < 20; n++) await chunk(stream, `\n\nLine ${n} 🦊 growing streamed response.`);
  const channelStats = await stats();
  assert.deepEqual(channelStats.rows, [`channel:${stream.id}`]);
  assert.deepEqual(channelStats.renders, sameLine.renders);
  assert.ok(await bottomDistance(false) <= 2, "channel follows content growth");
  await container(false).hover(); await page.mouse.wheel(0, -300); await page.waitForTimeout(200);
  const pausedTop = await container(false).evaluate((node) => node.scrollTop);
  assert.ok(await bottomDistance(false) > 100);
  await reset();
  for (let n = 0; n < 5; n++) await chunk(stream, `\n\nPaused line ${n}.`);
  assert.ok(Math.abs(await container(false).evaluate((node) => node.scrollTop) - pausedTop) <= 1, "manual up-scroll stays put");
  assert.equal((await stats()).writes, 0);
  await container(false).evaluate((node) => { node.scrollTop = node.scrollHeight; });
  await chunk(stream, "\n\nFollowing again.");
  assert.ok(await bottomDistance(false) <= 2);
  const ended = new Promise((done) => cdp.once("Tracing.tracingComplete", done));
  await cdp.send("Tracing.end"); await ended;
  assert.ok(trace.some((event) => event.name === "Layout"), "stream trace contains real layout work");
  const forced = trace.filter((event) => event.name === "Layout" && event.args?.beginData?.stackTrace?.length);
  assert.deepEqual(forced.map((event) => event.args.beginData.stackTrace), [], "no script-forced layout during streamed updates/follow");

  // Recovery snapshots and subsequent deltas cannot roll back the live prefix.
  staleBootstrap = true;
  publish({ type: "refresh", reason: "event_replay_gap" });
  await page.waitForTimeout(700); staleBootstrap = false;
  assert.ok((await bodyText(stream, false)).includes("Following again."));
  const duplicate = await chunk(stream, "\n\nAfter recovery 🧪.");
  for (const client of clients) client.write(sse(duplicate));
  await page.waitForTimeout(150);
  assert.equal((await bodyText(stream, false)).match(/After recovery/g)?.length, 1);
  const terminal = { ...stream, body: "Authoritative final response.", delivery_state: "complete" };
  Object.assign(stream, terminal); publish({ type: "message_upsert", message: terminal });
  await page.getByText(terminal.body, { exact: true }).waitFor();
  assert.equal(await page.evaluate((key) => window.__streamProbe.store.get(key), stream.id), undefined);

  const reply = message(201, { thread_root_id: id(1), body: "Thread start", sender_role: "agent", stream_key: `${id(8001)}:response`, delivery_state: "streaming" });
  state.messages.push(reply); publish({ type: "message_upsert", message: reply });
  await page.locator(`.thread [data-message-id="${reply.id}"]`).waitFor();
  await container(true).evaluate((node) => { node.scrollTop = node.scrollHeight; });
  trace.length = 0;
  await cdp.send("Tracing.start", traceOptions);
  await reset();
  for (let n = 0; n < 20; n++) await chunk(reply, `\n\nThread line ${n} 🦊.`);
  const threadStats = await stats();
  assert.deepEqual(threadStats.rows, [`reply:${reply.id}`]);
  assert.deepEqual(threadStats.renders, sameLine.renders);
  assert.ok(await bottomDistance(true) <= 2, "thread follows streamed growth");
  await container(true).hover(); await page.mouse.wheel(0, -300); await page.waitForTimeout(200);
  const threadPausedTop = await container(true).evaluate((node) => node.scrollTop);
  await reset();
  for (let n = 0; n < 5; n++) await chunk(reply, `\n\nDetached thread line ${n}.`);
  assert.ok(Math.abs(await container(true).evaluate((node) => node.scrollTop) - threadPausedTop) <= 1);
  assert.deepEqual((await stats()).renders, sameLine.renders);
  await container(true).evaluate((node) => { node.scrollTop = node.scrollHeight; });
  await chunk(reply, "\n\nThread follows again.");
  assert.ok(await bottomDistance(true) <= 2);
  const threadEnded = new Promise((done) => cdp.once("Tracing.tracingComplete", done));
  await cdp.send("Tracing.end"); await threadEnded;
  assert.ok(trace.some((event) => event.name === "Layout"), "thread trace contains real layout work");
  const threadForced = trace.filter((event) => event.name === "Layout" && event.args?.beginData?.stackTrace?.length);
  assert.deepEqual(threadForced.map((event) => event.args.beginData.stackTrace), [], "thread streaming has no script-forced layout");
  await chunk(reply, `\n\n[[message:${stream.id}]]`);
  const liveReference = page.locator(`.thread [data-message-id="${reply.id}"] .message-reference-card`);
  await liveReference.waitFor();
  await liveReference.hover();
  await page.locator(".message-reference-hovercard-body").filter({ hasText: "Authoritative final response." }).waitFor();
  await page.mouse.move(1690, 5);
  // An empty terminal delta still finalizes when its byte range is already covered.
  reply.delivery_state = "complete";
  publish({ type: "message_delta", message_id: reply.id, append: "", body_length: Array.from(reply.body).length, delivery_state: "complete" });
  await page.waitForTimeout(200);
  assert.equal(await page.evaluate((key) => window.__streamProbe.store.get(key), reply.id), undefined);

  // Unknown-id delta hydration retains code-point positioning across emoji.
  const unseen = message(202, { body: "Baseline🦊", sender_role: "agent", delivery_state: "streaming", stream_key: `${id(8002)}:response` });
  state.messages.push(unseen);
  await chunk(unseen, "🧪", false);
  await page.getByText("Baseline🦊🧪", { exact: true }).waitFor();
  assert.ok(requests.includes("/api/load_message"));
  state.messages = state.messages.filter((m) => m.id !== unseen.id);
  publish({ type: "message_delete", message_id: unseen.id });
  await page.waitForTimeout(250);
  assert.equal(await page.evaluate((key) => window.__streamProbe.store.get(key), unseen.id), undefined);
  assert.deepEqual(errors, []);
  console.log(JSON.stringify({ sameLine, channel: channelStats, thread: threadStats, forcedLayouts: forced.length + threadForced.length, checks: "bottom-follow, pause/resume, stale snapshot, cursor replay, final upsert, terminal delta, emoji hydration, delete" }));
} finally {
  await browser?.close();
  for (const client of clients) client.end();
  await new Promise((done) => api.close(done));
  await rm(directory, { recursive: true, force: true });
}
