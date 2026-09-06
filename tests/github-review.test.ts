import assert from "node:assert/strict";
import test from "node:test";
import { githubComparisonRequestKey, mergeGithubReviewComparisons } from "../src/github-review";
import type { GithubChannelOverview, GithubReviewComparisons } from "../src/types";

function overview(): GithubChannelOverview {
  return {
    account: { login: "owner", host: "github.com" },
    binding: {
      channel_id: "channel", repository_id: "repo", name_with_owner: "owner/repo",
      url: "", local_path: "", account_login: "owner", review_login: "owner",
      review_queue_synced_at: null, issue_queue_synced_at: null, created_at: "", updated_at: "",
    },
    issues: [],
    review_requests: [{
      number: 42, title: "PR", url: "", author_login: "author", is_draft: false,
      state: "OPEN", updated_at: "", head_sha: "head", is_review_requested: true, is_authored: false,
      checks: { status: "none", total: 0, pending: 0, failed: 0, failing_checks: [] },
      linked_thread_root_id: "thread", linked_task_id: "task", linked_task_number: 1,
      linked_task_status: "done", linked_assignee_id: "agent", linked_assignee_name: "Agent",
      review_anchor_sha: "base", review_is_stale: true, review_commits_ahead: null,
    }],
  };
}
const result: GithubReviewComparisons = {
  repository_id: "repo",
  comparisons: [{ pull_number: 42, review_anchor_sha: "base", head_sha: "head", commits_ahead: 3 }],
};

test("enriches matching heads without replacing list or task metadata", () => {
  const current = overview();
  const updated = mergeGithubReviewComparisons(current, result);
  assert.equal(updated.review_requests[0].review_commits_ahead, 3);
  assert.equal(current.review_requests[0].review_commits_ahead, null);
  assert.equal(updated.review_requests[0].linked_task_status, "done");
  assert.strictEqual(updated.issues, current.issues);
  assert.strictEqual(mergeGithubReviewComparisons(updated, result), updated);
  assert.equal(githubComparisonRequestKey(updated), null);
});

test("ignores late counts after rebind, push, or re-review", () => {
  for (const change of [
    (o: GithubChannelOverview) => { o.binding!.repository_id = "another-repo"; },
    (o: GithubChannelOverview) => { o.review_requests[0].head_sha = "new-head"; },
    (o: GithubChannelOverview) => { o.review_requests[0].review_anchor_sha = "new-base"; },
    (o: GithubChannelOverview) => { o.review_requests[0].review_is_stale = false; },
  ]) {
    const current = overview();
    change(current);
    assert.strictEqual(mergeGithubReviewComparisons(current, result), current);
  }
});

test("request deduplication distinguishes changed heads and channels", () => {
  const current = overview();
  const originalKey = githubComparisonRequestKey(current);
  assert.ok(originalKey);
  current.review_requests[0].head_sha = "new-head";
  assert.notEqual(githubComparisonRequestKey(current), originalKey);
  current.review_requests[0].head_sha = "head";
  current.binding!.channel_id = "another-channel";
  assert.notEqual(githubComparisonRequestKey(current), originalKey);
  current.binding = null;
  assert.equal(githubComparisonRequestKey(current), null);
});

test("failed optional comparisons keep the usable PR list", () => {
  const current = overview();
  assert.strictEqual(mergeGithubReviewComparisons(current, { repository_id: "repo", comparisons: [] }), current);
  assert.ok(githubComparisonRequestKey(current));
});
