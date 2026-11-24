// The menu a working thread's reply summary opens on hover, against a
// synthetic API; never starts a real backend.
// Run npm run build first. LANTOR_UI_SCREENSHOTS saves light/dark evidence.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile, mkdir } from "node:fs/promises";
import { resolve, join } from "node:path";
import { chromium, webkit } from "playwright";

const id = n => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const now = new Date();
const iso = offsetMinutes => new Date(now.getTime() + offsetMinutes * 60_000).toISOString();
const channelId = id(1000);
const baseAgent = { role: "agent", status: "running", runtime: "test", model: "", reasoning_effort: "", service_tier: "",
  description: "", launch_command: "", environment_variables: "", working_directory: "", workspace_exists: false,
  workspace_memory_path: "", workspace_memory_exists: false, workspace_entries: [], details_loaded: true,
  daily_budget_micros: 0, subscription_status: null };
const agents = [
  { ...baseAgent, id: id(800), handle: "Aster", display_name: "Aster", avatar: "A" },
  { ...baseAgent, id: id(801), handle: "Birch", display_name: "Birch", avatar: "B" },
  { ...baseAgent, id: id(802), handle: "Cedar", display_name: "Cedar", avatar: "C" },
];
const message = (n, body, minutesAgo) => ({ id: id(n), seq: n, channel_id: channelId, thread_root_id: null,
  sender_agent_id: null, sender_name: "Owner", sender_role: "owner", body, is_task: false, thread_followed: false,
  delivery_state: "complete", stream_key: "", task_number: null, task_status: null, attachments: [], artifacts: [],
  created_at: iso(-minutesAgo), updated_at: iso(-minutesAgo) });
const root = message(1, "@Aster review this change and tell me what it does", 60);
const next = message(2, "@Aster read the onboarding notes at https://example.com/notes and summarise them", 30);
const channels = [{ id: channelId, name: "reply-menu", kind: "channel", dm_agent_id: null,
  description: "", unread_count: 0, github_unread_count: 0, github_review_synced_at: null }];

/** One running turn per agent in the root's thread, each with its latest step. */
function working(count) {
  const runs = [], workItems = [], activities = [];
  agents.slice(0, count).forEach((agent, index) => {
    const runId = id(5000 + index);
    runs.push({ id: runId, agent_id: agent.id, work_item_id: null, command: "fixture", working_directory: "",
      status: "running", pid: null, exit_code: null, log: "", started_at: iso(-1), stopped_at: null,
      input_tokens: 0, output_tokens: 0, cost_micros: 0 });
    workItems.push({ id: id(5100 + index), agent_id: agent.id, agent_handle: agent.handle, channel_id: channelId,
      channel_name: "reply-menu", thread_root_id: root.id, source_message_id: root.id, task_id: null, task_number: null,
      source_kind: "mention", title: "Review", context: "", status: "running", run_id: runId,
      created_at: iso(-1), updated_at: iso(-1), completed_at: null });
    activities.push({ id: id(5200 + index), agent_id: agent.id, agent_handle: agent.handle, run_id: runId,
      kind: "command", phase: "command", status: "active", title: "Running command", summary: "Running command",
      detail: "", metadata: {}, created_at: iso(0) });
  });
  return { agent_runs: runs, agent_work_items: workItems, agent_activities: activities };
}

let state;
const clients = new Set();
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
  const args = raw ? JSON.parse(raw) : {};
  let result = { ok: true };
  switch (url.pathname) {
    case "/api/bootstrap": result = { ...state, ui_event_cursor: 0 }; break;
    case "/api/load_channel_previews": case "/api/load_activity_messages": case "/api/search_messages": result = []; break;
    case "/api/load_channel_messages": result = { messages: state.messages, next_before_seq: null, has_more: false }; break;
    case "/api/load_thread_page": result = { messages: [], next_before_seq: null, has_more: false }; break;
    case "/api/load_ui_state": result = Object.fromEntries(args.scopes.map(scope => [scope, state[scope]])); break;
    case "/api/replay_ui_events": result = { cursor: 0, replayGap: false, events: [] }; break;
  }
  res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(result));
});
await new Promise(done => api.listen(0, "127.0.0.1", done));

const freshState = count => ({ db_url: "synthetic://reply-progress-menu", web_base_url: null,
  owner_profile: { display_name: "Owner", avatar: "D", description: "", watched_apps: [] },
  channels, messages: [root, next],
  channel_message_history: channels.map(c => ({ channel_id: c.id, before_seq: null, has_more: false })),
  agents, tasks: [], channel_members: agents.map(agent => ({ channel_id: channelId, agent_id: agent.id })),
  thread_activities: [], saved_messages: [], dismissed_inbox_items: {}, read_inbox_items: {}, artifacts: [], reminders: [],
  ...working(count), supervisor: { pid: null, status: "stopped", updated_at: null },
  launch_agent: { label: "", plist_path: "", installed: false, loaded: false }, ui_event_cursor: 0 });

let browser;
try {
  for (const [engineName, engine] of Object.entries({ chromium, webkit })) {
    // Three rows make the menu taller than the clip margin of a row.
    for (const count of [1, 3]) {
      state = freshState(count);
      browser = await engine.launch();
      const page = await (await browser.newContext({ serviceWorkers: "block", viewport: { width: 1440, height: 900 } })).newPage();
      page.setDefaultTimeout(10000);
      const errors = []; page.on("pageerror", error => errors.push(error.message));
      await page.goto(`http://127.0.0.1:${api.address().port}`, { waitUntil: "domcontentloaded" });
      const summary = page.locator(`.conversation [data-message-id="${root.id}"] .thread-reply-summary.active-reply`);
      await summary.waitFor();
      assert.equal(await summary.getAttribute("title"), null, "no native tooltip covers the progress menu");
      await summary.hover();
      const menu = summary.locator(".thread-reply-active-menu");
      await menu.waitFor({ state: "visible" });
      await menu.getByText("Running command").first().waitFor();
      assert.equal(await menu.locator(".thread-reply-active-agent").count(), count);
      assert.equal(await summary.getAttribute("data-active-menu-placement"), "below", "the menu opens over the next message");

      // Every agent's row in the menu is what the pointer would hit: the next
      // message neither paints over it nor clips its lower part. The menu
      // ignores the pointer, so hit testing needs it to take part first.
      await menu.evaluate(element => { element.style.pointerEvents = "auto"; });
      const rows = await menu.locator(".thread-reply-active-agent").all();
      for (const row of rows) {
        const box = await row.boundingBox();
        for (const [x, y] of [[box.x + 24, box.y + box.height / 2], [box.x + box.width / 2, box.y + box.height - 3]]) {
          const covered = await page.evaluate(({ x, y }) => {
            const hit = document.elementFromPoint(x, y);
            return hit?.closest(".thread-reply-active-menu") ? null : `${hit?.tagName}.${hit?.className}`;
          }, { x, y });
          assert.equal(covered, null, `${engineName}, ${count} agent(s): the menu is on top at ${Math.round(x)},${Math.round(y)}`);
        }
      }
      if (process.env.LANTOR_UI_SCREENSHOTS) {
        await mkdir(process.env.LANTOR_UI_SCREENSHOTS, { recursive: true });
        const box = await menu.boundingBox();
        for (const theme of ["light", "dark"]) {
          await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
          await page.screenshot({ animations: "disabled", clip: { x: box.x - 220, y: box.y - 120, width: 900, height: box.height + 200 },
            path: join(process.env.LANTOR_UI_SCREENSHOTS, `reply-progress-menu-${count}-${theme}-${engineName}.png`) });
        }
      }
      assert.deepEqual(errors, []);
      await browser.close(); browser = null;
      console.log(`✓ ${engineName} ${count} working agent(s)`);
    }
  }
} finally {
  await browser?.close(); api.close(); for (const client of clients) client.end();
}
