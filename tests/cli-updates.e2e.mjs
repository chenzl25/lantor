// CLI version row and the update request sent to the agent, against a
// synthetic API; never starts a real backend, runs a CLI or reaches the npm registry.
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

const behind = { runtime: "codex", command: "codex", installed: "0.167.0", latest: "0.168.0", update_available: true,
  problem: null, checked_at: now.toISOString() };
const current = { ...behind, installed: "0.168.0", update_available: false };
const requestStart = "Codex 0.168.0 is out; this Mac has 0.167.0.";
// What load_cli_version returns; a test flips it once the agent "updated".
let installed = behind;

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
    case "/api/load_cli_version": result = installed; break;
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

// Clips the page rather than screenshotting the card, which would scroll it and
// move the avatar out from under the pointer.
async function shootCard(page, card, name) {
  if (!process.env.LANTOR_UI_SCREENSHOTS) return;
  await mkdir(process.env.LANTOR_UI_SCREENSHOTS, { recursive: true });
  for (const theme of ["light", "dark"]) {
    await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
    const clip = await card.boundingBox();
    await page.screenshot({ animations: "disabled", clip, path: join(process.env.LANTOR_UI_SCREENSHOTS, `cli-update-hover-${name}-${theme}.png`) });
  }
}

const agentMessage = { ...message(2, channelId, "Fixture reply"), sender_agent_id: agent.id, sender_name: agent.display_name, sender_role: "agent" };
const hoverState = () => ({ ...freshState(), messages: [message(1, channelId, "Fixture channel"), agentMessage] });
const reads = () => requests.filter(r => r.path === "/api/load_cli_version").length;

/** The request reached the agent's DM as an owner message, and Lantor ran nothing itself. */
async function assertAskedAster(page) {
  await page.locator(".message-list").getByText(requestStart, { exact: false }).first().waitFor();
  const sent = requests.filter(r => r.path === "/api/send_message").map(r => r.args);
  assert.equal(sent.length, 1);
  assert.equal(sent[0].channelId, dmId);
  assert.equal(sent[0].threadRootId, null);
  assert.ok(sent[0].body.startsWith(requestStart), sent[0].body);
  assert.ok(sent[0].body.includes("`codex update` usually works"));
  assert.ok(requests.every(r => r.path !== "/api/update_cli"), "Lantor no longer runs the update itself");
}

async function openProfile(page) {
  await page.getByRole("button", { name: "@Aster’s profile", exact: true }).first().click();
  await page.getByRole("tab", { name: /^Profile/ }).click();
  return page.getByRole("group", { name: "Codex version" });
}

let browser;
try {
  for (const [engineName, engine] of Object.entries({ chromium, webkit })) {
    installed = behind; state = freshState(); requests.length = 0;
    browser = await engine.launch();
    const context = await browser.newContext({ serviceWorkers: "block", viewport: { width: 1440, height: 960 } });
    const page = await context.newPage(); page.setDefaultTimeout(10000);
    const errors = []; page.on("pageerror", error => errors.push(error.message));
    await page.goto(`http://127.0.0.1:${api.address().port}`, { waitUntil: "domcontentloaded" });
    let row = await openProfile(page);
    await row.getByText("0.167.0", { exact: true }).waitFor();
    assert.deepEqual(requests.filter(r => r.path === "/api/load_cli_version").map(r => r.args), [{ runtime: "codex" }]);
    await row.getByText("Update asks @Aster to install it from its shell.", { exact: false }).waitFor();
    for (const width of [1440, 390]) {
      await page.setViewportSize({ width, height: width === 390 ? 844 : 960 });
      await row.scrollIntoViewIfNeeded();
      assert.ok(await row.evaluate(e => e.scrollWidth <= e.clientWidth + 1), `${width}px version row has no horizontal overflow`);
      if (process.env.LANTOR_UI_SCREENSHOTS) {
        await mkdir(process.env.LANTOR_UI_SCREENSHOTS, { recursive: true });
        for (const theme of ["light", "dark"]) {
          await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
          await page.screenshot({ animations: "disabled", path: join(process.env.LANTOR_UI_SCREENSHOTS, `cli-update-profile-${width}-${theme}-${engineName}.png`) });
        }
      }
    }
    await page.setViewportSize({ width: 1440, height: 960 });

    await row.getByRole("button", { name: "Update to 0.168.0" }).click();
    await assertAskedAster(page);
    assert.equal(await page.getByRole("group", { name: "Codex version" }).count(), 0, "the profile closes on the DM");

    // Once the agent has updated, the profile reads the new version.
    installed = current;
    row = await openProfile(page);
    await row.getByText("Latest", { exact: true }).waitFor();
    assert.equal(await row.getByText("0.168.0", { exact: true }).count(), 1);
    assert.equal(await row.getByRole("button").count(), 0);
    assert.deepEqual(errors, []);
    await browser.close(); browser = null;
    console.log(`✓ ${engineName} profile`);
  }

  // The avatar hover card offers the same request, so it is found without opening the profile.
  for (const [engineName, engine] of Object.entries({ chromium, webkit })) {
    for (const run of ["latest", "behind"]) {
      installed = run === "latest" ? current : behind; state = hoverState(); requests.length = 0;
      browser = await engine.launch();
      const context = await browser.newContext({ serviceWorkers: "block", viewport: { width: 1440, height: 960 } });
      const page = await context.newPage(); page.setDefaultTimeout(10000);
      const errors = []; page.on("pageerror", error => errors.push(error.message));
      await page.goto(`http://127.0.0.1:${api.address().port}`, { waitUntil: "domcontentloaded" });
      const avatar = page.locator(".message-list .agent-avatar-profile-anchor").first();
      const firstRead = page.waitForResponse(response => response.url().endsWith("/api/load_cli_version"));
      await avatar.hover();
      const card = page.getByRole("tooltip");
      await card.getByText("@Aster", { exact: true }).waitFor();
      const hint = card.getByRole("group", { name: "Codex update" });

      if (run === "latest") {
        await firstRead;
        await page.evaluate(() => new Promise(done => requestAnimationFrame(() => requestAnimationFrame(done))));
        assert.equal(reads(), 1, "the card read the status");
        assert.equal(await hint.count(), 0, "an up-to-date CLI adds nothing to the card");
      } else {
        await hint.getByText("Codex 0.168.0 is available").waitFor();
        await hint.getByText("This Mac has 0.167.0. Update asks @Aster to install it.").waitFor();
        assert.ok(await card.evaluate(e => e.scrollWidth <= e.clientWidth + 1), "the card has no horizontal overflow");
        await shootCard(page, card, `available-${engineName}`);
        // Leave and come back: the card reuses the status it just read.
        await page.mouse.move(5, 950);
        await card.waitFor({ state: "detached" });
        await avatar.hover();
        await hint.getByText("Codex 0.168.0 is available").waitFor();
        assert.equal(reads(), 1, "a second hover within a minute does not read the CLI again");

        await hint.getByRole("button", { name: "Update", exact: true }).click();
        await assertAskedAster(page);

        // Back in the channel after the agent updated, the card reads the version again.
        installed = current;
        await page.locator(".sidebar .channel").filter({ hasText: "cli-fixture" }).click();
        const secondRead = page.waitForResponse(response => response.url().endsWith("/api/load_cli_version"));
        await avatar.hover();
        await card.getByText("@Aster", { exact: true }).waitFor();
        await secondRead;
        assert.equal(reads(), 2, "the hover after the request reads the CLI again");
        await page.evaluate(() => new Promise(done => requestAnimationFrame(() => requestAnimationFrame(done))));
        assert.equal(await card.getByRole("group", { name: "Codex update" }).count(), 0, "the updated CLI adds nothing to the card");
      }
      assert.deepEqual(errors, []);
      await browser.close(); browser = null;
      console.log(`✓ ${engineName} hover ${run}`);
    }
  }
} finally {
  await browser?.close(); api.close(); for (const client of clients) client.end();
}
