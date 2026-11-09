// CLI version row and one-tap update against a synthetic API; never starts a
// real backend, runs a CLI or reaches the npm registry.
// Run npm run build first. LANTOR_UI_SCREENSHOTS saves desktop/phone and light/dark evidence.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile, mkdir } from "node:fs/promises";
import { resolve, join } from "node:path";
import { chromium, webkit } from "playwright";

const id = n => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const now = new Date();
const iso = offsetMinutes => new Date(now.getTime() + offsetMinutes * 60_000).toISOString();
const channelId = id(1000), dmId = id(1001);
const agent = { id: id(800), handle: "Aster", display_name: "Aster", role: "agent", status: "idle", runtime: "codex",
  model: "", reasoning_effort: "", service_tier: "", avatar: "S", description: "", launch_command: "", environment_variables: "",
  working_directory: "", workspace_exists: false, workspace_memory_path: "", workspace_memory_exists: false,
  workspace_entries: [], details_loaded: true, daily_budget_micros: 0, subscription_status: null };
const message = (n, channel, body) => ({ id: id(n), seq: n, channel_id: channel, thread_root_id: null, sender_agent_id: null,
  sender_name: "Owner", sender_role: "owner", body, is_task: false, thread_followed: false, delivery_state: "complete",
  stream_key: "", task_number: null, task_status: null, attachments: [], artifacts: [], created_at: iso(-60), updated_at: iso(-60) });
const channels = [{ id: channelId, name: "cli-fixture", kind: "channel", dm_agent_id: null },
  { id: dmId, name: `dm:${agent.id}`, kind: "dm", dm_agent_id: agent.id }].map(c => ({ ...c, description: "", unread_count: 0, github_unread_count: 0, github_review_synced_at: null }));
const toolsSection = { state: "ready", message: "", observed_at: null, items: [] };

const base = { runtime: "codex", command: "codex", installed: "0.167.0", latest: "0.168.0", update_available: true,
  updating: false, problem: null, last_update: null, checked_at: now.toISOString() };
const failedOutput = Array.from({ length: 20 }, (_, n) => `npm ERR! fixture line ${n + 1}`).join("\n");
const outcomes = {
  failure: { ok: false, command: "codex update", from: "0.167.0", to: "0.167.0", output: failedOutput, finished_at: iso(0) },
  success: { ok: true, command: "codex update", from: "0.167.0", to: "0.168.0", output: "Updated", finished_at: iso(0) },
};
// What load_cli_version returns: before an update, while it runs (one poll), then after.
let scenario = "failure", phase = "idle";
function cliStatus() {
  if (phase === "idle") return base;
  if (phase === "updating") { phase = "done"; return { ...base, updating: true }; }
  return scenario === "success"
    ? { ...base, installed: "0.168.0", update_available: false, last_update: outcomes.success }
    : { ...base, last_update: outcomes.failure };
}

let state, cursor = 0;
const clients = new Set(), requests = [];
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
    case "/api/load_thread_page": result = { messages: [], next_before_seq: null, has_more: false }; break;
    case "/api/load_ui_state": result = Object.fromEntries(args.scopes.map(scope => [scope, state[scope]])); break;
    case "/api/replay_ui_events": result = { cursor, replayGap: false, events: [] }; break;
    case "/api/load_agent_tools": result = { agent_id: agent.id, runtime: agent.runtime, checked_at: now.toISOString(),
      apps: toolsSection, mcp: toolsSection, plugins: toolsSection, skills: toolsSection }; break;
    case "/api/load_cli_version": result = cliStatus(); break;
    case "/api/update_cli": phase = "updating"; result = { ...base, updating: true }; break;
    case "/api/open_dm_with_agent": result = dmId; break;
    case "/api/send_message": {
      const sent = { ...message(state.messages.length + 10, args.channelId, args.body), id: args.messageId ?? id(state.messages.length + 10) };
      state.messages = [...state.messages, sent];
      result = sent;
      break;
    }
  }
  res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(result));
});
await new Promise(done => api.listen(0, "127.0.0.1", done));

const freshState = () => ({ db_url: "synthetic://cli-updates", web_base_url: null,
  owner_profile: { display_name: "Owner", avatar: "D", description: "", watched_apps: [] },
  channels, messages: [message(1, channelId, "Fixture channel")],
  channel_message_history: channels.map(c => ({ channel_id: c.id, before_seq: null, has_more: false })),
  agents: [agent], tasks: [], channel_members: [{ channel_id: channelId, agent_id: agent.id }], thread_activities: [],
  saved_messages: [], dismissed_inbox_items: {}, read_inbox_items: {}, artifacts: [], reminders: [], agent_runs: [],
  agent_work_items: [], agent_activities: [], supervisor: { pid: null, status: "stopped", updated_at: null },
  launch_agent: { label: "", plist_path: "", installed: false, loaded: false }, ui_event_cursor: 0 });

let browser;
try {
  for (const [engineName, engine] of Object.entries({ chromium, webkit })) {
    for (const run of ["failure", "success"]) {
      scenario = run; phase = "idle"; state = freshState(); requests.length = 0;
      browser = await engine.launch();
      const context = await browser.newContext({ serviceWorkers: "block", viewport: { width: 1440, height: 960 } });
      const page = await context.newPage(); page.setDefaultTimeout(10000);
      const errors = []; page.on("pageerror", error => errors.push(error.message));
      await page.goto(`http://127.0.0.1:${api.address().port}`, { waitUntil: "domcontentloaded" });
      await page.getByRole("button", { name: "@Aster’s profile", exact: true }).click();
      await page.getByRole("tab", { name: /^Profile/ }).click();
      const row = page.getByRole("group", { name: "Codex version" });
      await row.getByText("0.167.0", { exact: true }).waitFor();
      assert.deepEqual(requests.filter(r => r.path === "/api/load_cli_version").map(r => r.args), [{ runtime: "codex" }]);
      await row.getByText(/Each agent switches when it next starts/).waitFor();

      if (run === "failure") {
        for (const width of [1440, 390]) {
          await page.setViewportSize({ width, height: width === 390 ? 844 : 960 });
          await row.scrollIntoViewIfNeeded();
          assert.ok(await row.evaluate(e => e.scrollWidth <= e.clientWidth + 1), `${width}px version row has no horizontal overflow`);
        }
        await page.setViewportSize({ width: 1440, height: 960 });
      }

      await row.getByRole("button", { name: "Update to 0.168.0" }).click();
      await row.getByRole("status").filter({ hasText: "Updating…" }).waitFor();
      assert.equal(await row.getByRole("button", { name: "Update to 0.168.0" }).count(), 0, "no second update while one runs");
      assert.deepEqual(requests.filter(r => r.path === "/api/update_cli").map(r => r.args), [{ runtime: "codex" }]);

      if (run === "success") {
        await row.getByText(/Updated from 0\.167\.0 to 0\.168\.0 just now/).waitFor({ timeout: 15000 });
        await row.getByText("Latest", { exact: true }).waitFor();
        assert.equal(await row.getByText("0.168.0", { exact: true }).count(), 1);
        assert.equal(await row.getByRole("button").count(), 0);
      } else {
        const failure = row.getByRole("alert");
        await failure.getByText(/failed just now/).waitFor({ timeout: 15000 });
        const shown = await failure.locator(".cli-version-output").textContent();
        assert.ok(shown.startsWith("…\nnpm ERR! fixture line 9"), "the output shows its last lines");
        assert.ok(shown.endsWith("npm ERR! fixture line 20"));
        assert.equal(await row.getByRole("button", { name: "Update to 0.168.0" }).count(), 0, "Try again replaces the update button");
        await failure.getByRole("button", { name: "Try again" }).waitFor();
        for (const width of [1440, 390]) {
          await page.setViewportSize({ width, height: width === 390 ? 844 : 960 });
          await failure.scrollIntoViewIfNeeded();
          assert.ok(await row.evaluate(e => e.scrollWidth <= e.clientWidth + 1), `${width}px failure has no horizontal overflow`);
          if (process.env.LANTOR_UI_SCREENSHOTS) {
            await mkdir(process.env.LANTOR_UI_SCREENSHOTS, { recursive: true });
            for (const theme of ["light", "dark"]) {
              await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
              await page.screenshot({ animations: "disabled", path: join(process.env.LANTOR_UI_SCREENSHOTS, `cli-update-failure-${width}-${theme}-${engineName}.png`) });
            }
          }
        }
        await page.setViewportSize({ width: 1440, height: 960 });
        await failure.getByRole("button", { name: "Ask @Aster to fix it" }).click();
        await page.locator(".message-list").getByText("failed on this Mac", { exact: false }).first().waitFor();
        const sent = requests.filter(r => r.path === "/api/send_message").map(r => r.args);
        assert.equal(sent.length, 1);
        assert.equal(sent[0].channelId, dmId);
        assert.equal(sent[0].threadRootId, null);
        assert.ok(sent[0].body.startsWith("`codex update` failed on this Mac. Codex is 0.167.0; the latest release is 0.168.0."));
        assert.ok(sent[0].body.includes("npm ERR! fixture line 1\n"), "the agent gets the whole output");
        assert.equal(await page.getByRole("group", { name: "Codex version" }).count(), 0, "the profile closes on the DM");
      }
      assert.deepEqual(errors, []);
      await browser.close(); browser = null;
      console.log(`✓ ${engineName} ${run}`);
    }
  }
} finally {
  await browser?.close(); api.close(); for (const client of clients) client.end();
}
