// Usage card for accounts without usage windows (API keys, enterprise and
// credit plans), against a synthetic API; never starts a real backend.
// Run npm run build first. LANTOR_UI_SCREENSHOTS saves desktop/phone and light/dark evidence.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile, mkdir } from "node:fs/promises";
import { resolve, join } from "node:path";
import { chromium, webkit } from "playwright";

const id = n => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const now = new Date();
const iso = offsetMinutes => new Date(now.getTime() + offsetMinutes * 60_000).toISOString();
const channelId = id(1000);
const baseAgent = { role: "agent", status: "idle", runtime: "codex", model: "", reasoning_effort: "", service_tier: "",
  description: "", launch_command: "", environment_variables: "", working_directory: "", workspace_exists: false,
  workspace_memory_path: "", workspace_memory_exists: false, workspace_entries: [], details_loaded: true, daily_budget_micros: 0 };
const credits = (balance, status = "available") => ({ provider: "codex", plan: "ent26", status, windows: [],
  observed_at: iso(-2), billing: "credits", credits: balance === null ? { unlimited: true, balance: null } : { unlimited: false, balance },
  account_id: "account-1" });
const agent = { ...baseAgent, id: id(800), handle: "Aster", display_name: "Aster", avatar: "A", subscription_status: credits(null) };
const teammate = { ...baseAgent, id: id(801), handle: "Birch", display_name: "Birch", avatar: "B", subscription_status: credits(null) };
const message = (n, sender, body) => ({ id: id(n), seq: n, channel_id: channelId, thread_root_id: null,
  sender_agent_id: sender?.id ?? null, sender_name: sender?.display_name ?? "Owner", sender_role: sender ? "agent" : "owner",
  body, is_task: false, thread_followed: false, delivery_state: "complete", stream_key: "", task_number: null, task_status: null,
  attachments: [], artifacts: [], created_at: iso(-60), updated_at: iso(-60) });
const channels = [{ id: channelId, name: "usage-fixture", kind: "channel", dm_agent_id: null,
  description: "", unread_count: 0, github_unread_count: 0, github_review_synced_at: null }];
const toolsSection = { state: "ready", message: "", observed_at: null, items: [] };
const totals = (day, week, month, runs) => ({ last_day_micros: day, last_week_micros: week, last_month_micros: month, runs_last_month: runs });
const spend = { agent: totals(3_210_000, 40_100_000, 113_160_000, 412),
  account: { agent_count: 2, totals: totals(5_000_000, 61_000_000, 216_850_000, 790) } };

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
    case "/api/load_channel_messages": result = { messages: state.messages, next_before_seq: null, has_more: false }; break;
    case "/api/load_thread_page": result = { messages: [], next_before_seq: null, has_more: false }; break;
    case "/api/load_ui_state": result = Object.fromEntries(args.scopes.map(scope => [scope, state[scope]])); break;
    case "/api/replay_ui_events": result = { cursor, replayGap: false, events: [] }; break;
    case "/api/load_agent_tools": result = { agent_id: args.agentId, runtime: "codex", checked_at: now.toISOString(),
      apps: toolsSection, mcp: toolsSection, plugins: toolsSection, skills: toolsSection }; break;
    case "/api/load_cli_version": result = { runtime: "codex", command: "codex", installed: "0.168.0", latest: "0.168.0",
      update_available: false, problem: null, checked_at: now.toISOString() }; break;
    case "/api/load_agent_spend": result = spend; break;
  }
  res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(result));
});
await new Promise(done => api.listen(0, "127.0.0.1", done));

const freshState = subscription => ({ db_url: "synthetic://subscription-usage", web_base_url: null,
  owner_profile: { display_name: "Owner", avatar: "D", description: "", watched_apps: [] },
  channels, messages: [message(1, null, "Fixture channel"), message(2, { ...agent, subscription_status: subscription }, "Fixture reply")],
  channel_message_history: channels.map(c => ({ channel_id: c.id, before_seq: null, has_more: false })),
  agents: [{ ...agent, subscription_status: subscription }, teammate], tasks: [],
  channel_members: [{ channel_id: channelId, agent_id: agent.id }, { channel_id: channelId, agent_id: teammate.id }],
  thread_activities: [], saved_messages: [], dismissed_inbox_items: {}, read_inbox_items: {}, artifacts: [], reminders: [],
  agent_runs: [], agent_work_items: [], agent_activities: [], supervisor: { pid: null, status: "stopped", updated_at: null },
  launch_agent: { label: "", plist_path: "", installed: false, loaded: false }, ui_event_cursor: 0 });

async function shoot(page, name, clipTo) {
  if (!process.env.LANTOR_UI_SCREENSHOTS) return;
  await mkdir(process.env.LANTOR_UI_SCREENSHOTS, { recursive: true });
  for (const theme of ["light", "dark"]) {
    await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
    const clip = clipTo ? await clipTo.boundingBox() : undefined;
    await page.screenshot({ animations: "disabled", clip, path: join(process.env.LANTOR_UI_SCREENSHOTS, `subscription-usage-${name}-${theme}.png`) });
  }
}

/** The card names the credits, then this agent's spend and the account's, labelled as an estimate. */
async function assertWindowless(section, creditsText) {
  await section.getByText(creditsText, { exact: true }).waitFor();
  await section.getByText("This agent", { exact: true }).waitFor();
  for (const amount of ["$3.21", "$40.10", "$113.16", "$5.00", "$61.00", "$216.85"]) {
    assert.equal(await section.getByText(amount, { exact: true }).count(), 1, `${amount} is shown`);
  }
  await section.getByText("All 2 agents on this account", { exact: true }).waitFor();
  await section.getByText("List-price estimate, not the bill", { exact: true }).waitFor();
  assert.equal(await section.getByText("No usage info").count(), 0);
  assert.ok(await section.evaluate(e => e.scrollWidth <= e.clientWidth + 1), "the usage card has no horizontal overflow");
}

let browser;
try {
  for (const [engineName, engine] of Object.entries({ chromium, webkit })) {
    state = freshState(credits(null)); requests.length = 0;
    browser = await engine.launch();
    const context = await browser.newContext({ serviceWorkers: "block", viewport: { width: 1440, height: 960 } });
    const page = await context.newPage(); page.setDefaultTimeout(10000);
    const errors = []; page.on("pageerror", error => errors.push(error.message));
    await page.goto(`http://127.0.0.1:${api.address().port}`, { waitUntil: "domcontentloaded" });

    await page.getByRole("button", { name: "@Aster’s profile", exact: true }).first().click();
    await page.getByRole("tab", { name: /^Profile/ }).click();
    const section = page.locator(".subscription-section");
    await assertWindowless(section, "Unlimited");
    assert.deepEqual(requests.filter(r => r.path === "/api/load_agent_spend").map(r => r.args), [{ agentId: agent.id }]);
    for (const width of [1440, 390]) {
      await page.setViewportSize({ width, height: width === 390 ? 844 : 960 });
      await section.scrollIntoViewIfNeeded();
      assert.ok(await section.evaluate(e => e.scrollWidth <= e.clientWidth + 1), `${width}px usage card has no horizontal overflow`);
      await shoot(page, `profile-${width}-${engineName}`, section);
    }
    assert.deepEqual(errors, []);
    await browser.close(); browser = null;
    console.log(`✓ ${engineName} profile`);

    // The avatar hover card shows a credit balance, and that spend control stopped the account.
    state = freshState(credits("62500.0000000000", "limited")); requests.length = 0;
    browser = await engine.launch();
    const hoverPage = await (await browser.newContext({ serviceWorkers: "block", viewport: { width: 1440, height: 960 } })).newPage();
    hoverPage.setDefaultTimeout(10000);
    const hoverErrors = []; hoverPage.on("pageerror", error => hoverErrors.push(error.message));
    await hoverPage.goto(`http://127.0.0.1:${api.address().port}`, { waitUntil: "domcontentloaded" });
    await hoverPage.locator(".message-list .agent-avatar-profile-anchor").first().hover();
    const card = hoverPage.getByRole("tooltip");
    await card.getByText("@Aster", { exact: true }).waitFor();
    await assertWindowless(card, "62,500 left");
    await card.getByText("Limit reached", { exact: true }).waitFor();
    await card.getByText("Spend limit reached", { exact: true }).waitFor();
    await shoot(hoverPage, `hover-${engineName}`, card);
    assert.deepEqual(hoverErrors, []);
    await browser.close(); browser = null;
    console.log(`✓ ${engineName} hover`);
  }
} finally {
  await browser?.close(); api.close(); for (const client of clients) client.end();
}
