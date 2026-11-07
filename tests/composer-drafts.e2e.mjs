// Production App against an isolated API: unsent composer text survives a reload.
// Run npm run build first. No live workspace data is read or mutated.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { chromium, webkit } from "playwright";

const id = n => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const now = new Date().toISOString(), alphaId = id(1000), betaId = id(1001);
let nextSeq = 10;
const message = (channelId, body, extra = {}) => ({ id: id(nextSeq), seq: nextSeq++, channel_id: channelId, thread_root_id: null,
  sender_agent_id: null, sender_name: "Dylan", sender_role: "owner", body, is_task: false, thread_followed: false,
  delivery_state: "complete", stream_key: "", task_number: null, task_status: null, attachments: [], artifacts: [],
  created_at: now, updated_at: now, ...extra });
const channel = (channelId, name) => ({ id: channelId, name, kind: "channel", dm_agent_id: null, description: "",
  unread_count: 0, github_unread_count: 0, github_review_synced_at: null });
const alphaRoot = message(alphaId, "Alpha discussion"), betaRoot = message(betaId, "Beta discussion");
const state = { db_url: "synthetic://composer-drafts", web_base_url: null, owner_profile: { display_name: "Dylan", avatar: "D", description: "" },
  channels: [channel(alphaId, "alpha"), channel(betaId, "beta")], messages: [alphaRoot, betaRoot],
  channel_message_history: [{ channel_id: alphaId, before_seq: null, has_more: false }, { channel_id: betaId, before_seq: null, has_more: false }],
  agents: [], tasks: [], channel_members: [], thread_activities: [], saved_messages: [], dismissed_inbox_items: {}, read_inbox_items: {},
  artifacts: [], reminders: [], agent_runs: [], agent_work_items: [], agent_activities: [], supervisor: { pid: null, status: "stopped", updated_at: null },
  launch_agent: { label: "", plist_path: "", installed: false, loaded: false }, ui_event_cursor: 0 };

const dist = resolve("dist");
const api = createServer(async (req, res) => {
  const url = new URL(req.url, "http://localhost");
  if (!url.pathname.startsWith("/api/")) {
    const file = resolve(dist, url.pathname === "/" ? "index.html" : url.pathname.slice(1));
    if (!file.startsWith(dist + "/")) { res.writeHead(404).end(); return; }
    try { const mime = { html: "text/html", js: "application/javascript", css: "text/css", png: "image/png", woff: "font/woff", woff2: "font/woff2" };
      res.writeHead(200, { "content-type": mime[file.split(".").pop()] || "application/octet-stream" }).end(await readFile(file));
    } catch { res.writeHead(404).end(); }
    return;
  }
  if (url.pathname === "/api/events") { res.writeHead(200, { "content-type": "text/event-stream" }); res.write(": ready\n\n"); return; }
  let raw = ""; for await (const chunk of req) raw += chunk;
  const args = raw ? JSON.parse(raw) : {};
  let result = { ok: true };
  switch (url.pathname) {
    case "/api/bootstrap": result = state; break;
    case "/api/load_channel_previews": case "/api/load_activity_messages": case "/api/search_messages": result = []; break;
    case "/api/load_channel_messages": result = { messages: state.messages.filter(m => m.channel_id === args.channelId && !m.thread_root_id), next_before_seq: null, has_more: false, thread_activities: [] }; break;
    case "/api/load_thread_messages": result = state.messages.filter(m => m.id === args.threadRootId || m.thread_root_id === args.threadRootId); break;
    case "/api/load_ui_state": result = Object.fromEntries(args.scopes.map(scope => [scope, state[scope]])); break;
    case "/api/replay_ui_events": result = { cursor: 0, replayGap: false, events: [] }; break;
    case "/api/send_message": {
      result = message(args.channelId, args.body, { id: args.messageId, thread_root_id: args.threadRootId ?? null });
      state.messages.push(result);
      break;
    }
  }
  res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(result));
});
await new Promise(done => api.listen(0, "127.0.0.1", done));
const origin = `http://127.0.0.1:${api.address().port}`;
const storedDrafts = page => page.evaluate(() => JSON.parse(localStorage.getItem("lantor.composerDrafts.v1") ?? "{}"));
const storedText = drafts => Object.fromEntries(Object.entries(drafts).map(([key, draft]) => [key, draft.text]));

async function run(engine) {
  // Each engine starts from empty storage and the same two root messages.
  state.messages.splice(2);
  const browser = await engine.launch();
  try {
    const context = await browser.newContext({ serviceWorkers: "block", viewport: { width: 1440, height: 900 } });
    const page = await context.newPage(); page.setDefaultTimeout(8000);
    const errors = []; page.on("pageerror", error => errors.push(error.message));
    const composer = name => page.getByRole("textbox", { name: `Message #${name}`, exact: true });
    const replyComposer = () => page.getByRole("textbox", { name: "Reply in thread", exact: true });
    const openChannel = name => page.locator(".sidebar .channel-name").getByText(name, { exact: true }).click();
    const openThread = async root => {
      const row = page.locator(`.conversation [data-message-id="${root.id}"]`);
      await row.hover();
      await row.getByRole("button", { name: "Reply in thread", exact: true }).click();
    };

    await page.goto(origin, { waitUntil: "domcontentloaded" });
    await openChannel("alpha");
    await composer("alpha").fill("Alpha draft");
    await openChannel("beta");
    await composer("beta").fill("Beta draft");
    await openThread(betaRoot);
    await replyComposer().fill("Reply draft");
    await page.waitForFunction(() => localStorage.getItem("lantor.composerDrafts.v1")?.includes("Reply draft"));
    assert.deepEqual(storedText(await storedDrafts(page)), {
      [`channel:${alphaId}`]: "Alpha draft",
      [`channel:${betaId}`]: "Beta draft",
      [`thread:${betaRoot.id}`]: "Reply draft",
    });

    // A reload, as when iOS discards the Home Screen app, brings every draft back.
    await page.reload({ waitUntil: "domcontentloaded" });
    await openChannel("beta");
    assert.equal(await composer("beta").inputValue(), "Beta draft");
    if (!await replyComposer().isVisible()) await openThread(betaRoot);
    assert.equal(await replyComposer().inputValue(), "Reply draft");
    await openChannel("alpha");
    assert.equal(await composer("alpha").inputValue(), "Alpha draft");

    // Sending clears only that composer's stored draft.
    await composer("alpha").press("Enter");
    await page.locator(".conversation").getByText("Alpha draft", { exact: true }).waitFor();
    await page.waitForFunction(alphaKey => !localStorage.getItem("lantor.composerDrafts.v1")?.includes(alphaKey), `channel:${alphaId}`);
    await page.reload({ waitUntil: "domcontentloaded" });
    await openChannel("alpha");
    assert.equal(await composer("alpha").inputValue(), "");
    await openChannel("beta");
    assert.equal(await composer("beta").inputValue(), "Beta draft");

    // Hiding the page saves at once instead of waiting for typing to pause.
    await composer("beta").fill("Beta draft, edited");
    const savedWhenHidden = await page.evaluate(() => {
      Object.defineProperty(document, "visibilityState", { configurable: true, get: () => "hidden" });
      document.dispatchEvent(new Event("visibilitychange"));
      return JSON.parse(localStorage.getItem("lantor.composerDrafts.v1") ?? "{}");
    });
    assert.equal(savedWhenHidden[`channel:${betaId}`]?.text, "Beta draft, edited");

    // A second tab saves its own drafts without erasing this tab's.
    await page.evaluate(() => {
      Object.defineProperty(document, "visibilityState", { configurable: true, get: () => "visible" });
    });
    const other = await context.newPage(); other.setDefaultTimeout(8000);
    other.on("pageerror", error => errors.push(error.message));
    await other.goto(origin, { waitUntil: "domcontentloaded" });
    await other.locator(".sidebar .channel-name").getByText("alpha", { exact: true }).click();
    await other.getByRole("textbox", { name: "Message #alpha", exact: true }).fill("Second tab draft");
    await other.waitForFunction(() => localStorage.getItem("lantor.composerDrafts.v1")?.includes("Second tab draft"));
    await replyComposer().fill("Reply draft, edited");
    await page.waitForFunction(() => localStorage.getItem("lantor.composerDrafts.v1")?.includes("Reply draft, edited"));
    assert.deepEqual(storedText(await storedDrafts(page)), {
      [`channel:${alphaId}`]: "Second tab draft",
      [`channel:${betaId}`]: "Beta draft, edited",
      [`thread:${betaRoot.id}`]: "Reply draft, edited",
    });

    assert.deepEqual(errors, []);
    return browser.version();
  } finally {
    await browser.close();
  }
}

try {
  const versions = {};
  for (const [name, engine] of [["chromium", chromium], ["webkit", webkit]]) versions[name] = await run(engine);
  console.log(JSON.stringify({ versions, checks: "reload restores channel and thread drafts, send clears, hidden page saves, tabs merge" }));
} finally {
  api.close();
}
