// Production App against an isolated API: a page left alone does no styling
// work on the main thread. A thread summary with unread replies keeps
// pulsing, but only through opacity, which the compositor animates by itself;
// a box-shadow pulse restyled and repainted it on every frame while on screen.
// Run npm run build first. No live workspace data is read or mutated.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { chromium, webkit } from "playwright";

const id = n => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const now = new Date().toISOString(), channelId = id(1000);
const message = (n, extra = {}) => ({ id: id(n), seq: n, channel_id: channelId, thread_root_id: null, sender_agent_id: null,
  sender_name: "Dylan", sender_role: "owner", body: `Discussion ${n}`, is_task: false, thread_followed: false,
  delivery_state: "complete", stream_key: "", task_number: null, task_status: null, attachments: [], artifacts: [], created_at: now, updated_at: now, ...extra });
const root = message(1, { body: "Release checklist" });
const replies = [message(2, { thread_root_id: root.id, body: "Tagged" }), message(3, { thread_root_id: root.id, body: "Notes drafted" })];
const state = { db_url: "synthetic://idle-animations", web_base_url: null, owner_profile: { display_name: "Dylan", avatar: "D", description: "" },
  channels: [{ id: channelId, name: "release", kind: "channel", dm_agent_id: null, description: "", unread_count: 0, github_unread_count: 0, github_review_synced_at: null }],
  messages: [root, ...replies], channel_message_history: [{ channel_id: channelId, before_seq: null, has_more: false }],
  agents: [], channel_members: [], tasks: [],
  thread_activities: [{ thread_root_id: root.id, channel_id: channelId, unread_count: 2, reply_count: 2, latest_message_id: replies[1].id, latest_activity_at: now }],
  saved_messages: [], dismissed_inbox_items: {}, read_inbox_items: {}, artifacts: [], reminders: [], decisions: [],
  github_inbox: { enabled: false, account_login: null, synced_at: null, error: null, items: [] }, agent_runs: [], agent_work_items: [], agent_activities: [],
  supervisor: { pid: null, status: "stopped", updated_at: null }, launch_agent: { label: "", plist_path: "", installed: false, loaded: false }, ui_event_cursor: 0 };
const api = createServer(async (req, res) => {
  const url = new URL(req.url, "http://localhost");
  if (!url.pathname.startsWith("/api/")) {
    const file = resolve("dist", url.pathname === "/" ? "index.html" : url.pathname.slice(1));
    if (!file.startsWith(resolve("dist") + "/")) { res.writeHead(404).end(); return; }
    try { const mime = { html: "text/html", js: "application/javascript", css: "text/css", woff2: "font/woff2" };
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
    case "/api/load_channel_previews": case "/api/load_activity_messages": case "/api/search_messages":
    case "/api/search_channel_wikis": case "/api/load_github_references": result = []; break;
    case "/api/load_channel_messages": result = { messages: [root], next_before_seq: null, has_more: false }; break;
    case "/api/load_ui_state": result = Object.fromEntries(args.scopes.map(scope => [scope, state[scope] ?? []])); break;
    case "/api/replay_ui_events": result = { cursor: 0, replayGap: false, events: [] }; break;
  }
  res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(result));
});
await new Promise(done => api.listen(0, "127.0.0.1", done));
const origin = `http://127.0.0.1:${api.address().port}`;

// The CSS properties each running animation changes.
const animatedProperties = page => page.evaluate(() => document.getAnimations()
  .filter(animation => animation.playState === "running")
  .map(animation => ({
    name: animation.animationName ?? animation.transitionProperty,
    pseudo: animation.effect.pseudoElement ?? "",
    properties: [...new Set(animation.effect.getKeyframes().flatMap(frame => Object.keys(frame)
      .filter(key => !["offset", "computedOffset", "easing", "composite"].includes(key))))],
  })));

let browser;
try {
  for (const [name, engine] of Object.entries({ chromium, webkit })) {
    browser = await engine.launch();
    const context = await browser.newContext({ viewport: { width: 1280, height: 820 }, serviceWorkers: "block" });
    const page = await context.newPage(); page.setDefaultTimeout(8000);
    const errors = []; page.on("pageerror", error => errors.push(error.message));
    await page.goto(origin, { waitUntil: "domcontentloaded" });
    const summary = page.locator(`.conversation [data-message-id="${root.id}"] .thread-reply-summary.unread-replies`);
    await summary.waitFor();
    await page.waitForTimeout(500);

    // The unread summary still pulses, and everything that animates changes
    // only what the compositor can animate without the main thread.
    const running = await animatedProperties(page);
    const offMainThread = new Set(["opacity", "transform"]);
    assert.deepEqual(running.filter(animation => animation.properties.some(property => !offMainThread.has(property))), [],
      `${name}: only opacity and transform animate while idle`);
    assert.ok(running.some(animation => animation.pseudo === "::after" && animation.properties.includes("opacity")), `${name}: the unread summary pulses ${JSON.stringify(running)}`);

    // Chromium counts style recalculations: an idle page needs none.
    if (name === "chromium") {
      const cdp = await context.newCDPSession(page);
      await cdp.send("Performance.enable");
      const recalcs = async () => (await cdp.send("Performance.getMetrics")).metrics.find(metric => metric.name === "RecalcStyleCount").value;
      const before = await recalcs();
      await page.waitForTimeout(1000);
      const idle = await recalcs() - before;
      assert.ok(idle <= 5, `${name}: ${idle} style recalculations in an idle second`);
    }

    // With reduced motion the summary keeps its resting ring and does not pulse.
    await page.emulateMedia({ reducedMotion: "reduce" });
    assert.deepEqual(await summary.evaluate(element => element.getAnimations({ subtree: true }).length), 0, `${name}: no pulse with reduced motion`);
    assert.equal(await summary.evaluate(element => getComputedStyle(element, "::before").opacity), "1", `${name}: the resting ring stays`);
    assert.deepEqual(errors, [], `${name}: no page errors`);
    await browser.close(); browser = null;
  }
  console.log("idle animations: ok");
} finally {
  await browser?.close();
  await new Promise(done => { api.closeAllConnections?.(); api.close(done); });
}
