// Production App against an isolated API: Cmd+F finds text in the open thread,
// including replies that are not loaded and text in collapsed messages and
// code blocks. Run npm run build first. No live workspace data is read or mutated.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { chromium, devices, webkit } from "playwright";

const id = n => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const channelId = id(1000);
const start = Date.parse("2026-01-05T09:00:00.000Z");
let seq = 0;
const message = (body, extra = {}) => {
  seq += 1;
  const at = new Date(start + seq * 60_000).toISOString();
  return { id: id(seq), seq, channel_id: channelId, thread_root_id: null, sender_agent_id: null, sender_name: "Dylan",
    sender_role: "owner", body, is_task: false, thread_followed: false, delivery_state: "complete", stream_key: "",
    task_number: null, task_status: null, attachments: [], artifacts: [], created_at: at, updated_at: at, ...extra };
};

// Old reply 5: the match is deep in a code block that collapses (over 30 lines).
const codeBody = ["Old code:", "```rust", ...Array.from({ length: 40 }, (_, line) => line === 37
  ? "    let budget = NeedleWord::new(); // the match"
  : `    let filler_${line} = ${line};`), "```"].join("\n");
// Loaded reply 200: the match is in the last paragraph of a message that collapses (over 48 lines).
const longBody = Array.from({ length: 30 }, (_, line) => line === 29 ? "Final paragraph names needleword here." : `Paragraph ${line} of a long report.`).join("\n\n");
const bodies = new Map([
  [5, codeBody],
  [100, "An old note about NEEDLEWORD in prose."],
  [200, longBody],
  [220, "needleword twice: needleword"],
]);
const root = message("Root mentions needleword once");
const shortRoot = message("Short thread root");
const replies = [];
for (let index = 0; index < 230; index += 1) {
  replies.push(message(bodies.get(index) ?? `Reply ${index}`, { thread_root_id: root.id, sender_role: index % 2 ? "agent" : "owner", sender_name: index % 2 ? "Agent" : "Dylan" }));
}
const shortReplies = [message("needleword in a short thread", { thread_root_id: shortRoot.id }), message("no match", { thread_root_id: shortRoot.id })];
const all = [root, shortRoot, ...replies, ...shortReplies];
const reply = index => replies[index];
const activity = (threadRoot, rows) => ({ thread_root_id: threadRoot.id, channel_id: channelId, reply_count: rows.length, unread_count: 0,
  agent_unread_count: 0, first_unread_agent_message_id: null, latest_message_id: rows.at(-1).id, latest_activity_at: rows.at(-1).created_at });
const state = { db_url: "synthetic://thread-find", web_base_url: null, owner_profile: { display_name: "Dylan", avatar: "D", description: "" },
  channels: [{ id: channelId, name: "finding", kind: "channel", dm_agent_id: null, description: "", unread_count: 0, github_unread_count: 0, github_review_synced_at: null }],
  messages: [root, shortRoot, ...replies.slice(-2), ...shortReplies],
  channel_message_history: [{ channel_id: channelId, before_seq: null, has_more: false }],
  agents: [], tasks: [], channel_members: [], thread_activities: [activity(root, replies), activity(shortRoot, shortReplies)],
  saved_messages: [], dismissed_inbox_items: {}, read_inbox_items: {}, artifacts: [], reminders: [], agent_runs: [],
  agent_work_items: [], agent_activities: [], supervisor: { pid: null, status: "stopped", updated_at: null },
  launch_agent: { label: "", plist_path: "", installed: false, loaded: false }, ui_event_cursor: 0 };

// The same rules as message_store::load_thread_page.
function threadPage({ threadRootId, beforeSeq, sinceSeq, limit = 50 }) {
  const below = all.filter(m => m.thread_root_id === threadRootId && m.seq < (beforeSeq ?? Infinity));
  const newest = below.slice(-Math.min(Math.max(limit, 1), 200));
  const from = newest.length ? Math.min(newest[0].seq, sinceSeq ?? Infinity) : Infinity;
  const page = below.filter(m => m.seq >= from);
  const oldest = page[0]?.seq ?? null;
  const hasMore = oldest !== null && all.some(m => m.thread_root_id === threadRootId && m.seq < oldest);
  return { messages: [all.find(m => m.id === threadRootId), ...page], next_before_seq: hasMore ? oldest : null, has_more: hasMore };
}

// The same rules as message_store::search::find_in_thread (ASCII is enough here).
function findInThread({ threadRootId, query, beforeSeq }) {
  const fold = text => text.toLowerCase().replace(/\s/g, " ");
  const needle = fold(query);
  if (!needle.trim()) return [];
  return all.filter(m => m.thread_root_id === threadRootId && m.seq < (beforeSeq ?? Infinity))
    .map(m => ({ message_id: m.id, seq: m.seq, matches: fold(m.body).split(needle).length - 1 }))
    .filter(hit => hit.matches > 0);
}

const dist = resolve("dist");
let requests = [];
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
  requests.push({ path: url.pathname, args });
  let result = { ok: true };
  switch (url.pathname) {
    case "/api/bootstrap": result = state; break;
    case "/api/load_channel_previews": case "/api/load_activity_messages": case "/api/search_messages": result = []; break;
    case "/api/load_channel_messages": result = { messages: state.messages.filter(m => !m.thread_root_id), next_before_seq: null, has_more: false, thread_activities: state.thread_activities }; break;
    case "/api/load_thread_page": result = threadPage(args); break;
    case "/api/find_in_thread": result = findInThread(args); break;
    case "/api/load_thread_messages": result = all.filter(m => m.id === args.threadRootId || m.thread_root_id === args.threadRootId); break;
    case "/api/load_message": result = all.find(m => m.id === args.messageId); break;
    case "/api/load_ui_state": result = Object.fromEntries(args.scopes.map(scope => [scope, state[scope]])); break;
    case "/api/replay_ui_events": result = { cursor: 0, replayGap: false, events: [] }; break;
  }
  res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(result));
});
await new Promise(done => api.listen(0, "127.0.0.1", done));
const origin = `http://127.0.0.1:${api.address().port}`;
const findRequests = () => requests.filter(r => r.path === "/api/find_in_thread").map(r => r.args);

async function run(engine, name, contextOptions) {
  const browser = await engine.launch();
  try {
    const context = await browser.newContext({ serviceWorkers: "block", ...contextOptions });
    const page = await context.newPage(); page.setDefaultTimeout(8000);
    const errors = []; page.on("pageerror", error => errors.push(error.message));
    const mobile = Boolean(contextOptions.isMobile);
    const bar = page.locator(".thread .thread-find-bar");
    const input = bar.getByRole("textbox", { name: "Find in thread" });
    const count = bar.locator(".thread-find-count");
    const current = () => page.evaluate(() => [...document.querySelectorAll(".thread article[data-find-current]")].map(row => row.dataset.messageId));
    // The current match is painted inside the thread's view, below the bar.
    const currentInView = () => page.evaluate(() => {
      const range = [...CSS.highlights.get("lantor-thread-find-current") ?? []][0];
      if (!range) return "no current match";
      const match = range.getBoundingClientRect();
      const bar = document.querySelector(".thread-find-bar").getBoundingClientRect();
      const view = document.querySelector(".thread .thread-scroll").getBoundingClientRect();
      if (match.height === 0) return "match has no box";
      return match.top >= bar.bottom && match.bottom <= view.bottom ? "in view" : `out of view: ${match.top}..${match.bottom} in ${bar.bottom}..${view.bottom}`;
    });
    const openThread = async threadRoot => {
      const row = page.locator(`.conversation [data-message-id="${threadRoot.id}"]`);
      if (mobile) await row.locator(".markdown-body").first().click();
      else { await row.hover(); await row.getByRole("button", { name: "View replies", exact: true }).click(); }
      await page.locator(`.thread [data-message-id="${threadRoot.id}"]`).waitFor();
    };
    const step = async (key, expected, messageId) => {
      await input.press(key);
      await count.filter({ hasText: new RegExp(`^${expected}$`) }).waitFor();
      await page.waitForFunction(target => document.querySelector(`.thread article[data-find-current][data-message-id="${target}"]`), messageId);
      await page.waitForFunction(() => {
        const range = [...CSS.highlights.get("lantor-thread-find-current") ?? []][0];
        const bar = document.querySelector(".thread-find-bar")?.getBoundingClientRect();
        const view = document.querySelector(".thread .thread-scroll")?.getBoundingClientRect();
        const match = range?.getBoundingClientRect();
        return match && bar && view && match.height > 0 && match.top >= bar.bottom && match.bottom <= view.bottom;
      }).catch(() => {});
      assert.equal(await currentInView(), "in view", `${name}: ${expected} is scrolled into view`);
      assert.deepEqual(await current(), [messageId], `${name}: ${expected} is on the expected message`);
    };

    requests = [];
    await page.goto(origin, { waitUntil: "domcontentloaded" });
    if (mobile) await page.locator(".sidebar .channel").filter({ hasText: "finding" }).first().click();
    await page.locator(`.conversation [data-message-id="${root.id}"]`).waitFor();
    await openThread(root);
    await page.waitForFunction(() => document.querySelectorAll(".thread .reply-list article[data-message-id]").length === 50);

    // Cmd+F opens the bar (the options menu on a phone) and focuses it.
    if (mobile) {
      await page.locator(".thread").getByRole("button", { name: "Thread options" }).click();
      await page.getByRole("menuitem", { name: "Find in thread" }).click();
    } else {
      await page.keyboard.press("ControlOrMeta+f");
    }
    await input.waitFor();
    await page.waitForFunction(() => document.activeElement?.closest(".thread-find-bar"));

    // Counts loaded matches on the page and earlier ones from the server.
    await input.fill("needleword");
    await count.filter({ hasText: /^\d+\/6$/ }).waitFor();
    assert.deepEqual(findRequests(), [{ threadRootId: root.id, query: "needleword", beforeSeq: reply(180).seq }],
      `${name}: only replies older than the loaded run are searched on the server`);
    assert.equal(await page.evaluate(() => CSS.highlights.get("lantor-thread-find")?.size), 4, `${name}: loaded matches are highlighted`);

    // Walk to the first match, then through every match in thread order.
    const position = Number((await count.textContent()).split("/")[0]);
    for (let press = position; press < 6; press += 1) await input.press("Enter");
    await step("Enter", "1/6", root.id);
    // An unloaded reply loads, and its collapsed code block opens at the match.
    await step("Enter", "2/6", reply(5).id);
    assert.equal(await page.locator(`.thread [data-message-id="${reply(5).id}"] .code-block-shell`).evaluate(node => node.classList.contains("collapsed")), false,
      `${name}: the code block holding the match expands`);
    assert.equal(await page.locator(`.thread [data-message-id="${reply(5).id}"]`).evaluate(node => node.classList.contains("tap-focused")), false,
      `${name}: expanding for find does not select the reply`);
    await step("Enter", "3/6", reply(100).id);
    // A collapsed long message expands at its match.
    await step("Enter", "4/6", reply(200).id);
    assert.equal(await page.locator(`.thread [data-message-id="${reply(200).id}"] .message-long-preview`).evaluate(node => node.classList.contains("collapsed")), false,
      `${name}: the long message holding the match expands`);
    await step("Enter", "5/6", reply(220).id);
    await step("Enter", "6/6", reply(220).id);
    await step("Enter", "1/6", root.id);
    await step("Shift+Enter", "6/6", reply(220).id);
    assert.equal(await page.evaluate(() => CSS.highlights.get("lantor-thread-find")?.size), 6, `${name}: every match is highlighted once loaded`);


    // Case does not matter; a query with no match says so.
    await input.fill("NEEDLEWORD twice");
    await count.filter({ hasText: /^1\/1$/ }).waitFor();
    await input.fill("absent-term");
    await count.filter({ hasText: "No results" }).waitFor();
    await input.fill("needleword");
    await count.filter({ hasText: /^\d+\/6$/ }).waitFor();

    // The current match is painted; closing the bar repaints it plain.
    await page.waitForFunction(() => {
      const range = [...CSS.highlights.get("lantor-thread-find-current") ?? []][0];
      const view = document.querySelector(".thread .thread-scroll")?.getBoundingClientRect();
      const match = range?.getBoundingClientRect();
      return match && view && match.height > 0 && match.top >= view.top + 56 && match.bottom <= view.bottom;
    });
    const clip = await page.evaluate(() => {
      const box = [...CSS.highlights.get("lantor-thread-find-current")][0].getBoundingClientRect();
      return { x: box.x, y: box.y, width: Math.max(box.width, 1), height: Math.max(box.height, 1) };
    });
    const painted = await page.screenshot({ clip });
    // Escape closes the bar, not the thread, and clears the highlights.
    await input.press("Escape");
    await bar.waitFor({ state: "detached" });
    await page.locator(`.thread [data-message-id="${root.id}"]`).waitFor();
    assert.equal(await page.evaluate(() => CSS.highlights.size), 0, `${name}: closing clears the highlights`);
    await page.evaluate(() => new Promise(done => requestAnimationFrame(() => requestAnimationFrame(done))));
    assert.equal((await page.screenshot({ clip })).equals(painted), false, `${name}: the match was painted and closing repaints it`);
    assert.deepEqual(await current(), [], `${name}: no row is kept rendered for find`);

    if (!mobile) {
      // Cmd+F again brings the last query back, selected.
      await page.keyboard.press("ControlOrMeta+f");
      await input.waitFor();
      assert.equal(await input.inputValue(), "needleword");
      await count.filter({ hasText: /^\d+\/6$/ }).waitFor();
      assert.equal(await input.evaluate(node => node.selectionStart === 0 && node.selectionEnd === node.value.length), true);
      // The header button closes it.
      await page.locator(".thread").getByRole("button", { name: "Find in thread", exact: true }).click();
      await bar.waitFor({ state: "detached" });

      // A fully loaded thread never asks the server.
      await page.locator(".thread").getByRole("button", { name: "Close thread", exact: true }).click();
      await openThread(shortRoot);
      requests = [];
      await page.keyboard.press("ControlOrMeta+f");
      await input.fill("needleword");
      await count.filter({ hasText: "1/1" }).waitFor();
      // A server search would start after its 180 ms debounce.
      await page.waitForTimeout(400);
      assert.deepEqual(findRequests(), [], `${name}: a loaded thread is searched on the page only`);
    }
    assert.deepEqual(errors, []);
    console.log(`ok - ${name}`);
  } finally {
    await browser.close();
  }
}

try {
  await run(chromium, "chromium desktop", { viewport: { width: 1280, height: 820 } });
  await run(webkit, "webkit desktop", { viewport: { width: 1280, height: 820 } });
  await run(webkit, "webkit iphone", { ...devices["iPhone 13"] });
} finally {
  api.close();
}
