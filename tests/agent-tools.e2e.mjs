// Tools disclosure against a synthetic API; never starts a real backend or provider.
// Run npm run build first. No live workspace data is read or mutated.
// LANTOR_UI_SCREENSHOTS saves desktop/phone and light/dark evidence.
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
const root = { id: id(1), seq: 1, channel_id: channelId, thread_root_id: null, sender_agent_id: null, sender_name: "Owner",
  sender_role: "owner", body: "Forward #github-pulls here", is_task: false, thread_followed: false, delivery_state: "complete",
  stream_key: "", task_number: null, task_status: null, attachments: [], artifacts: [], created_at: iso(-60), updated_at: iso(-60) };
const channels = [{ id: channelId, name: "tool-fixture", kind: "channel", dm_agent_id: null },
  { id: dmId, name: `dm:${agent.id}`, kind: "dm", dm_agent_id: agent.id }].map(c => ({ ...c, description: "", unread_count: 0, github_unread_count: 0, github_review_synced_at: null }));
const section = items => ({state:"ready",message:"",observed_at:null,items});
const tools = Array.from({length:272}, (_,n)=>({name:`fixture_tool_${n}`,description:`Synthetic tool ${n}`}));
const app = (name,status,source,tool_count,plugin=null) => ({name,status,source,tool_count,plugin,detail:null});
const defaultWatched = ["Notion","Slack","Gmail"];
const codexResult = {agent_id:agent.id,runtime:"codex",checked_at:now.toISOString(),
  apps:section([app("Notion","connected","chatgpt_app",81,"notion-fixture"),app("Gmail","connected","chatgpt_app",21,"mail-fixture"),
    app("Google Drive","connected","chatgpt_app",45),app("Drafts","needs_setup","chatgpt_app",0)]),
  mcp:section([
    {name:"apps",status:"unknown",auth_status:"oAuth",tools_error:null,tools},
    {name:"docs",status:"unknown",auth_status:"unsupported",tools_error:null,tools:tools.slice(0,5)},
    ...["review","computer","local"].map(name=>({name,status:"unknown",auth_status:"unknown",tools_error:null,tools:[]})),
    {name:"repl",status:"unknown",auth_status:"unsupported",tools_error:null,tools:tools.slice(0,4)},
    {name:"needs-login",status:"unknown",auth_status:"notLoggedIn",tools_error:"Sign in to read tools",tools:[]},
  ]),
  plugins:section([{name:"notion-fixture",description:"",builtin:false,enabled:true,apps:["Notion"]},
    {name:"mail-fixture",description:"",builtin:false,enabled:true,apps:["Gmail"]},
    ...Array.from({length:20},(_,n)=>({name:`Installed plugin ${n}`,description:"Synthetic plugin",builtin:false,enabled:true,apps:[]}))]),
  skills:section(Array.from({length:64},(_,n)=>({name:`Skill ${n}`,description:"Synthetic skill"})))};
const claudeResult={...codexResult,runtime:"claude",
  apps:section([app("Notion","login_required","claude_connector",0),app("Workspace docs","connected","mcp_server",8),
    app("Single tool","connected","mcp_server",1)]),
  mcp:section([{name:"Workspace docs",status:"connected",auth_status:null,tools_error:null,tools:tools.slice(0,8)},
    {name:"Single tool",status:"connected",auth_status:null,tools_error:null,tools:tools.slice(0,1)},
    {name:"Zeta login",status:"needs-auth",auth_status:null,tools_error:null,tools:[]}]),
  plugins:{...section([...Array.from({length:3},(_,n)=>({name:`User plugin ${n}`,description:"",builtin:false,enabled:true,apps:[]})),
    ...Array.from({length:3},(_,n)=>({name:`internal-${n}@builtin`,description:"",builtin:true,enabled:true,apps:[]}))]),observed_at:iso(-30)},
  skills:{...section(Array.from({length:33},(_,n)=>({name:`Workspace skill ${n}`,description:""}))),observed_at:iso(-30)}};
let toolsResult=codexResult,failNext=false,failWatchNext=false;
const state = { db_url: "synthetic://agent-tools", web_base_url: null, owner_profile: { display_name: "Owner", avatar: "D", description: "", watched_apps: [...defaultWatched] },
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
    case "/api/load_agent_tools":
      if (failNext) { failNext = false; res.writeHead(503).end(JSON.stringify({message:"fixture failure"})); return; }
      result = toolsResult;
      break;
    case "/api/update_watched_apps":
      if (failWatchNext) { failWatchNext = false; res.writeHead(503).end(JSON.stringify({message:"fixture failure"})); return; }
      state.owner_profile = { ...state.owner_profile, watched_apps: args.apps };
      result = { apps: args.apps };
      setTimeout(() => publish({ type: "refresh", reason: "owner_profile_updated" }), 0);
      break;

  }
  res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(result));
});
await new Promise(done => api.listen(0, "127.0.0.1", done));
let browser;
const timings = [];
try {
  for (const [engineName, engine] of Object.entries({ chromium, webkit })) {
    for (const runtime of ["codex", "claude"]) {
      agent.runtime = runtime;
      toolsResult = structuredClone(runtime === "codex" ? codexResult : claudeResult);
      state.owner_profile = { ...state.owner_profile, watched_apps: [...defaultWatched] };
      requests.length = 0;
      browser = await engine.launch();
      const context = await browser.newContext({ serviceWorkers: "block", viewport: { width: 1440, height: 960 } });
      const page = await context.newPage(); page.setDefaultTimeout(10000);
      const errors = []; page.on("pageerror", error => errors.push(error.message));
      await page.goto(`http://127.0.0.1:${api.address().port}`, { waitUntil: "domcontentloaded" });
      await page.getByRole("button", { name: "@Aster’s profile", exact: true }).click();
      await page.getByRole("tab", { name: /^Profile/ }).click();
      const panel = page.getByRole("region", { name: "Agent tools" });
      await panel.waitFor();
      const toolRequests = () => requests.filter(r => r.path === "/api/load_agent_tools");
      assert.equal(toolRequests().length, 0, "opening Profile does not probe");
      const disclosure = panel.getByRole("button", { name: "Tools", exact: true });
      assert.equal(await disclosure.getAttribute("aria-expanded"), "false");
      await disclosure.click();
      await panel.getByText(/Checked /).waitFor();
      assert.deepEqual(toolRequests().map(r => r.args), [{ agentId: agent.id, refresh: false }]);
      assert.equal(await panel.locator(".agent-tools-list").count(), 0, "tool names render on demand");
      assert.equal(await panel.locator(".agent-tools-group ul").count(), 0, "plugins and skills start collapsed");
      const appRows = panel.locator(".agent-tools-apps > ul > .agent-tool-app");
      assert.deepEqual(await appRows.locator("strong").allTextContents(), defaultWatched, "watched apps keep their order");
      const otherApps = panel.getByRole("button", { name: /^Other apps/ });
      if (runtime === "codex") {
        assert.match(await appRows.nth(0).textContent(), /Connected · 81 tools.*ChatGPT app · notion-fixture plugin/);
        assert.match(await appRows.nth(1).textContent(), /Not connected.*Connect it as a ChatGPT app or add an MCP server/);
        assert.match(await appRows.nth(2).textContent(), /Connected · 21 tools/);
        assert.match(await otherApps.textContent(), /1 connected/);
        await otherApps.click();
        const others = panel.locator(".agent-tools-other-apps .agent-tool-app");
        assert.deepEqual(await others.locator("strong").allTextContents(), ["Drafts", "Google Drive"], "problems first");
        assert.match(await others.first().textContent(), /Needs setup/);
        await otherApps.click();
      } else {
        assert.ok(await appRows.nth(0).evaluate(e => e.classList.contains("needs-attention")), "a login problem stands out");
        assert.match(await appRows.nth(0).textContent(), /Login required.*claude\.ai connector/);
        assert.match(await appRows.nth(1).textContent(), /Not connected.*Add it as a claude\.ai connector or an MCP server/);
        assert.match(await otherApps.textContent(), /2 connected/);
      }
      if (runtime === "codex") {
        assert.match(await panel.locator(".agent-tool-server").first().textContent(), /needs-login/);
        assert.equal(await panel.locator(".agent-tool-server .connected").count(), 0, "a Codex MCP catalog never implies an online connection");
        assert.equal(await panel.locator(".agent-tool-server").filter({ hasText: "review" }).getByText("Unknown · 0 tools").count(), 1);
      } else {
        assert.equal(await panel.getByText("Connected · 8 tools").count(), 1);
        assert.equal(await panel.getByText("Connected · 1 tool", { exact: true }).count(), 1);
        assert.equal(await panel.getByText("3 installed · 3 built-in").count(), 1);
        const first = panel.locator(".agent-tool-server").first();
        assert.match(await first.textContent(), /Zeta login.*Login required · 0 tools/);
        assert.ok(await first.evaluate(e => e.classList.contains("needs-attention")), "a login failure stands out");
      }
      for (const width of [1440, 390]) {
        await page.setViewportSize({ width, height: width === 390 ? 844 : 960 });
        await panel.scrollIntoViewIfNeeded();
        assert.ok(await panel.evaluate(e => e.scrollWidth <= e.clientWidth + 1), `${width}px tools has no horizontal overflow`);
        if (process.env.LANTOR_UI_SCREENSHOTS) {
          await mkdir(process.env.LANTOR_UI_SCREENSHOTS, { recursive: true });
          for (const theme of ["light", "dark"]) {
            await page.evaluate(theme => { document.documentElement.dataset.theme = theme; }, theme);
            await page.screenshot({ animations:"disabled", path: join(process.env.LANTOR_UI_SCREENSHOTS, `agent-tools-${runtime}-${width}-${theme}-${engineName}.png`) });
          }
        }
        if (runtime === "codex") {
          const apps = panel.locator(".agent-tool-server").filter({ hasText: "apps" });
          await apps.getByRole("button").click();
          assert.equal(await apps.locator("li").count(), 272);
          const started = performance.now();
          await apps.getByRole("searchbox", { name: "Search tools" }).fill("fixture_tool_271");
          await page.waitForFunction(() => document.querySelectorAll(".agent-tool-server .agent-tools-list li").length === 1);
          const elapsed = performance.now() - started;
          assert.ok(elapsed < 750, `272 tool search completed in ${elapsed.toFixed(1)}ms`);
          assert.equal(await apps.locator("li code").textContent(), "fixture_tool_271");
          timings.push({ engine: engineName, width, search_ms: elapsed });
          await apps.getByRole("button").click();
        }
      }
      await panel.getByRole("button", { name: /^Plugins/ }).click();
      assert.equal(await panel.locator(".agent-tools-group").first().locator("li").count(), runtime === "codex" ? 22 : 6);
      if (runtime === "codex") {
        const content = await panel.locator(".agent-tools-group").first().textContent();
        assert.match(content, /Provide apps.*notion-fixture → Notion/);
        assert.match(content, /Other plugins/);
      }
      if (runtime === "claude") {
        const content = await panel.locator(".agent-tools-group").first().textContent();
        assert.ok(content.indexOf("User plugin 0") < content.indexOf("internal-0@builtin"));
        assert.match(content, /From the last workspace run/);
      }
      await panel.getByRole("button", { name: /^Skills/ }).click();
      assert.equal(await panel.locator(".agent-tools-group").last().locator("li").count(), runtime === "codex" ? 64 : 33);
      const restarted = page.waitForResponse(response => response.url().endsWith("/api/start_agent"));
      await panel.getByRole("button", { name: "Restart", exact: true }).click();
      await restarted;
      assert.deepEqual(requests.filter(r => r.path === "/api/start_agent").map(r => r.args), [{ agentId: agent.id }]);
      await panel.getByRole("button", { name: "Edit", exact: true }).click();
      await panel.getByRole("button", { name: "Stop watching Slack" }).click();
      const watchInput = panel.getByRole("textbox", { name: "App to watch" });
      await watchInput.fill("  linear ");
      await watchInput.press("Enter");
      await watchInput.fill("notion");
      await watchInput.press("Enter");
      failWatchNext = true;
      await panel.getByRole("button", { name: "Save", exact: true }).click();
      await panel.getByText("Could not save. Try again.").waitFor();
      await panel.getByRole("button", { name: "Save", exact: true }).click();
      await page.waitForFunction(() => [...document.querySelectorAll(".agent-tools-apps > ul > .agent-tool-app strong")]
        .map(e => e.textContent).join() === "Notion,Gmail,linear");
      assert.deepEqual(requests.filter(r => r.path === "/api/update_watched_apps").at(-1).args, { apps: ["Notion", "Gmail", "linear"] },
        "trimmed, and a differently cased duplicate is not added");
      assert.equal(await panel.getByRole("textbox", { name: "App to watch" }).count(), 0, "saving closes the editor");
      assert.match(await appRows.nth(2).textContent(), /Not connected/);
      toolsResult = { ...toolsResult, mcp: section(toolsResult.mcp.items.slice(0, 1)) };
      await panel.getByRole("button", { name: "Refresh", exact: true }).click();
      await panel.getByText(/^1 MCP server · /).waitFor();
      failNext = true;
      await panel.getByRole("button", { name: "Refresh", exact: true }).click();
      await panel.getByRole("alert").waitFor();
      assert.equal(toolRequests().at(-1).args.refresh, true);
      assert.match(await panel.getByRole("alert").textContent(), /Could not read status/);
      toolsResult = { ...toolsResult, mcp: {state:"unavailable",message:"Could not read status",observed_at:null,items:[]} };
      await panel.getByRole("button", { name: "Refresh", exact: true }).click();
      await panel.getByText("MCP: Could not read status", { exact: true }).waitFor();
      assert.equal(await panel.locator(".agent-tools-group").last().locator("li").count(), runtime === "codex" ? 64 : 33, "MCP failure preserves skills");
      await disclosure.click();
      await disclosure.click();
      await panel.getByText(/Checked /).waitFor();
      assert.equal(toolRequests().at(-1).args.refresh, false, "reopening uses the backend cache");
      assert.deepEqual(errors, [], `${engineName} ${runtime} page errors`);
      agent.status = "running";
      const busyPage = await context.newPage(); busyPage.setDefaultTimeout(10000);
      await busyPage.goto(`http://127.0.0.1:${api.address().port}`, { waitUntil: "domcontentloaded" });
      await busyPage.getByRole("button", { name: "@Aster’s profile", exact: true }).click();
      await busyPage.getByRole("tab", { name: /^Profile/ }).click();
      const busyPanel = busyPage.getByRole("region", { name: "Agent tools" });
      await busyPanel.getByRole("button", { name: "Tools", exact: true }).click();
      await busyPanel.getByText(/Checked /).waitFor();
      const busyRestart = busyPanel.getByRole("button", { name: "Restart", exact: true });
      assert.ok(await busyRestart.isDisabled(), "Restart follows the same rule as the header button");
      assert.equal(await busyRestart.getAttribute("title"), "Agent is already running");
      agent.status = "idle";
      await browser.close(); browser = null;
      console.log(`agent tools e2e passed: ${engineName} ${runtime}`);
    }
  }
  console.log(JSON.stringify({ timings }));
} finally {
  await browser?.close(); api.close(); for (const client of clients) client.end();
}
