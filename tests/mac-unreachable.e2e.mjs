// Production App on a phone whose Mac is asleep: the tunnel answers every API
// call with its own HTML error page. Lantor must say the Mac is out of reach,
// never show the page, keep retrying, and recover on its own when the Mac
// wakes up. Run npm run build first.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { mkdir } from "node:fs/promises";
import { join, resolve } from "node:path";
import { chromium, webkit } from "playwright";

const id = n => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const now = new Date().toISOString();
const channelId = id(1000);
const message = (n, body, sender_name = "Dylan", sender_role = "owner") => ({ id: id(n), seq: n, channel_id: channelId,
  thread_root_id: null, sender_agent_id: null, sender_name, sender_role, body, is_task: false, thread_followed: false, delivery_state: "complete",
  stream_key: "", task_number: null, task_status: null, attachments: [], artifacts: [], created_at: now, updated_at: now });
// Different senders, so each row has its own header.
const messages = [message(1, "Plain message"), message(2, "Saved message", "Hancock", "agent")];
const channels = [{ id: channelId, name: "alpha", kind: "channel", dm_agent_id: null, description: "", unread_count: 0,
  github_unread_count: 0, github_review_synced_at: null }];
const state = { db_url: "synthetic://mac-unreachable", web_base_url: null, owner_profile: { display_name: "Dylan", avatar: "D", description: "" },
  channels, messages, channel_message_history: [{ channel_id: channelId, before_seq: null, has_more: false }],
  agents: [], tasks: [], channel_members: [], thread_activities: [], dismissed_inbox_items: {}, read_inbox_items: {},
  saved_messages: [{ id: id(3000), message_id: messages[1].id, channel_id: channelId, channel_name: "alpha", thread_root_id: null,
    sender_name: "Hancock", sender_role: "agent", body: messages[1].body, created_at: now, saved_at: now }],
  artifacts: [], reminders: [], agent_runs: [], agent_work_items: [], agent_activities: [], decisions: [],
  supervisor: { pid: null, status: "stopped", updated_at: null }, launch_agent: { label: "", plist_path: "", installed: false, loaded: false },
  ui_event_cursor: 0 };

const tunnelPage = `<!DOCTYPE html><html><head><title>Cloudflare Tunnel error</title></head><body>${"<p>error code: 1033</p>".repeat(80)}</body></html>`;
let macAwake = true;
let sends = 0;
const eventClients = new Set();
const api = createServer(async (req, res) => {
  const url = new URL(req.url, "http://localhost");
  if (!url.pathname.startsWith("/api/")) {
    const file = resolve("dist", url.pathname === "/" ? "index.html" : url.pathname.slice(1));
    if (!file.startsWith(resolve("dist") + "/")) { res.writeHead(404).end(); return; }
    try {
      const mime = { html: "text/html", js: "application/javascript", css: "text/css", png: "image/png", woff: "font/woff", woff2: "font/woff2" };
      res.writeHead(200, { "content-type": mime[file.split(".").pop()] || "application/octet-stream" }).end(await readFile(file));
    } catch { res.writeHead(404).end(); }
    return;
  }
  // The service worker serves the app shell; only the API needs the Mac.
  if (!macAwake) { res.writeHead(530, { "content-type": "text/html" }).end(tunnelPage); return; }
  if (url.pathname === "/api/events") {
    res.writeHead(200, { "content-type": "text/event-stream" }); res.write(": ready\n\n");
    eventClients.add(res); res.on("close", () => eventClients.delete(res)); return;
  }
  let raw = "";
  try { for await (const chunk of req) raw += chunk; } catch { return; }
  let result = { ok: true };
  switch (url.pathname) {
    case "/api/bootstrap": result = state; break;
    case "/api/load_channel_previews": case "/api/load_activity_messages": case "/api/search_messages": result = []; break;
    case "/api/load_channel_messages": result = { messages, next_before_seq: null, has_more: false }; break;
    case "/api/load_ui_state": { const args = raw ? JSON.parse(raw) : {}; result = Object.fromEntries(args.scopes.map(scope => [scope, state[scope]])); break; }
    case "/api/replay_ui_events": result = { cursor: 0, replayGap: false, events: [] }; break;
    case "/api/send_message": sends += 1; break;
  }
  res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(result));
});
await new Promise(done => api.listen(0, "127.0.0.1", done));
const appUrl = `http://127.0.0.1:${api.address().port}`;

function sleep() {
  macAwake = false;
  for (const client of eventClients) client.destroy();
}

async function screenshot(page, label, name) {
  if (!process.env.LANTOR_UI_SCREENSHOTS) return;
  await mkdir(process.env.LANTOR_UI_SCREENSHOTS, { recursive: true });
  await page.screenshot({ path: join(process.env.LANTOR_UI_SCREENSHOTS, `mac-unreachable-${label}-${name}.png`) });
}

async function run(browserType, name) {
  const browser = await browserType.launch();
  const context = await browser.newContext({ viewport: { width: 393, height: 852 }, isMobile: name === "webkit",
    hasTouch: true, serviceWorkers: "block" });
  const page = await context.newPage();
  const pageErrors = [];
  page.on("pageerror", error => pageErrors.push(error));
  const status = page.locator(".web-app-status");
  const outOfReach = status.getByText("Can’t reach your Mac. It may be asleep.", { exact: true });
  const channelRow = page.locator(".sidebar .channel").filter({ hasText: "alpha" }).first();
  try {
    // Cold launch while the Mac sleeps: the splash keeps trying and the status
    // bar says why, long after a toast would have cleared.
    sleep();
    await page.goto(appUrl, { waitUntil: "domcontentloaded" });
    await outOfReach.waitFor({ timeout: 8000 });
    await page.waitForTimeout(7000);
    assert.equal(await outOfReach.isVisible(), true, "the reason stays on screen");
    assert.equal(await page.locator(".boot").getByText("Opening your workspace").isVisible(), true);
    assert.doesNotMatch(await page.locator("body").innerText(), /error code|<html|DOCTYPE/i);
    await screenshot(page, "launch", name);

    // The Mac wakes up: Lantor opens without a reload or a tap.
    macAwake = true;
    await channelRow.waitFor({ timeout: 12000 });
    await outOfReach.waitFor({ state: "detached", timeout: 8000 });

    // Phones save from a tapped thread message's actions; only saved messages carry the tag.
    await channelRow.click();
    await page.locator(".conversation").getByText("Plain message").waitFor();
    if (name === "webkit") {
      const tags = page.locator(".conversation .mobile-message-save-tag");
      assert.equal(await tags.count(), 1);
      assert.equal(await tags.first().getAttribute("aria-label"), "Remove from Saved");
    }

    // The Mac falls asleep while the app is open.
    sleep();
    await outOfReach.waitFor({ timeout: 12000 });
    const composer = page.locator(".conversation textarea").first();
    await composer.fill("Sent while the Mac sleeps");
    await page.locator(".conversation").getByRole("button", { name: "Send message", exact: true }).click();
    const toast = page.locator(".app-toast, [role=alert]").filter({ hasText: "Can’t reach your Mac" });
    await toast.first().waitFor({ timeout: 5000 });
    assert.doesNotMatch(await page.locator("body").innerText(), /error code|<html|DOCTYPE/i, "the tunnel's page never shows");
    assert.equal(await composer.inputValue(), "Sent while the Mac sleeps", "the draft is kept");
    assert.equal(sends, 0);
    await screenshot(page, "send", name);

    // It wakes again: the status clears without a reload.
    macAwake = true;
    await outOfReach.waitFor({ state: "detached", timeout: 15000 });
    assert.deepEqual(pageErrors, []);
    console.log(`${name}: Mac out of reach passed`);
  } finally {
    await browser.close();
  }
}

try {
  await run(webkit, "webkit");
  await run(chromium, "chromium");
} finally {
  api.close();
}
