// Production UI against an isolated synthetic API. Never uses live workspace data.
// A just-sent image must stay visible while the server is still producing its
// thumbnail, then switch to the server image and free the local copy.
// Run: npm run build && node tests/sent-image-preview.e2e.mjs
// (LANTOR_PREVIEW_ENGINE=webkit for Safari's engine.)
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { chromium, webkit } from "playwright";

const id = (n) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const now = new Date().toISOString();
const channelId = id(1);
let seq = 10;
const message = (body, extra = {}) => ({ id: id(++seq), seq, channel_id: channelId, thread_root_id: null,
  sender_agent_id: null, sender_name: "Test owner", sender_role: "owner", body, is_task: false,
  thread_followed: true, delivery_state: "complete", stream_key: "", task_number: null, task_status: null,
  attachments: [], artifacts: [], created_at: now, updated_at: now, ...extra });
const state = { db_url: "synthetic://sent-image-preview", web_base_url: null,
  owner_profile: { display_name: "Test owner", avatar: "T", description: "" },
  channels: [{ id: channelId, name: "preview-test", description: "", kind: "channel", dm_agent_id: null,
    unread_count: 0, github_unread_count: 0, github_review_synced_at: null }],
  messages: [message("Existing message")],
  channel_message_history: [{ channel_id: channelId, before_seq: null, has_more: false }],
  agents: [], agent_runs: [], thread_activities: [], channel_members: [], saved_messages: [],
  dismissed_inbox_items: {}, read_inbox_items: {}, tasks: [], artifacts: [], reminders: [], agent_schedules: [],
  agent_work_items: [], agent_activities: [], supervisor: { pid: null, status: "stopped", updated_at: null },
  launch_agent: { label: "", plist_path: "", installed: false, loaded: false }, ui_event_cursor: 0 };

// 2x2 PNGs: the upload and the server's (different) thumbnail.
const uploadPng = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAEElEQVR4nGO4o6EBRAwQCgAjrgSxn17XlQAAAABJRU5ErkJggg==", "base64");
const thumbnailPng = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAEElEQVR4nGPQiLoDRAwQCgAjhgV5kdVeeQAAAABJRU5ErkJggg==", "base64");

const clients = new Set(), events = [], heldThumbnails = [], sends = [];
let sendMode = { eventFirst: true };
const publish = (event) => {
  const delivery = { cursor: events.length + 1, event: JSON.stringify(event) };
  events.push(delivery);
  for (const client of clients) client.write(`id: ${delivery.cursor}\nevent: lantor\ndata: ${delivery.event}\n\n`);
};
let attachmentSeq = 100;
const api = createServer(async (req, res) => {
  const url = new URL(req.url, "http://localhost");
  if (url.pathname.startsWith("/api/attachments/")) {
    // Hold every server image until the test releases it, like a cold thumbnail.
    const ok = await new Promise((release) => heldThumbnails.push({ path: url.pathname + url.search, release }));
    if (!ok) { res.writeHead(500).end(); return; }
    res.writeHead(200, { "content-type": "image/png", "cache-control": "private, max-age=31536000, immutable" }).end(thumbnailPng);
    return;
  }
  if (!url.pathname.startsWith("/api/")) {
    const file = resolve("dist", url.pathname === "/" ? "index.html" : url.pathname.slice(1));
    if (!file.startsWith(resolve("dist") + "/")) { res.writeHead(404).end(); return; }
    try {
      const mime = { html: "text/html", js: "application/javascript", css: "text/css", woff2: "font/woff2", svg: "image/svg+xml" };
      res.writeHead(200, { "content-type": mime[file.split(".").pop()] || "application/octet-stream" }).end(await readFile(file));
    } catch { res.writeHead(404).end(); }
    return;
  }
  if (url.pathname === "/api/events") {
    res.writeHead(200, { "content-type": "text/event-stream" }); res.write(": ready\n\n");
    clients.add(res); res.on("close", () => clients.delete(res)); return;
  }
  const chunks = []; for await (const chunk of req) chunks.push(chunk);
  const raw = Buffer.concat(chunks);
  let args, files = [];
  if (req.headers["content-type"]?.startsWith("multipart/form-data")) {
    const form = await new Response(raw, { headers: { "content-type": req.headers["content-type"] } }).formData();
    args = JSON.parse(form.get("request")); files = form.getAll("attachments");
  } else args = raw.length ? JSON.parse(raw.toString()) : {};
  let result = { ok: true };
  switch (url.pathname) {
    case "/api/bootstrap": result = { ...state, ui_event_cursor: events.length }; break;
    case "/api/load_channel_previews": case "/api/load_activity_messages": result = []; break;
    case "/api/load_channel_messages": result = { messages: state.messages, next_before_seq: null, has_more: false }; break;
    case "/api/load_thread_messages": result = state.messages.filter((m) => m.id === args.threadRootId || m.thread_root_id === args.threadRootId); break;
    case "/api/load_message": result = state.messages.find((m) => m.id === args.messageId); break;
    case "/api/load_ui_state": result = Object.fromEntries(args.scopes.map((scope) => [scope, state[scope]])); break;
    case "/api/replay_ui_events": result = { cursor: events.length, replayGap: false, events: events.filter((e) => e.cursor > args.cursor) }; break;
    case "/api/send_message": {
      const row = message(args.body, { ...(args.messageId ? { id: args.messageId } : {}) });
      row.attachments = files.map((file) => ({ id: id(++attachmentSeq), message_id: row.id, original_name: file.name,
        mime_type: file.type, size_bytes: file.size, storage_path: "", created_at: now }));
      // Commit only when the test says so, after it starts watching the image.
      await new Promise((commit) => sends.push({ row, commit }));
      state.messages.push(row);
      if (sendMode.eventFirst) {
        publish({ type: "message_upsert", reason: "message", message: row });
        // The HTTP response stays in flight; the event alone settles the send.
        await new Promise(() => {});
      }
      result = row;
      break;
    }
  }
  res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(result));
});
await new Promise((done) => api.listen(0, "127.0.0.1", done));

const until = async (page, predicate, label) => {
  for (let i = 0; i < 200; i++) { if (await predicate()) return; await page.waitForTimeout(25); }
  assert.fail(`timed out waiting for ${label}`);
};

let browser;
try {
  browser = await (process.env.LANTOR_PREVIEW_ENGINE === "webkit" ? webkit : chromium).launch();
  const context = await browser.newContext({ serviceWorkers: "block", viewport: { width: 1280, height: 900 } });
  const page = await context.newPage(); page.setDefaultTimeout(5000);
  const errors = []; page.on("pageerror", (error) => errors.push(error.message));
  await page.goto(`http://127.0.0.1:${api.address().port}`, { waitUntil: "domcontentloaded" });
  await page.locator(".conversation .markdown-body").filter({ hasText: "Existing message" }).waitFor();
  await until(page, () => clients.size > 0, "event stream");

  // Records every frame in which the newest message's image is missing or not painted.
  await page.evaluate(() => {
    window.__blankFrames = [];
    window.__watchImage = (messageId) => {
      const frame = () => {
        const image = document.querySelector(`.conversation [data-message-id="${messageId}"] .message-attachment.image img`);
        if (!image || !image.complete || image.naturalWidth === 0) window.__blankFrames.push(image ? image.getAttribute("src") : "missing");
        if (window.__watching === messageId) requestAnimationFrame(frame);
      };
      window.__watching = messageId;
      requestAnimationFrame(frame);
    };
  });
  const imageState = (messageId) => page.evaluate((messageId) => {
    const image = document.querySelector(`.conversation [data-message-id="${messageId}"] .message-attachment.image img`);
    const tile = image?.closest(".message-attachment");
    return image && { src: image.getAttribute("src"), painted: image.complete && image.naturalWidth > 0, pending: tile.classList.contains("pending") };
  }, messageId);
  const blobAlive = (src) => page.evaluate((src) => fetch(src).then(() => true, () => false), src);

  async function sendImage(name, eventFirst) {
    sendMode = { eventFirst };
    const before = sends.length;
    await page.locator('.conversation input[type="file"]').setInputFiles({ name, mimeType: "image/png", buffer: uploadPng });
    await page.locator(".conversation textarea").fill(`Sending ${name}`);
    await page.getByRole("button", { name: "Send message", exact: true }).click();
    await until(page, () => sends.length === before + 1, "send request");
    const { row, commit } = sends.at(-1);
    await until(page, async () => (await imageState(row.id))?.painted === true, "local image painted");
    // Watch every frame from before the send settles.
    await page.evaluate((messageId) => window.__watchImage(messageId), row.id);
    const image = await page.locator(`.conversation [data-message-id="${row.id}"] .message-attachment.image img`).elementHandle();
    commit();
    // Settled (no longer "pending"), still showing the local copy.
    await until(page, async () => (await imageState(row.id))?.pending === false, "settled message");
    assert.equal(await image.evaluate((node) => node.isConnected), true, "the painted image element is kept, not remounted");
    return row;
  }

  for (const eventFirst of [true, false]) {
    const row = await sendImage(`shot-${eventFirst ? "event" : "http"}.png`, eventFirst);
    const settled = await imageState(row.id);
    assert.match(settled.src, /^blob:/, "settled image keeps its local copy while the server image loads");
    await until(page, () => heldThumbnails.length > 0, "server image request");
    await page.waitForTimeout(400);
    assert.match((await imageState(row.id)).src, /^blob:/, "still the local copy while the server is slow");
    const held = heldThumbnails.shift();
    assert.equal(held.path, `/api/attachments/${row.attachments[0].id}?w=480`);
    held.release(true);
    await until(page, async () => (await imageState(row.id)).src.startsWith("/api/"), "swap to server image");
    assert.equal((await imageState(row.id)).painted, true, "server image paints on swap");
    await page.waitForTimeout(100);
    const blankFrames = await page.evaluate(() => { window.__watching = null; const frames = window.__blankFrames; window.__blankFrames = []; return frames; });
    assert.deepEqual(blankFrames, [], "no frame shows a blank image");
    assert.equal(await blobAlive(settled.src), false, "local copy is freed after the swap");
    console.log(`PASS: ${eventFirst ? "event-first" : "HTTP-first"} send keeps the local image until the server image loads`);
  }

  // A failing server image still frees the local copy and shows the server result.
  const failed = await sendImage("shot-fail.png", true);
  const local = (await imageState(failed.id)).src;
  await until(page, () => heldThumbnails.length > 0, "server image request");
  heldThumbnails.shift().release(false);
  await until(page, async () => (await imageState(failed.id)).src.startsWith("/api/"), "swap after failure");
  await page.waitForTimeout(100);
  assert.equal(await blobAlive(local), false, "local copy is freed after a failed server image");
  heldThumbnails.splice(0).forEach((held) => held.release(false));
  console.log("PASS: failed server image swaps and frees the local copy");
  assert.deepEqual(errors, []);
} finally {
  heldThumbnails.splice(0).forEach((held) => held.release(false));
  await browser?.close();
  api.close();
  for (const client of clients) client.destroy();
}
