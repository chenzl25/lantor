// Production App against an isolated API: loops in the agent drawer's Reminders tab.
// Run npm run build first. No live workspace data is read or mutated.
// Set LANTOR_UI_SCREENSHOTS=<dir> to save light/dark screenshots of the tab.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile, mkdir } from "node:fs/promises";
import { resolve, join } from "node:path";
import { chromium, webkit } from "playwright";

const id = n => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const now = new Date();
const iso = offsetMinutes => new Date(now.getTime() + offsetMinutes * 60_000).toISOString();
const channelId = id(1000), dmId = id(1001);
const agent = { id: id(800), handle: "Speed", display_name: "Speed", role: "agent", status: "idle", runtime: "claude",
  model: "", reasoning_effort: "", service_tier: "", avatar: "S", description: "", launch_command: "", environment_variables: "",
  working_directory: "", workspace_exists: false, workspace_memory_path: "", workspace_memory_exists: false,
  workspace_entries: [], details_loaded: true, daily_budget_micros: 0, subscription_status: null };
const root = { id: id(1), seq: 1, channel_id: channelId, thread_root_id: null, sender_agent_id: null, sender_name: "Dylan",
  sender_role: "owner", body: "Forward #github-pulls here", is_task: false, thread_followed: false, delivery_state: "complete",
  stream_key: "", task_number: null, task_status: null, attachments: [], artifacts: [], created_at: iso(-60), updated_at: iso(-60) };
const channels = [{ id: channelId, name: "oncall", kind: "channel", dm_agent_id: null },
  { id: dmId, name: `dm:${agent.id}`, kind: "dm", dm_agent_id: agent.id }].map(c => ({ ...c, description: "", unread_count: 0, github_unread_count: 0, github_review_synced_at: null }));
const reminder = (n, extra) => ({ id: id(n), channel_id: channelId, channel_name: "oncall", creator_agent_id: agent.id,
  creator_agent_handle: "Speed", thread_root_id: root.id, message_id: null, note: "", status: "scheduled", recurrence: "none",
  interval_seconds: null, check_command: "", check_action: "", last_checked_at: null, last_check_status: "",
  last_check_detail: "", check_failures: 0, due_at: iso(5), fired_at: null, completed_at: null, created_at: iso(-60), updated_at: iso(-1), ...extra });
const initialReminders = [
  reminder(700, { title: "Forward #github-pulls", recurrence: "interval", interval_seconds: 300,
    check_command: "\"$HOME/tools/slack-forward/run.sh\" \"$HOME/tools/slack-forward/github-pulls.json\"",
    check_action: "post", last_checked_at: iso(-2), last_check_status: "quiet", due_at: iso(3) }),
  reminder(701, { title: "Watch iceberg CI", recurrence: "interval", interval_seconds: 900, check_command: "./ci-watch.sh",
    check_action: "wake", last_checked_at: iso(-4), last_check_status: "failed", last_check_detail: "exit status 1: gh: not logged in",
    check_failures: 3, due_at: iso(26) }),
  reminder(702, { title: "Daily worktree cleanup", recurrence: "daily", due_at: iso(600) }),
  // Long titles and unbroken paths must stay inside the row.
  reminder(703, { title: "PR #202 deploy: verify supervisor switch, then create #github-pulls loop and report the first check",
    note: "Read artifacts/deploy-20261022-loops/README.md and supervisor-switch-status.json. If stage=complete: emit "
      + "reminder_create loop in #oncall:832403dc (interval_minutes 5, check_command "
      + "`tools/slack-forward/run.sh tools/slack-forward/github-pulls.json`, check_action post).", due_at: iso(8) }),
];
const state = { db_url: "synthetic://agent-loops", web_base_url: null, owner_profile: { display_name: "Dylan", avatar: "D", description: "" },
  channels, messages: [root], channel_message_history: channels.map(c => ({ channel_id: c.id, before_seq: null, has_more: false })),
  agents: [agent], tasks: [], channel_members: [{ channel_id: channelId, agent_id: agent.id }], thread_activities: [],
  saved_messages: [], dismissed_inbox_items: {}, read_inbox_items: {}, artifacts: [], reminders: [], agent_runs: [],
  agent_work_items: [], agent_activities: [], supervisor: { pid: null, status: "stopped", updated_at: null },
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
    case "/api/cancel_reminder":
      state.reminders = state.reminders.filter(r => r.id !== args.reminderId);
      publish({ type: "refresh", reason: "reminder_cancelled" });
      break;
  }
  res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(result));
});
await new Promise(done => api.listen(0, "127.0.0.1", done));
let browser;
try {
  for (const [name, engine] of Object.entries({ chromium, webkit })) {
    state.reminders = structuredClone(initialReminders);
    requests.length = 0;
    browser = await engine.launch();
    const context = await browser.newContext({ serviceWorkers: "block", viewport: { width: 1440, height: 960 } });
    const page = await context.newPage(); page.setDefaultTimeout(8000);
    const errors = []; page.on("pageerror", error => errors.push(error.message));
    await page.goto(`http://127.0.0.1:${api.address().port}`, { waitUntil: "domcontentloaded" });
    await page.getByRole("button", { name: "View @Speed details", exact: true }).click();
    await page.getByRole("tab", { name: /Reminders/ }).click();
    const rows = page.locator(".agent-reminder-row");
    await rows.first().waitFor();
    assert.equal(await rows.count(), 4);
    const overflow = await rows.evaluateAll(list => list.flatMap(row => {
      const edge = row.getBoundingClientRect().right;
      return [...row.querySelectorAll("*")]
        .filter(node => node.getBoundingClientRect().right > edge + 0.5)
        .map(node => `${row.querySelector("strong")?.textContent?.slice(0, 24)}: ${node.tagName.toLowerCase()}.${node.className}`);
    }));
    assert.deepEqual(overflow, [], "reminder row content stays inside the row");

    const forward = rows.filter({ hasText: "Forward #github-pulls" });
    assert.match(await forward.locator(".agent-reminder-title span").textContent(), /^Loop$/);
    assert.equal(await forward.locator(".agent-reminder-loop-meta").textContent(), "every 5 min · posts output");
    assert.match(await forward.locator(".agent-reminder-loop-check").textContent(), /^Last check .+ · no output$/);
    assert.equal(await forward.locator(".agent-reminder-command").getAttribute("title"), initialReminders[0].check_command);
    assert.match(await forward.locator(".agent-reminder-foot small").textContent(), /^Next .+ · #oncall$/);

    const failing = rows.filter({ hasText: "Watch iceberg CI" });
    assert.equal(await failing.evaluate(e => e.classList.contains("check-failed")), true);
    assert.equal(await failing.locator(".agent-reminder-loop-meta").textContent(), "every 15 min · wakes @Speed on output");
    assert.match(await failing.locator(".agent-reminder-loop-check").textContent(), /failed \(3 in a row\): exit status 1: gh: not logged in$/);

    const daily = rows.filter({ hasText: "Daily worktree cleanup" });
    assert.equal(await daily.locator(".agent-reminder-loop-meta").count(), 0, "plain reminders keep their compact row");
    assert.equal(await daily.getByRole("button", { name: "Cancel", exact: true }).count(), 1);

    if (process.env.LANTOR_UI_SCREENSHOTS) {
      await mkdir(process.env.LANTOR_UI_SCREENSHOTS, { recursive: true });
      const drawer = page.locator(".agent-reminders-section");
      await drawer.screenshot({ path: join(process.env.LANTOR_UI_SCREENSHOTS, `agent-loops-light-${name}.png`) });
      await page.evaluate(() => { document.documentElement.dataset.theme = "dark"; });
      await drawer.screenshot({ path: join(process.env.LANTOR_UI_SCREENSHOTS, `agent-loops-dark-${name}.png`) });
      await page.evaluate(() => { document.documentElement.dataset.theme = "light"; });
    }

    // Stop asks first; confirming cancels the loop and the row disappears on the scoped refresh.
    await forward.getByRole("button", { name: "Stop", exact: true }).click();
    const dialog = page.getByRole("dialog");
    await dialog.getByText('Stop loop "Forward #github-pulls"?').waitFor();
    await dialog.getByRole("button", { name: "Stop loop", exact: true }).click();
    await forward.waitFor({ state: "detached" });
    assert.deepEqual(requests.filter(r => r.path === "/api/cancel_reminder").map(r => r.args), [{ reminderId: initialReminders[0].id }]);
    assert.equal(await rows.count(), 3);
    assert.deepEqual(errors, [], `${name} page errors`);
    await browser.close(); browser = null;
    console.log(`agent loops e2e passed in ${name}`);
  }
} finally {
  await browser?.close();
  api.close();
  for (const client of clients) client.end();
}
