// Production App against an isolated API: the GitHub panel lists the owner's
// notifications with GitHub's filters and counts, and marks threads read or
// done; a channel bound to a repository shares that unread state in its GitHub
// tab. Run npm run build first. No live workspace data or GitHub account is used.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { chromium, devices, webkit } from "playwright";

const id = n => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const channelId = id(1000);
const createdAt = "2026-01-05T09:00:00.000Z";
const root = { id: id(1), seq: 1, channel_id: channelId, thread_root_id: null, sender_agent_id: null, sender_name: "Dylan",
  sender_role: "owner", body: "Review org/repo#101", is_task: false, thread_followed: false, delivery_state: "complete",
  stream_key: "", task_number: null, task_status: null, attachments: [], artifacts: [], created_at: createdAt, updated_at: createdAt };
const reply = { ...root, id: id(2), seq: 2, thread_root_id: root.id, body: "Review notes for the spill sort" };
const agent = { id: id(800), handle: "Speed", display_name: "Speed", role: "agent", status: "idle", runtime: "claude",
  model: "", reasoning_effort: "", service_tier: "", avatar: "S", description: "", launch_command: "", environment_variables: "",
  working_directory: "", workspace_exists: false, workspace_memory_path: "", workspace_memory_exists: false,
  workspace_entries: [], details_loaded: true, daily_budget_micros: 0, subscription_status: null };

const minutesAgo = minutes => new Date(Date.now() - minutes * 60_000).toISOString();
const notification = (threadId, reason, unread, extra = {}) => ({
  thread_id: threadId, repository: "org/repo", subject_type: "PullRequest", title: `Pull request ${threadId}`,
  number: Number(threadId), url: `https://github.com/org/repo/pull/${threadId}`, reason, unread,
  updated_at: minutesAgo(Number(threadId) - 100), thread_root_id: null, channel_id: null, ...extra,
});
const initialItems = () => [
  notification("101", "review_requested", true, { title: "Make the spilling sort an external merge sort", thread_root_id: root.id,
    url: "https://github.com/org/repo/pull/101#issuecomment-5" }),
  notification("102", "mention", true, { repository: "org/docs", subject_type: "Issue", title: "Document spilling",
    url: "https://github.com/org/docs/issues/102" }),
  notification("103", "author", false, { title: "An older pull request" }),
  notification("104", "comment", true, { subject_type: "RepositoryAdvisory", number: null, title: "Advisory: pgwire frame",
    url: "https://github.com/org/repo/security" }),
  notification("105", "assign", true, { title: "Assigned pull request" }),
];
const teamMention = notification("100", "team_mention", true, { title: "Team review" });

let state;
let requests = [];
let cursor = 0;
const clients = new Set();
const publish = event => { cursor++; for (const client of clients) client.write(`id: ${cursor}\nevent: lantor\ndata: ${JSON.stringify(event)}\n\n`); };
// As the backend counts a bound channel's GitHub unread state.
const channelUnread = () => state.github_inbox.items.filter(entry => entry.unread && entry.repository === "org/repo"
  && (entry.subject_type === "PullRequest" || entry.subject_type === "Issue")).length;
const withChannelCounts = () => {
  if (state.linked) state.channels[0].github_unread_count = channelUnread();
  return state.channels;
};
const pullRequest = (number, title, extra = {}) => ({ number, title, url: `https://github.com/org/repo/pull/${number}`,
  author_login: "alice", is_draft: false, state: "OPEN", updated_at: createdAt, head_sha: `sha${number}`,
  checks: { status: "success", total: 1, pending: 0, failed: 0, failing_checks: [] }, is_review_requested: true, is_authored: false,
  linked_thread_root_id: null, linked_task_id: null, linked_task_number: null, linked_task_status: null, linked_assignee_id: null,
  linked_assignee_name: null, review_anchor_sha: null, review_is_stale: false, review_commits_ahead: null, ...extra });
const overview = () => ({ account: { login: "octo", host: "github.com" },
  binding: { channel_id: channelId, repository_id: "R_1", name_with_owner: "org/repo", url: "https://github.com/org/repo", local_path: "",
    account_login: "octo", review_login: "octo", review_queue_synced_at: new Date().toISOString(),
    issue_queue_synced_at: new Date().toISOString(), created_at: createdAt, updated_at: createdAt },
  review_requests: [pullRequest(105, "Assigned pull request"), pullRequest(107, "A quiet pull request")], issues: [] });

/** `linked`: the channel is bound to org/repo and counts the inbox's unread threads. */
function resetState({ linked = false } = {}) {
  const items = initialItems();
  if (linked) {
    items.push(notification("106", "mention", true, { subject_type: "Issue", title: "Spill files leak",
      url: "https://github.com/org/repo/issues/106" }));
    for (const entry of items) if (entry.repository === "org/repo") entry.channel_id = channelId;
  }
  state = { linked, db_url: "synthetic://github-inbox", web_base_url: null, owner_profile: { display_name: "Dylan", avatar: "D", description: "" },
    channels: [{ id: channelId, name: "repo", kind: "channel", dm_agent_id: null, description: "", unread_count: 0, github_unread_count: 0,
      github_unread_from_inbox: linked, github_review_synced_at: null, github_issue_synced_at: null }],
    messages: [root], channel_message_history: [{ channel_id: channelId, before_seq: null, has_more: false }],
    agents: linked ? [agent] : [], tasks: [], channel_members: linked ? [{ channel_id: channelId, agent_id: agent.id }] : [],
    thread_activities: [{ thread_root_id: root.id, channel_id: channelId, reply_count: 1, unread_count: 0, agent_unread_count: 0,
      first_unread_agent_message_id: null, latest_message_id: reply.id, latest_activity_at: createdAt }],
    saved_messages: [], dismissed_inbox_items: {}, read_inbox_items: {}, artifacts: [], reminders: [], agent_runs: [],
    agent_work_items: [], agent_activities: [], supervisor: { pid: null, status: "stopped", updated_at: null },
    launch_agent: { label: "", plist_path: "", installed: false, loaded: false }, ui_event_cursor: 0,
    // Synced long enough ago that opening the panel checks GitHub again.
    github_inbox: { enabled: true, account_login: "octo", synced_at: linked ? new Date().toISOString() : minutesAgo(10), error: null, items } };
  requests = [];
}

const inboxResponse = () => structuredClone(state.github_inbox);
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
  if (url.pathname === "/api/events") {
    res.writeHead(200, { "content-type": "text/event-stream" }); res.write(": ready\n\n"); clients.add(res); res.on("close", () => clients.delete(res)); return;
  }
  let raw = ""; for await (const chunk of req) raw += chunk;
  const args = raw ? JSON.parse(raw) : {};
  requests.push({ path: url.pathname, args });
  let result = { ok: true };
  const item = () => state.github_inbox.items.find(entry => entry.thread_id === args.threadId);
  switch (url.pathname) {
    case "/api/bootstrap": withChannelCounts(); result = { ...state, ui_event_cursor: cursor }; break;
    case "/api/load_channel_previews": case "/api/load_activity_messages": case "/api/search_messages": result = []; break;
    case "/api/load_channel_messages": result = { messages: [root], next_before_seq: null, has_more: false, thread_activities: state.thread_activities }; break;
    case "/api/load_thread_page": result = { messages: [root, reply], next_before_seq: null, has_more: false }; break;
    case "/api/load_message": result = [root, reply].find(m => m.id === args.messageId); break;
    case "/api/load_ui_state": withChannelCounts(); result = Object.fromEntries(args.scopes.map(scope => [scope, state[scope]])); break;
    case "/api/replay_ui_events": result = { cursor, replayGap: false, events: [] }; break;
    case "/api/load_github_review_queue": case "/api/refresh_github_review_queue": case "/api/refresh_github_issue_queue":
      result = overview(); break;
    case "/api/load_github_review_comparisons": result = { repository_id: "R_1", comparisons: [] }; break;
    case "/api/mark_github_inbox_repository_read":
      for (const entry of state.github_inbox.items) {
        if (entry.repository === args.repository && (entry.subject_type === "PullRequest" || entry.subject_type === "Issue")) entry.unread = false;
      }
      publish({ type: "refresh", reason: "github_inbox_updated" });
      result = inboxResponse();
      break;
    case "/api/create_github_review_task":
      result = { thread_root_id: root.id, task_id: id(900), task_number: 7, head_sha: `sha${args.pullNumber}`, created: true };
      break;
    case "/api/load_github_references": result = { repositories: [], items: [] }; break;
    case "/api/lookup_github_references":
      result = { error: null, items: args.references.filter(ref => ref.number === 101).map(ref => ({ name_with_owner: ref.nameWithOwner,
        number: ref.number, kind: "pull", title: "Make the spilling sort an external merge sort", url: "https://github.com/org/repo/pull/101",
        state: "open", state_reason: "", is_draft: false, checks_status: "success", updated_at: createdAt })) };
      break;
    case "/api/refresh_github_inbox":
      // Slow enough that the panel shows its cached threads first.
      await new Promise(done => setTimeout(done, 150));
      if (!state.github_inbox.items.some(entry => entry.thread_id === teamMention.thread_id)) state.github_inbox.items.push(teamMention);
      state.github_inbox.synced_at = new Date().toISOString();
      result = inboxResponse();
      break;
    case "/api/mark_github_inbox_read":
      item().unread = false;
      publish({ type: "refresh", reason: "github_inbox_updated" });
      result = inboxResponse();
      break;
    case "/api/mark_github_inbox_done":
      if (args.threadId === "105") {
        // GitHub refuses: the row comes back with the error.
        await new Promise(done => setTimeout(done, 150));
        // As web.rs answers a failed operation.
        res.writeHead(400, { "content-type": "application/json" }).end(JSON.stringify({ ok: false, message: "GitHub CLI request failed: HTTP 403" }));
        return;
      }
      state.github_inbox.items = state.github_inbox.items.filter(entry => entry.thread_id !== args.threadId);
      result = inboxResponse();
      break;
  }
  res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(result));
});
await new Promise(done => api.listen(0, "127.0.0.1", done));
const origin = `http://127.0.0.1:${api.address().port}`;
const calls = path => requests.filter(r => r.path === path).map(r => r.args);

async function run(engine, name, contextOptions) {
  resetState();
  const browser = await engine.launch();
  try {
    const context = await browser.newContext({ serviceWorkers: "block", ...contextOptions });
    const page = await context.newPage(); page.setDefaultTimeout(8000);
    const errors = []; page.on("pageerror", error => errors.push(error.message));
    await page.addInitScript(() => {
      window.__opened = [];
      window.open = url => { window.__opened.push(String(url)); return null; };
    });
    const mobile = Boolean(contextOptions.isMobile);
    await page.goto(origin);
    // The sidebar counts unread threads before the panel opens; phones list
    // GitHub in the menu on Home, next to Saved.
    let entry;
    if (mobile) {
      await page.getByRole("navigation", { name: "Primary mobile navigation" }).getByRole("button", { name: "Home" }).click();
      await page.getByRole("button", { name: "Your menu" }).click();
      entry = page.getByRole("menuitem", { name: /GitHub/ });
      await entry.filter({ hasText: "GitHub (4 unread)" }).waitFor();
    } else {
      entry = page.locator(".quick-actions").getByRole("button", { name: /GitHub/ });
      await entry.filter({ hasText: "4" }).waitFor();
    }
    await entry.click();
    const panel = page.getByRole("dialog", { name: "GitHub notifications" });
    await panel.waitFor();
    const rows = panel.locator(".github-inbox-row");
    const visibleIds = () => rows.evaluateAll(nodes => nodes.map(node => node.dataset.threadId));
    // Cached threads show at once, newest first; the stale cache is checked again.
    assert.deepEqual(await visibleIds(), ["101", "102", "103", "104", "105"], `${name}: cached threads first`);
    await panel.locator('.github-inbox-row[data-thread-id="100"]').waitFor();
    assert.equal(calls("/api/refresh_github_inbox").length, 1, `${name}: opening a stale inbox checks GitHub once`);
    await panel.getByText("@octo · 5 unread").waitFor();

    // GitHub's filters, each with its unread count.
    const tab = label => panel.getByRole("tab", { name: new RegExp(`^${label}`) });
    const tabCounts = await panel.getByRole("tab").evaluateAll(nodes => nodes.map(node => node.textContent));
    assert.deepEqual(tabCounts, ["Participating5", "Review requested1", "Assigned1", "Mentioned1", "Team mentioned1"], name);
    await tab("Review requested").click();
    assert.deepEqual(await visibleIds(), ["101"], `${name}: review requests only`);
    // The pull request shows its state from the reference lookup.
    await panel.locator('.github-inbox-row[data-thread-id="101"] .github-reference[data-status="open"]').waitFor();
    await tab("Participating").click();
    await panel.getByRole("combobox", { name: "Repository" }).selectOption("org/docs");
    assert.deepEqual(await visibleIds(), ["102"], `${name}: one repository`);
    await panel.getByRole("combobox", { name: "Repository" }).selectOption("");

    // Opening a thread opens GitHub at the latest comment and marks it read there.
    await panel.locator('.github-inbox-row[data-thread-id="101"] h3').click();
    await panel.locator('.github-inbox-row[data-thread-id="101"]:not(.unread)').waitFor();
    assert.deepEqual(await page.evaluate(() => window.__opened), ["https://github.com/org/repo/pull/101#issuecomment-5"], name);
    assert.deepEqual(calls("/api/mark_github_inbox_read"), [{ threadId: "101" }], name);
    await panel.getByText("@octo · 4 unread").waitFor();
    if (!mobile) await entry.filter({ hasText: "4" }).waitFor({ state: "attached" });

    // Done hides a thread; a refused write brings it back with the error.
    await panel.locator('.github-inbox-row[data-thread-id="102"]').getByRole("button", { name: "Done" }).click();
    await panel.locator('.github-inbox-row[data-thread-id="102"]').waitFor({ state: "detached" });
    await panel.locator('.github-inbox-row[data-thread-id="105"]').getByRole("button", { name: "Done" }).click();
    await panel.locator('.github-inbox-row[data-thread-id="105"]').waitFor({ state: "detached" });
    await panel.getByRole("alert").filter({ hasText: "HTTP 403" }).waitFor();
    await panel.locator('.github-inbox-row[data-thread-id="105"].unread').waitFor();
    assert.deepEqual(calls("/api/mark_github_inbox_done"), [{ threadId: "102" }, { threadId: "105" }], name);

    // Mark as read without opening GitHub (desktop; phones show only Done).
    const markRead = panel.locator('.github-inbox-row[data-thread-id="104"]').getByRole("button", { name: "Mark as read" });
    if (mobile) {
      assert.equal(await markRead.isVisible(), false, name);
    } else {
      await markRead.click();
      await panel.locator('.github-inbox-row[data-thread-id="104"]:not(.unread)').waitFor();
      assert.equal((await page.evaluate(() => window.__opened)).length, 1, `${name}: marking read opens nothing`);
    }

    // A pull request with a Lantor review thread opens it.
    await panel.locator('.github-inbox-row[data-thread-id="101"]').getByRole("button", { name: "Open Lantor thread" }).click();
    await panel.waitFor({ state: "detached" });
    await page.locator(`.thread [data-message-id="${reply.id}"]`).waitFor();
    assert.deepEqual(errors, []);
    console.log(`ok - ${name}`);
  } finally {
    await browser.close();
  }
}

async function openInbox(page, mobile) {
  if (mobile) {
    await page.getByRole("navigation", { name: "Primary mobile navigation" }).getByRole("button", { name: "Home" }).click();
    await page.getByRole("button", { name: "Your menu" }).click();
    await page.getByRole("menuitem", { name: /GitHub/ }).click();
  } else {
    await page.locator(".quick-actions").getByRole("button", { name: /GitHub/ }).click();
  }
  const panel = page.getByRole("dialog", { name: "GitHub notifications" });
  await panel.waitFor();
  return panel;
}

// The channel bound to org/repo: its GitHub tab counts the inbox's unread
// threads there, the inbox links to it, and either side reads them on GitHub.
async function runLinked(engine, name, contextOptions) {
  resetState({ linked: true });
  const browser = await engine.launch();
  try {
    const context = await browser.newContext({ serviceWorkers: "block", ...contextOptions });
    const page = await context.newPage(); page.setDefaultTimeout(8000);
    const errors = []; page.on("pageerror", error => errors.push(error.message));
    await page.addInitScript(() => {
      window.__opened = [];
      window.open = url => { window.__opened.push(String(url)); return null; };
    });
    const mobile = Boolean(contextOptions.isMobile);
    await page.goto(origin);

    // Inbox rows in the bound repository name the channel; pull requests
    // without a Lantor thread can go to an agent.
    let panel = await openInbox(page, mobile);
    const inboxRow = threadId => panel.locator(`.github-inbox-row[data-thread-id="${threadId}"]`);
    await inboxRow("106").getByRole("button", { name: "repo" }).waitFor();
    assert.equal(await inboxRow("102").locator(".github-inbox-channel").count(), 0, `${name}: org/docs has no channel`);
    assert.equal(await inboxRow("104").locator(".github-inbox-channel").count(), 0, `${name}: the tab lists no advisories`);
    assert.equal(await inboxRow("101").getByRole("button", { name: "Review with agent" }).count(), 0, `${name}: reviewed already`);
    await inboxRow("105").getByRole("button", { name: "Review with agent" }).waitFor();

    // The channel chip opens the channel's GitHub tab at that thread.
    await inboxRow("106").getByRole("button", { name: "repo" }).click();
    await panel.waitFor({ state: "detached" });
    const githubTab = page.locator(".tabs button", { hasText: "GitHub" });
    await githubTab.locator(".tab-unread-badge", { hasText: "3" }).waitFor();
    const notificationsTab = page.getByRole("tab", { name: /^Notifications/ });
    assert.equal(await notificationsTab.getAttribute("aria-selected"), "true", `${name}: opens Notifications`);
    const list = page.locator(".github-notification-list");
    await list.locator('.github-inbox-row.highlighted[data-thread-id="106"]').waitFor();
    // Pull requests and issues of org/repo; not its advisory, not org/docs.
    assert.deepEqual(await list.locator(".github-inbox-row").evaluateAll(nodes => nodes.map(node => node.dataset.threadId)),
      ["101", "103", "105", "106"], name);

    // A pull request card shows its unread notification; opening it reads it on GitHub.
    await page.getByRole("tab", { name: /^Pull requests/ }).click();
    const card = page.locator(".github-pull-card", { hasText: "Assigned pull request" });
    await card.locator("mark.notification", { hasText: "assigned" }).waitFor();
    assert.equal(await page.locator(".github-pull-card.unread").count(), 1, `${name}: only #105 is unread`);
    await card.locator(".github-pull-title").click();
    await page.locator(".github-pull-card.unread").waitFor({ state: "detached" });
    assert.deepEqual(calls("/api/mark_github_inbox_read"), [{ threadId: "105" }], name);
    assert.deepEqual(await page.evaluate(() => window.__opened), ["https://github.com/org/repo/pull/105"], name);
    await githubTab.locator(".tab-unread-badge", { hasText: "2" }).waitFor();

    // Mark all read reads the rest of the repository's threads.
    await notificationsTab.click();
    await page.getByRole("button", { name: "Mark all read" }).click();
    await githubTab.locator(".tab-unread-badge").waitFor({ state: "detached" });
    assert.equal(await list.locator(".github-inbox-row.unread").count(), 0, name);
    assert.deepEqual(calls("/api/mark_github_inbox_repository_read"), [{ repository: "org/repo" }], name);
    // The tab does not clear the old review-request badge on open.
    assert.equal(calls("/api/mark_github_review_attention_read").length, 0, name);
    // The inbox's request shows the thread once; the tab opens as usual later.
    await page.locator(".tabs button", { hasText: "Chat" }).click();
    await githubTab.click();
    await page.getByRole("tab", { name: /^Pull requests/, selected: true }).waitFor();

    // Review with agent from the inbox starts a review task in the channel.
    panel = await openInbox(page, mobile);
    await inboxRow("105").getByRole("button", { name: "Review with agent" }).click();
    const dialog = page.getByRole("dialog", { name: "Review org/repo#105" });
    await dialog.getByRole("button", { name: "Start review" }).click();
    await panel.waitFor({ state: "detached" });
    assert.deepEqual(calls("/api/create_github_review_task"), [{ channelId, pullNumber: 105, agentId: agent.id }], name);
    await page.locator(`.thread [data-message-id="${reply.id}"]`).waitFor();
    assert.deepEqual(errors, []);
    console.log(`ok - ${name}: channel link`);
  } finally {
    await browser.close();
  }
}

try {
  await run(chromium, "chromium desktop", { viewport: { width: 1280, height: 820 } });
  await run(webkit, "webkit desktop", { viewport: { width: 1280, height: 820 } });
  await run(webkit, "webkit iphone", { ...devices["iPhone 13"] });
  await runLinked(chromium, "chromium desktop", { viewport: { width: 1280, height: 820 } });
  await runLinked(webkit, "webkit desktop", { viewport: { width: 1280, height: 820 } });
  await runLinked(webkit, "webkit iphone", { ...devices["iPhone 13"] });
} finally {
  api.close();
}
