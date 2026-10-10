// Regenerates the README screenshots from a synthetic demo workspace.
//
//   npm run build && npm run screenshots:readme
//
// The built app in dist/ runs against a fake in-process API, so no live
// workspace, database or agent runtime is read or touched. The clock and time
// zone are pinned, so reruns differ only where the UI itself changed.
import { createServer } from "node:http";
import { readFile, mkdir } from "node:fs/promises";
import { resolve, join } from "node:path";
import { tmpdir } from "node:os";
import { chromium } from "playwright";

const dist = resolve(process.argv[2] ?? "dist");
const out = resolve(process.argv[3] ?? "docs/assets");
const only = process.argv[4] ? new Set(process.argv[4].split(",")) : null;
await mkdir(out, { recursive: true });

// Thursday morning in the demo; every timestamp is relative to it.
const NOW = new Date("2026-10-08T17:40:00Z");
const TIME_ZONE = "America/Los_Angeles";
const at = minutesAgo => new Date(NOW.getTime() - minutesAgo * 60_000).toISOString();
const id = n => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;

// ---------------------------------------------------------------- agents
const baseAgent = {
  role: "agent", status: "idle", model: "", reasoning_effort: "high", service_tier: "", launch_command: "",
  environment_variables: "", working_directory: "", workspace_exists: true, workspace_memory_path: "",
  workspace_memory_exists: true, workspace_entries: [], details_loaded: true, daily_budget_micros: 0, subscription_status: null,
};
const usage = (provider, plan, fiveHour, weekly) => ({
  provider, plan, status: "available", observed_at: at(3),
  windows: [
    { id: "primary", label: "5h", used_percent: fiveHour, resets_at: Math.floor(NOW.getTime() / 1000) + 2 * 3600 + 20 * 60 },
    { id: "secondary", label: "Weekly", used_percent: weekly, resets_at: Math.floor(NOW.getTime() / 1000) + 3 * 86400 },
  ],
});
const hancock = { ...baseAgent, id: id(800), handle: "Hancock", display_name: "Hancock", runtime: "codex", model: "gpt-6.1-sol",
  avatar: "dicebear:dylan:Hancock", description: "Frontend, iPhone layout and design polish", status: "running",
  subscription_status: usage("codex", "Pro", 34, 58) };
const speed = { ...baseAgent, id: id(801), handle: "Speed", display_name: "Speed", runtime: "codex", model: "gpt-6.1-sol",
  avatar: "dicebear:dylan:Speed-7", description: "Sync engine, storage and performance", subscription_status: usage("codex", "Pro", 12, 41) };
const vegapunk = { ...baseAgent, id: id(802), handle: "Vegapunk", display_name: "Vegapunk", runtime: "claude", model: "opus",
  avatar: "dicebear:dylan:Vegapunk", description: "Research, profiling and code review", subscription_status: usage("claude", "Max", 22, 37) };
const theo = { ...baseAgent, id: id(803), handle: "Theo", display_name: "Theo", runtime: "claude", model: "opus",
  avatar: "dicebear:dylan:Theo-3", description: "Release notes, docs and triage", subscription_status: usage("claude", "Max", 5, 37) };
const agents = [hancock, speed, vegapunk, theo];

// ---------------------------------------------------------------- channels
const devId = id(1000), designId = id(1001), releaseId = id(1002), allId = id(1003);
const channel = (cid, name, description, extra = {}) => ({
  id: cid, name, description, kind: "channel", dm_agent_id: null, unread_count: 0, github_unread_count: 0,
  github_review_synced_at: null, latest_message_at: at(2), ...extra,
});
const dmChannels = agents.map((agent, index) => ({
  ...channel(id(1100 + index), `dm:${agent.id}`, ""), kind: "dm", dm_agent_id: agent.id,
  unread_count: agent === theo ? 1 : 0,
}));
const channels = [
  channel(allId, "all", "Announcements"),
  channel(devId, "pebble-dev", "Building Pebble, the self-hosted notes app", { github_unread_count: 2, github_review_synced_at: at(6) }),
  channel(designId, "design", "Editor and mobile design", { unread_count: 3 }),
  channel(releaseId, "release", "Release 2.4 checklist", { unread_count: 1 }),
  channel(id(1004), "ios", "The iPhone app"),
  channel(id(1005), "research", "Ideas and experiments"),
  ...dmChannels,
];

// ---------------------------------------------------------------- messages
let seq = 0;
const msg = (n, minutesAgo, extra = {}) => ({
  id: id(n), seq: ++seq, channel_id: devId, thread_root_id: null, sender_agent_id: null, sender_name: "Dylan",
  sender_role: "owner", body: "", is_task: false, thread_followed: false, delivery_state: "complete", stream_key: "",
  task_number: null, task_status: null, attachments: [], artifacts: [], created_at: at(minutesAgo), updated_at: at(minutesAgo), ...extra,
});
const by = agent => ({ sender_agent_id: agent.id, sender_name: agent.display_name, sender_role: "agent" });

const morning = msg(12, 162, { ...by(theo), body:
  "Overnight: 3 pull requests merged, 6 new issues triaged. Two need a look from you: pebble-notes/pebble#320 and #322." });
const syncAsk = msg(1, 148, {
  body: "@Vegapunk Opening a notebook with 5,000 notes takes about 40 seconds to sync on my phone. Find out why and tell me how we should fix it.",
  thread_followed: true,
});
const syncFindings = msg(2, 131, { ...by(vegapunk), thread_root_id: syncAsk.id, body: [
  "Found it. Every note is saved in its own SQLite transaction, so a 5,000-note sync waits for 5,000 disk flushes.",
  "",
  "| Step | Time | Share |",
  "| --- | ---: | ---: |",
  "| `commit` (one per note) | 36.8 s | 92% |",
  "| Download changes | 2.1 s | 5% |",
  "| Update the search index | 1.1 s | 3% |",
  "",
  "Profile and test notebook: `bench/sync-5k.md`. There are two ways to fix it, so I asked below.",
].join("\n") });
const syncDecisionMsg = msg(3, 130, { ...by(vegapunk), thread_root_id: syncAsk.id,
  body: "**Decision needed:** Which sync fix should we ship first?" });
const syncTask = msg(4, 96, { ...by(vegapunk), is_task: true, task_number: 42, task_status: "in_progress",
  body: "Save each sync in one transaction instead of one per note. Target: a 5,000-note notebook syncs in under 3 seconds on iPhone." });
const syncTaskReply = msg(5, 31, { ...by(speed), thread_root_id: syncTask.id,
  body: "Writes are batched and the 5k benchmark is down from 40.0 s to 2.3 s. Running the full sync suite before I open the PR." });
const darkMode = msg(6, 74, { ...by(hancock), is_task: true, task_number: 39, task_status: "in_review",
  body: "Dark mode for the editor is ready for review in pebble-notes/pebble#318. Code blocks, tables and the slash menu all follow the theme now." });
const darkModeReply = msg(7, 70, { thread_root_id: darkMode.id, body: "Looks great. @Vegapunk can you review the PR?" });
const toolbarAsk = msg(8, 9, {
  body: "@Hancock The editor toolbar buttons are hard to hit on iPhone. Can you make them at least 44 pt and keep the toolbar on one line?",
});
const designMsg = msg(9, 40, { channel_id: designId, ...by(hancock), body: "Here are three toolbar layouts for small screens." });
const releaseMsg = msg(10, 25, { channel_id: releaseId, ...by(theo), body: "Draft release notes for 2.4 are ready." });
const theoDm = msg(11, 15, { channel_id: dmChannels[3].id, ...by(theo), body: "Morning! I triaged the 6 new issues; two look like duplicates of #301." });
const messages = [morning, syncAsk, syncFindings, syncDecisionMsg, syncTask, syncTaskReply, darkMode, darkModeReply, toolbarAsk,
  designMsg, releaseMsg, theoDm];

const threadActivities = [
  [syncAsk, syncDecisionMsg, 2], [syncTask, syncTaskReply, 1], [darkMode, darkModeReply, 1],
].map(([root, latest, count]) => ({ thread_root_id: root.id, channel_id: devId, unread_count: 0, reply_count: count,
  latest_message_id: latest.id, latest_activity_at: latest.created_at }));

// ---------------------------------------------------------------- tasks and decisions
const task = (n, number, title, status, assignee, minutesAgo, messageId, channelId = devId, channelName = "pebble-dev") => ({
  id: id(n), number, message_id: messageId, channel_id: channelId, title, status, version: 1, channel_name: channelName,
  assignee_id: assignee?.id ?? null, assignee_name: assignee?.display_name ?? null, created_at: at(minutesAgo), updated_at: at(minutesAgo),
});
const tasks = [
  task(900, 42, "Save each sync in one transaction", "in_progress", speed, 31, syncTask.id),
  task(901, 39, "Dark mode for the editor", "in_review", hancock, 74, darkMode.id),
  task(902, 37, "Draft the 2.4 release notes", "in_review", theo, 25, releaseMsg.id, releaseId, "release"),
  task(903, 43, "Show sync progress in the status bar", "todo", null, 20, id(60)),
  task(904, 35, "Fix the blurry share-sheet icon", "done", hancock, 600, id(61)),
];
const decision = (n, message, requester, title, context, options, minutesAgo, extra = {}) => ({
  id: id(n), message_id: message.id, channel_id: message.channel_id, channel_name: channels.find(c => c.id === message.channel_id).name,
  thread_root_id: message.thread_root_id, requester_agent_id: requester.id, requester_handle: requester.handle, task_id: null,
  task_number: null, title, context, options, status: "open", answer_option_id: null, answer_note: "", answer_message_id: null,
  resolved_at: null, created_at: at(minutesAgo), updated_at: at(minutesAgo), ...extra,
});
const decisions = [
  decision(700, syncDecisionMsg, vegapunk, "Which sync fix should we ship first?",
    "Both fix the 40-second sync. They differ in how soon we can ship and how much changes.", [
      { id: "a", label: "Save each sync in one transaction", detail: "Sync drops to about 2 seconds. A small change in the storage layer that can ship this week.", recommended: true },
      { id: "b", label: "Move sync to a background worker", detail: "The editor never waits for sync, but it takes about a week and needs a database migration.", recommended: false },
    ], 130),
  decision(701, releaseMsg, theo, "Publish the 2.4 release notes today?",
    "The notes cover dark mode and faster sync. Sync is still in progress.", [
      { id: "a", label: "Publish today and add sync when it lands", detail: "Users hear about dark mode now; the notes change once more.", recommended: true },
      { id: "b", label: "Wait for the sync fix", detail: "One complete announcement, about two days later.", recommended: false },
    ], 22),
];

// ---------------------------------------------------------------- live run (progress dock)
const run = { id: id(600), agent_id: hancock.id, agent_handle: hancock.handle, work_item_id: id(610), command: "codex", working_directory: "",
  status: "running", pid: 4242, exit_code: null, log: "", input_tokens: 0, output_tokens: 0, cost_micros: 0, started_at: at(8.5), stopped_at: null };
const workItem = { id: id(610), agent_id: hancock.id, agent_handle: hancock.handle, channel_id: devId, channel_name: "pebble-dev",
  thread_root_id: null, source_message_id: toolbarAsk.id, inbox_item_id: null, task_id: null, task_number: null, source_kind: "mention",
  title: "Make the editor toolbar buttons 44 pt", context: "", status: "running", run_id: run.id,
  created_at: at(9), updated_at: at(0.3), completed_at: null };
const activity = (n, minutesAgo, kind, title, detail, commands, edits, status = "success") => ({
  id: id(n), agent_id: hancock.id, agent_handle: hancock.handle, run_id: run.id, kind, phase: kind, status,
  title, summary: "", detail, created_at: at(minutesAgo),
  metadata: commands === undefined ? {} : { run_command_count: commands, run_file_edit_count: edits },
});
const agentActivities = [
  activity(620, 8.4, "thinking", "Reading the toolbar layout", "Checking how the toolbar wraps below 400 px.", 2, 0),
  activity(621, 5.2, "file_edit", "Resizing the toolbar buttons", "Buttons now use a 44 pt hit area; icons stay 20 pt.", 6, 2),
  activity(622, 0.4, "command", "Running the iPhone layout tests", "Checking the toolbar at 320, 375 and 430 px wide.", 11, 3, "active"),
];

// ---------------------------------------------------------------- GitHub tab
const pr = (number, title, author, minutesAgo, checks, extra = {}) => ({
  number, title, url: `https://github.com/pebble-notes/pebble/pull/${number}`, author_login: author, is_draft: false, state: "OPEN",
  updated_at: at(minutesAgo), head_sha: `${number}a7c3e9f0b1d2`, checks, is_review_requested: true, is_authored: false,
  linked_thread_root_id: null, linked_task_id: null, linked_task_number: null, linked_task_status: null, linked_assignee_id: null,
  linked_assignee_name: null, review_anchor_sha: null, review_is_stale: false, review_commits_ahead: null, ...extra,
});
const passing = total => ({ status: "success", total, pending: 0, failed: 0, failing_checks: [] });
const github = {
  account: { login: "dylan", host: "github.com" },
  binding: { channel_id: devId, repository_id: "R_pebble", name_with_owner: "pebble-notes/pebble", url: "https://github.com/pebble-notes/pebble",
    local_path: "~/code/pebble", account_login: "dylan", review_login: "dylan", review_queue_synced_at: at(6), issue_queue_synced_at: at(6),
    created_at: at(60 * 24 * 30), updated_at: at(6) },
  review_requests: [
    pr(318, "Editor: dark mode for code blocks, tables and the slash menu", "hancock-bot", 70, passing(12), {
      linked_thread_root_id: darkMode.id, linked_task_id: id(905), linked_task_number: 44, linked_task_status: "in_progress",
      linked_assignee_id: vegapunk.id, linked_assignee_name: vegapunk.display_name }),
    pr(321, "Sync: batch note writes into one transaction", "speed-bot", 12, { status: "pending", total: 12, pending: 4, failed: 0, failing_checks: [] }),
    pr(316, "Search: rank exact title matches first", "mia-chen", 260, { status: "failure", total: 12, pending: 0, failed: 1, failing_checks: ["e2e (webkit)"] }, {
      linked_thread_root_id: id(62), linked_task_id: id(906), linked_task_number: 36, linked_task_status: "done",
      linked_assignee_id: vegapunk.id, linked_assignee_name: vegapunk.display_name, review_anchor_sha: "316prev", review_is_stale: true, review_commits_ahead: 3 }),
  ],
  issues: [
    { number: 322, title: "Pasting a table from Google Sheets loses the header row", labels: [{ name: "bug", color: "d73a4a" }, { name: "editor", color: "1d76db" }] },
    { number: 320, title: "Offline edits made on two devices both say 'Saved'", labels: [{ name: "bug", color: "d73a4a" }, { name: "sync", color: "0e8a16" }] },
    { number: 317, title: "Export a notebook as a single Markdown file", labels: [{ name: "enhancement", color: "a2eeef" }] },
  ].map((issue, index) => ({ url: `https://github.com/pebble-notes/pebble/issues/${issue.number}`, author_login: ["sam-k", "lea-m", "jo-p"][index],
    assignee_logins: [], state: "OPEN", created_at: at(60 * (5 + index * 7)), updated_at: at(60 * (2 + index * 3)), comments_count: [4, 2, 9][index],
    is_related: index === 1, linked_thread_root_id: null, linked_task_id: null, linked_task_number: null, linked_task_status: null,
    linked_assignee_id: null, linked_assignee_name: null, ...issue })),
};

// ---------------------------------------------------------------- channel wiki
const wikiContent = `# pebble-dev

Read this before you start work in this channel.

## Project
- Repo: \`pebble-notes/pebble\`, checked out at \`~/code/pebble\`.
- Rust sync server in \`server/\`, React editor in \`web/\`, iPhone shell in \`ios/\`.

## How we work
- One task per pull request. Link the PR in the task thread.
- Run \`just check\` and the iPhone layout tests before asking for review.
- Ask with a decision card when there is more than one reasonable way forward.

## Decisions
- 2026-10-07: Ship sync in one transaction first; the background worker can wait (thread with @Vegapunk).
- 2026-10-02: Minimum touch target is 44 pt on every screen.
- 2026-09-28: Notes stay plain Markdown files on disk. No proprietary format.
`;
const wikiRevision = (n, shortId, minutesAgo, author, note, content, parent) => ({ id: id(n), short_id: shortId,
  parent_short_id: parent, content, author, note, created_at: at(minutesAgo) });
const wiki = { head: wikiRevision(500, "7c41e9a2", 95, "@Vegapunk", "Record the sync decision", wikiContent, "2b8d0f63"), max_bytes: 65536,
  revisions: [] };
wiki.revisions = [wiki.head, wikiRevision(499, "2b8d0f63", 60 * 26, "Dylan", "Add the touch target rule", wikiContent, "e05a7c19"),
  wikiRevision(498, "e05a7c19", 60 * 24 * 9, "@Theo", "First version", wikiContent, null)];

// ---------------------------------------------------------------- fake API
const state = {
  db_url: "synthetic://readme", web_base_url: null,
  owner_profile: { display_name: "Dylan", avatar: "dicebear:dylan:Dylan", description: "Building Pebble" },
  channels, messages, channel_message_history: channels.map(c => ({ channel_id: c.id, before_seq: null, has_more: false })),
  agents, tasks, decisions,
  channel_members: [hancock, speed, vegapunk, theo].map(agent => ({ channel_id: devId, agent_id: agent.id, agent_handle: agent.handle,
    agent_display_name: agent.display_name, created_at: at(60 * 24 * 30) })),
  thread_activities: threadActivities, saved_messages: [], dismissed_inbox_items: {}, read_inbox_items: {}, artifacts: [], reminders: [],
  agent_schedules: [], agent_runs: [run], agent_work_items: [workItem], agent_activities: agentActivities,
  supervisor: { pid: 4100, status: "running", updated_at: at(1) },
  launch_agent: { label: "", plist_path: "", installed: false, loaded: false }, ui_event_cursor: 0,
};
const threadMessages = rootId => messages.filter(m => m.id === rootId || m.thread_root_id === rootId);
const unknownCalls = new Set();
const mime = { html: "text/html", js: "application/javascript", css: "text/css", png: "image/png", svg: "image/svg+xml",
  woff: "font/woff", woff2: "font/woff2", webmanifest: "application/manifest+json", json: "application/json" };
const server = createServer(async (req, res) => {
  const url = new URL(req.url, "http://localhost");
  if (!url.pathname.startsWith("/api/")) {
    const file = resolve(dist, url.pathname === "/" ? "index.html" : url.pathname.slice(1));
    if (!file.startsWith(dist + "/")) { res.writeHead(404).end(); return; }
    try { res.writeHead(200, { "content-type": mime[file.split(".").pop()] || "application/octet-stream" }).end(await readFile(file)); }
    catch { res.writeHead(404).end(); }
    return;
  }
  if (url.pathname === "/api/events") { res.writeHead(200, { "content-type": "text/event-stream" }); res.write(": ready\n\n"); return; }
  let raw = ""; for await (const chunk of req) raw += chunk;
  const args = raw ? JSON.parse(raw) : {};
  let result = { ok: true };
  switch (url.pathname) {
    case "/api/bootstrap": result = state; break;
    case "/api/load_channel_previews": case "/api/load_activity_messages": case "/api/search_messages":
    case "/api/search_channel_wikis": case "/api/agent_workspace_list": result = []; break;
    case "/api/load_channel_messages":
      result = { messages: messages.filter(m => m.channel_id === args.channelId), thread_activities: threadActivities,
        next_before_seq: null, has_more: false };
      break;
    case "/api/load_thread_messages": result = threadMessages(args.threadRootId); break;
    case "/api/load_ui_state": result = Object.fromEntries(args.scopes.map(scope => [scope, state[scope] ?? []])); break;
    case "/api/replay_ui_events": result = { cursor: 0, replayGap: false, events: [] }; break;
    case "/api/load_agent_detail": result = { agent: agents.find(a => a.id === args.agentId) ?? hancock, agent_activities: agentActivities, agent_work_items: [workItem] }; break;
    case "/api/load_channel_wiki": result = wiki; break;
    case "/api/load_github_review_queue": case "/api/refresh_github_review_queue": case "/api/refresh_github_issue_queue":
      result = github;
      break;
    case "/api/mark_inbox_items_read": case "/api/mark_github_review_attention_read": break;
    case "/api/load_github_review_comparisons": result = { repository_id: "R_pebble", comparisons: [] }; break;
    default: unknownCalls.add(url.pathname);
  }
  res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(result));
});
await new Promise(done => server.listen(0, "127.0.0.1", done));
const base = `http://127.0.0.1:${server.address().port}`;

// ---------------------------------------------------------------- shots
const DESKTOP = { viewport: { width: 1512, height: 945 }, deviceScaleFactor: 2 };
const TAB = { viewport: { width: 1180, height: 750 }, deviceScaleFactor: 2 };
const IPHONE = { viewport: { width: 393, height: 793 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true };
const browser = await chromium.launch();
const failures = [];
async function shot(name, device, fn) {
  if (only && !only.has(name)) return;
  const context = await browser.newContext({ ...device, serviceWorkers: "block", timezoneId: TIME_ZONE, locale: "en-US", colorScheme: "light" });
  const page = await context.newPage();
  page.setDefaultTimeout(8000);
  await page.clock.setFixedTime(NOW);
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  try {
    await page.goto(base, { waitUntil: "domcontentloaded" });
    await page.locator(device.isMobile ? ".sidebar" : ".conversation").first().waitFor();
    await page.waitForTimeout(400);
    const target = await fn(page);
    await page.waitForTimeout(400);
    const options = { path: join(out, `${name}.png`), animations: "disabled", caret: "hide" };
    if (target && typeof target.screenshot === "function") await target.screenshot(options);
    else await page.screenshot({ ...options, ...target });
    if (errors.length) throw new Error(errors.join("; "));
    console.log("ok", name);
  } catch (error) {
    failures.push(name);
    const debugPath = join(tmpdir(), `lantor-readme-${name}-failed.png`);
    await page.screenshot({ path: debugPath }).catch(() => {});
    console.log("FAIL", name, error.message.split("\n")[0], `(page: ${debugPath})`);
  } finally {
    await context.close();
  }
}
const openChannel = async (page, name) => {
  await page.locator(".sidebar .channel-block").getByText(name, { exact: true }).first().click();
  await page.locator(".conversation .message-row, .conversation [data-message-id]").first().waitFor();
  await page.waitForTimeout(400);
};
const openThread = async (page, root) => {
  const row = page.locator(`.conversation [data-message-id="${root.id}"]`);
  await row.hover();
  await row.getByRole("button", { name: "View thread replies", exact: true }).click();
  await page.locator(".thread").waitFor();
  await page.waitForTimeout(400);
};

const settle = async page => {
  await page.evaluate(() => document.activeElement instanceof HTMLElement && document.activeElement.blur());
  await page.mouse.move(2, 940);
};
const closeThread = async page => {
  const close = page.locator(".thread-close");
  if (await close.count()) { await close.click(); await page.waitForTimeout(300); }
};
// Screenshot the box around the given elements, padded but kept inside the page.
// Everything else is hidden so a floating element is not cropped with stray text.
const around = async (page, locators, pad = 16) => {
  for (const locator of locators) await locator.evaluate(element => element.setAttribute("data-readme-focus", ""));
  await page.addStyleTag({ content: "body * { visibility: hidden !important; } [data-readme-focus], [data-readme-focus] * { visibility: visible !important; }" });
  const boxes = await Promise.all(locators.map(locator => locator.boundingBox()));
  const viewport = page.viewportSize();
  const x = Math.max(0, Math.min(...boxes.map(b => b.x)) - pad);
  const y = Math.max(0, Math.min(...boxes.map(b => b.y)) - pad);
  const right = Math.min(viewport.width, Math.max(...boxes.map(b => b.x + b.width)) + pad);
  const bottom = Math.min(viewport.height, Math.max(...boxes.map(b => b.y + b.height)) + pad);
  return { clip: { x, y, width: right - x, height: bottom - y } };
};
const channelTab = async (page, label) => {
  await openChannel(page, "pebble-dev");
  await closeThread(page);
  await page.locator(".tabs button").filter({ hasText: label }).click();
  await page.waitForTimeout(700);
  await settle(page);
  return page.locator(".conversation");
};

// Desktop: a channel with a running agent, tasks and a decision waiting in a thread.
await shot("lantor-workspace", DESKTOP, async page => {
  await openChannel(page, "pebble-dev");
  // A first visit on desktop opens the channel's first thread, which is this one.
  if (!(await page.locator(".thread").count())) await openThread(page, syncAsk);
  await settle(page);
});
// Desktop crops for the feature tour.
await shot("lantor-github", TAB, page => channelTab(page, "GitHub"));
await shot("lantor-wiki", TAB, page => channelTab(page, "Wiki"));
await shot("lantor-progress", DESKTOP, async page => {
  await openChannel(page, "pebble-dev");
  await closeThread(page);
  const dock = page.locator(".conversation .activity-progress-dock");
  await dock.locator(".activity-progress-toggle").click();
  await page.waitForTimeout(400);
  await settle(page);
  return around(page, [dock, dock.locator(".activity-progress-history")], 4);
});
await shot("lantor-agent-usage", DESKTOP, async page => {
  await openChannel(page, "pebble-dev");
  await closeThread(page);
  const avatar = page.locator(`.conversation [data-message-id="${darkMode.id}"] .agent-avatar`).first();
  await avatar.hover();
  const card = page.locator(".agent-avatar-profile-card");
  await card.waitFor();
  await page.waitForTimeout(300);
  return around(page, [card], 4);
});
// iPhone Home Screen app.
await shot("lantor-mobile-home", IPHONE, async page => {
  await openChannel(page, "pebble-dev");
  await page.getByRole("button", { name: "Home", exact: true }).click();
  await page.waitForTimeout(400);
  await settle(page);
});
await shot("lantor-mobile-channel", IPHONE, async page => {
  await openChannel(page, "pebble-dev");
  await settle(page);
});
await shot("lantor-mobile-needs-you", IPHONE, async page => {
  await page.getByRole("button", { name: /Needs you/ }).first().click();
  await page.waitForTimeout(400);
});

await browser.close();
server.close();
if (unknownCalls.size) console.log("unhandled API calls (answered with ok):", [...unknownCalls].join(", "));
if (failures.length) { console.log(`failed: ${failures.join(", ")}`); process.exitCode = 1; }
