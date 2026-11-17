// Reports (artifacts) in the production App against an isolated API, on a
// desktop browser and on an iPhone Home Screen web app: one tap on the card
// opens the reader, the chat does not repeat the card, the phone reader fills
// the screen, copy and share/download hand over the Markdown, a report read
// once opens again without the server, and a long path in a table cell does
// not widen the table on a phone. Run npm run build first. No live workspace
// data is read or mutated.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { chromium, devices, webkit } from "playwright";

const id = (n) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const now = new Date().toISOString(), channelId = id(1000);
const agent = { id: id(800), handle: "arch", display_name: "Arch", role: "agent", status: "idle", runtime: "claude",
  model: "", reasoning_effort: "", service_tier: "", avatar: "A", description: "", launch_command: "", environment_variables: "",
  working_directory: "", workspace_exists: false, workspace_memory_path: "", workspace_memory_exists: false,
  workspace_entries: [], details_loaded: true, daily_budget_micros: 0, subscription_status: null };
const message = (n, extra = {}) => ({ id: id(n), seq: n, channel_id: channelId, thread_root_id: null, sender_agent_id: agent.id,
  sender_name: "Arch", sender_role: "agent", body: `Message ${n}`, is_task: false, thread_followed: false,
  delivery_state: "complete", stream_key: "", task_number: null, task_status: null, attachments: [], artifacts: [],
  created_at: now, updated_at: now, ...extra });
const longPath = "src/runtime/supervisor/handover/replacement_binary_switch_state.rs";
const table = `| File | Change |\n| --- | --- |\n| \`${longPath}\` (new) | Records which binary takes over and when the switch is safe. |\n| \`src/lib.rs\` | Registers the module. |\n`;
const content = `# Deploy review\n\nTwo risks and one blocker before the switch.\n\n## Files\n\n${table}\n## Rollout\n\nSwitch when no agent is working.\n`;
const artifact = (n, messageN, title, summary) => ({ id: id(n), message_id: id(messageN), channel_id: channelId, thread_root_id: null,
  creator_agent_id: agent.id, creator_agent_handle: "arch", kind: "markdown", title, summary, content: "", metadata: {},
  created_at: now, updated_at: now });
const review = artifact(5001, 3, "Deploy review", "Two risks and one blocker before the switch.");
const flaky = artifact(5002, 4, "Flaky report", "Opens on the second try.");
// Agents usually post reports in a thread.
const threaded = { ...artifact(5003, 6, "Thread report", "Posted as a reply."), thread_root_id: id(5) };
const fullContent = { [review.id]: content, [flaky.id]: "# Flaky report\n\nLoaded after a retry.\n",
  [threaded.id]: "Notes from the thread.\n" };
const announce = (report) => `Created artifact: ${report.title}\n\n${report.summary}`;
const messages = [
  message(1, { sender_agent_id: null, sender_name: "Dylan", sender_role: "owner", body: "@arch review the deploy" }),
  message(2, { body: table }),
  message(3, { body: announce(review), artifacts: [review] }),
  message(4, { body: announce(flaky), artifacts: [flaky] }),
  message(5, { sender_agent_id: null, sender_name: "Dylan", sender_role: "owner", body: "Thread with a report" }),
  message(6, { thread_root_id: id(5), body: announce(threaded), artifacts: [threaded] }),
];
const roots = messages.filter((item) => !item.thread_root_id);
const channels = [{ id: channelId, name: "deploys", kind: "channel", dm_agent_id: null, description: "", unread_count: 0,
  github_unread_count: 0, github_review_synced_at: null }];
const state = { db_url: "synthetic://artifacts", web_base_url: null, owner_profile: { display_name: "Dylan", avatar: "D", description: "" },
  channels, messages, channel_message_history: [{ channel_id: channelId, before_seq: null, has_more: false }],
  agents: [agent], channel_members: [{ channel_id: channelId, agent_id: agent.id }], tasks: [],
  thread_activities: [{ thread_root_id: id(5), channel_id: channelId, unread_count: 0, reply_count: 1, latest_message_id: id(6), latest_activity_at: now }],
  saved_messages: [], dismissed_inbox_items: {}, read_inbox_items: {}, artifacts: [review, flaky, threaded], reminders: [], decisions: [],
  agent_runs: [], agent_work_items: [], agent_activities: [], supervisor: { pid: null, status: "stopped", updated_at: null },
  launch_agent: { label: "", plist_path: "", installed: false, loaded: false }, ui_event_cursor: 0 };
let reads = new Map();

const api = createServer(async (req, res) => {
  const url = new URL(req.url, "http://localhost");
  if (!url.pathname.startsWith("/api/")) {
    const file = resolve("dist", url.pathname === "/" ? "index.html" : url.pathname.slice(1));
    if (!file.startsWith(resolve("dist") + "/")) { res.writeHead(404).end(); return; }
    try { const mime = { html: "text/html", js: "application/javascript", css: "text/css", png: "image/png", woff: "font/woff", woff2: "font/woff2", svg: "image/svg+xml" };
      res.writeHead(200, { "content-type": mime[file.split(".").pop()] || "application/octet-stream" }).end(await readFile(file));
    } catch { res.writeHead(404).end(); }
    return;
  }
  if (url.pathname === "/api/events") { res.writeHead(200, { "content-type": "text/event-stream" }); res.write(": ready\n\n"); return; }
  let raw = "";
  for await (const chunk of req) raw += chunk;
  const args = raw ? JSON.parse(raw) : {};
  let result = { ok: true };
  switch (url.pathname) {
    case "/api/bootstrap": result = state; break;
    case "/api/load_channel_previews": case "/api/load_activity_messages": case "/api/search_messages": result = []; break;
    case "/api/load_channel_messages": result = { messages: roots, next_before_seq: null, has_more: false, thread_activities: state.thread_activities }; break;
    case "/api/load_thread_page": result = { messages: messages.filter((item) => item.id === args.threadRootId || item.thread_root_id === args.threadRootId),
      next_before_seq: null, has_more: false }; break;
    case "/api/load_ui_state": result = Object.fromEntries(args.scopes.map((scope) => [scope, state[scope]])); break;
    case "/api/replay_ui_events": result = { cursor: 0, replayGap: false, events: [] }; break;
    case "/api/artifact_read": {
      const count = (reads.get(args.artifactId) ?? 0) + 1;
      reads.set(args.artifactId, count);
      // The deploy review answers once: opening it again must not need the server.
      // The flaky report fails once, then answers.
      const failing = (args.artifactId === review.id && count > 1) || (args.artifactId === flaky.id && count === 1);
      if (failing) { res.writeHead(500, { "content-type": "application/json" }).end(JSON.stringify({ message: "database is locked" })); return; }
      const summary = state.artifacts.find((item) => item.id === args.artifactId);
      result = { ...summary, content: fullContent[args.artifactId] };
      break;
    }
  }
  res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(result));
});
await new Promise((done) => api.listen(0, "127.0.0.1", done));
const appUrl = `http://127.0.0.1:${api.address().port}/#/channel/${channelId}`;

// The Home Screen web app shares instead of downloading; the stub records what it was given.
const homeScreenApp = `
  Object.defineProperty(navigator, "standalone", { configurable: true, value: true });
  window.__shared = [];
  navigator.canShare = (data) => Array.isArray(data?.files) && data.files.length > 0;
  navigator.share = async (data) => {
    for (const file of data.files) window.__shared.push({ name: file.name, type: file.type, text: await file.text() });
  };`;
const recordClipboard = `
  window.__copied = null;
  Object.defineProperty(navigator, "clipboard", { configurable: true,
    value: { writeText: async (text) => { window.__copied = text; } } });`;

const engines = {
  desktop: [chromium, { viewport: { width: 1280, height: 900 } }, ""],
  phone: [webkit, devices["iPhone 14"], homeScreenApp],
};
let browser;
try {
  for (const [name, [engine, device, initScript]] of Object.entries(engines)) {
    reads = new Map();
    browser = await engine.launch();
    const context = await browser.newContext({ ...device, serviceWorkers: "block", acceptDownloads: true });
    await context.addInitScript(recordClipboard + initScript);
    const page = await context.newPage();
    page.setDefaultTimeout(10000);
    const errors = [];
    page.on("pageerror", (error) => errors.push(error.message));
    await page.goto(appUrl, { waitUntil: "domcontentloaded" });
    if (name === "phone") {
      const channelLink = page.locator(".sidebar .channel").filter({ hasText: "deploys" });
      if (await channelLink.count()) await channelLink.first().click();
    }

    // The chat shows one card per report, not the announcement text as well.
    const row = page.locator(`article[data-message-id="${id(3)}"]`);
    const card = row.getByRole("button", { name: /Deploy review/ });
    await card.waitFor();
    assert.equal(await row.getByText("Created artifact").count(), 0, `${name}: the card replaces the announcement`);
    assert.match(await card.innerText(), /Two risks and one blocker/, `${name}: the card shows the summary`);

    // A table in a chat message fits a phone; on a desktop its code stays on one line.
    const chatTable = page.locator(`article[data-message-id="${id(2)}"] .markdown-table-scroll`);
    const chatFit = await chatTable.evaluate((element) => ({ scroll: element.scrollWidth, client: element.clientWidth,
      whiteSpace: getComputedStyle(element.querySelector("td code")).whiteSpace }));
    if (name === "phone") {
      assert.ok(chatFit.scroll <= chatFit.client + 1, `phone: chat table fits (${chatFit.scroll} > ${chatFit.client})`);
    } else {
      assert.equal(chatFit.whiteSpace, "nowrap", "desktop: code in a table cell stays on one line");
    }

    // One tap opens the reader.
    await card.click();
    const reader = page.getByRole("dialog", { name: "Deploy review" });
    await reader.getByRole("heading", { name: "Rollout" }).waitFor();
    assert.match(await reader.locator(".dialog-header").innerText(), /@arch/, `${name}: the reader names the author`);
    // The report's own "# Deploy review" heading would repeat the reader's title.
    assert.equal(await reader.locator(".markdown-body h1").count(), 0, `${name}: the repeated title heading is dropped`);
    const box = await reader.boundingBox();
    const viewport = page.viewportSize();
    if (name === "phone") {
      assert.ok(box.width >= viewport.width - 16 && box.height >= viewport.height - 80,
        `phone: the reader fills the screen (${JSON.stringify(box)})`);
      const readerTable = await reader.locator(".markdown-table-scroll").evaluate((element) => ({ scroll: element.scrollWidth, client: element.clientWidth }));
      assert.ok(readerTable.scroll <= readerTable.client + 1, `phone: report table fits (${JSON.stringify(readerTable)})`);
    } else {
      assert.equal(Math.round(box.width), 960, "desktop: the reader is wide enough for tables");
    }

    await reader.getByRole("button", { name: "Copy Markdown" }).click();
    await reader.getByRole("button", { name: "Copied" }).waitFor();
    assert.equal(await page.evaluate(() => window.__copied), content, `${name}: copy hands over the whole Markdown`);

    if (name === "phone") {
      await reader.getByRole("button", { name: "Share" }).click();
      await page.waitForFunction(() => window.__shared.length === 1);
      assert.deepEqual(await page.evaluate(() => window.__shared), [{ name: "Deploy review.md", type: "text/markdown", text: content }]);
    } else {
      const [download] = await Promise.all([page.waitForEvent("download"), reader.getByRole("button", { name: "Download" }).click()]);
      assert.equal(download.suggestedFilename(), "Deploy review.md");
      assert.equal(await readFile(await download.path(), "utf8"), content);
    }

    // Opening it again shows the report even though the server now fails.
    await reader.getByRole("button", { name: "Close" }).click();
    await reader.waitFor({ state: "detached" });
    await card.click();
    await reader.getByRole("heading", { name: "Rollout" }).waitFor();
    assert.equal(await reader.getByRole("alert").count(), 0, `${name}: the second open reads nothing`);
    assert.equal(reads.get(review.id), 1, `${name}: the report was read once`);
    await reader.getByRole("button", { name: "Close" }).click();
    await reader.waitFor({ state: "detached" });

    // A failed read offers Retry.
    await page.locator(`article[data-message-id="${id(4)}"]`).getByRole("button", { name: /Flaky report/ }).click();
    const flakyReader = page.getByRole("dialog", { name: "Flaky report" });
    await flakyReader.getByRole("alert").filter({ hasText: "database is locked" }).waitFor();
    await flakyReader.getByRole("button", { name: "Retry" }).click();
    await flakyReader.getByText("Loaded after a retry.").waitFor();
    await flakyReader.getByRole("button", { name: "Close" }).click();
    await flakyReader.waitFor({ state: "detached" });

    // A report in a thread opens the same way, and the tap does not also select the reply.
    await page.getByRole("button", { name: "View 1 reply in thread" }).click();
    const reply = page.locator(`.thread article[data-message-id="${id(6)}"]`);
    await reply.getByRole("button", { name: /Thread report/ }).click();
    const threadReader = page.getByRole("dialog", { name: "Thread report" });
    await threadReader.getByText("Notes from the thread.").waitFor();
    await threadReader.getByRole("button", { name: "Close" }).click();
    await threadReader.waitFor({ state: "detached" });
    assert.equal(await page.locator(".message-tap-actions").count(), 0, `${name}: the reply is not selected`);
    assert.equal(await reply.getByText("Created artifact").count(), 0, `${name}: the thread shows only the card`);

    assert.deepEqual(errors, [], `${name}: no page errors`);
    await browser.close();
    browser = null;
    console.log(`${name}: ok`);
  }
} finally {
  await browser?.close();
  api.close();
}
