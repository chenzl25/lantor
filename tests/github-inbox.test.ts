import assert from "node:assert/strict";
import test from "node:test";

import {
  githubInboxCounts,
  githubInboxItemsWithPending,
  githubInboxReasonLabel,
  githubInboxRepositories,
  githubInboxUnreadCount,
  githubInboxWorkItems,
  githubNotificationKey,
  githubNotificationsByResource,
  matchesGithubInboxFilter,
} from "../src/github-inbox";
import { scopesForRefresh } from "../src/ui-state-sync";
import type { GithubInbox, GithubInboxItem } from "../src/types";

function item(thread_id: string, reason: string, unread: boolean, repository = "org/repo"): GithubInboxItem {
  return {
    thread_id,
    repository,
    subject_type: "PullRequest",
    title: `Change ${thread_id}`,
    number: Number(thread_id),
    url: `https://github.com/${repository}/pull/${thread_id}`,
    reason,
    unread,
    updated_at: "2026-11-09T02:00:00Z",
    thread_root_id: null,
    channel_id: null,
  };
}

const items = [
  item("1", "review_requested", true),
  item("2", "review_requested", false),
  item("3", "mention", true, "org/docs"),
  item("4", "assign", true),
  item("5", "author", true),
  item("6", "comment", false, "org/docs"),
];

test("filter counts are unread threads per reason, as on GitHub", () => {
  assert.deepEqual(githubInboxCounts(items), {
    participating: 4,
    review_requested: 1,
    assign: 1,
    mention: 1,
    team_mention: 0,
  });
  assert.deepEqual(
    items.filter((entry) => matchesGithubInboxFilter(entry, "review_requested")).map((entry) => entry.thread_id),
    ["1", "2"],
  );
  assert.equal(items.filter((entry) => matchesGithubInboxFilter(entry, "participating")).length, items.length);
});

test("pending marks apply before the backend answers", () => {
  const pending = githubInboxItemsWithPending(items, { read: new Set(["1"]), done: new Set(["4"]) });
  assert.deepEqual(pending.map((entry) => entry.thread_id), ["1", "2", "3", "5", "6"]);
  assert.equal(pending[0].unread, false);
  assert.equal(githubInboxCounts(pending).participating, 2);
  const none = { read: new Set<string>(), done: new Set<string>() };
  assert.equal(githubInboxItemsWithPending(items, none), items);
});

test("repositories list the most unread first", () => {
  assert.deepEqual(githubInboxRepositories(items), [
    { repository: "org/repo", unread: 3, total: 4 },
    { repository: "org/docs", unread: 1, total: 2 },
  ]);
});

test("the sidebar counts unread threads only once the inbox is on", () => {
  const inbox: GithubInbox = { enabled: true, account_login: "octo", synced_at: null, error: null, items };
  assert.equal(githubInboxUnreadCount(inbox), 4);
  assert.equal(githubInboxUnreadCount({ ...inbox, enabled: false }), 0);
  assert.equal(githubInboxUnreadCount(undefined), 0);
});

test("reasons read as GitHub writes them", () => {
  assert.equal(githubInboxReasonLabel("review_requested"), "review requested");
  assert.equal(githubInboxReasonLabel("assign"), "assigned");
  assert.equal(githubInboxReasonLabel("member_feature_requested"), "member feature requested");
});

test("an inbox update reloads the inbox and the channels that count it", () => {
  assert.deepEqual(scopesForRefresh("github_inbox_updated"), ["github_inbox", "channels"]);
  // A new binding changes which channel an inbox thread names.
  assert.deepEqual(scopesForRefresh("github_repository_bound"), ["github_inbox", "channels"]);
  // A new review task links its pull request in the inbox.
  assert.ok(scopesForRefresh("github_review_task_created")?.includes("github_inbox"));
  // Other GitHub refreshes still reload the channels' queue badges.
  assert.deepEqual(scopesForRefresh("github_review_attention_read"), ["channels"]);
  assert.deepEqual(scopesForRefresh("github_channel_queue_refreshed"), ["channels"]);
});

test("a bound channel shows the pull request and issue threads of its repository", () => {
  const issue = { ...item("7", "mention", true, "Org/Repo"), subject_type: "Issue", url: "https://github.com/Org/Repo/issues/7" };
  const release = { ...item("8", "subscribed", true), subject_type: "Release", number: null };
  const threads = [...items, issue, release];
  assert.deepEqual(
    githubInboxWorkItems(threads, "ORG/repo").map((entry) => entry.thread_id),
    ["1", "2", "4", "5", "7"],
  );

  const byResource = githubNotificationsByResource(githubInboxWorkItems(threads, "org/repo"));
  assert.equal(byResource.get(githubNotificationKey("PullRequest", 1))?.thread_id, "1");
  assert.equal(byResource.get(githubNotificationKey("Issue", 7))?.thread_id, "7");
  // Pull request 7 and issue 7 are different resources.
  assert.equal(byResource.get(githubNotificationKey("PullRequest", 7)), undefined);
});
