import assert from "node:assert/strict";
import test from "node:test";

import { DecisionStore, deriveNeedsYou, formatIdle, STALLED_TASK_AFTER_MS } from "../src/decisions";
import type { Decision, Task, ThreadActivity } from "../src/types";

const NOW = Date.parse("2026-09-23T12:00:00.000Z");
const hoursAgo = (hours: number) => new Date(NOW - hours * 3_600_000).toISOString();

function decision(id: string, status: string, createdHoursAgo: number): Decision {
  return {
    id, message_id: `m-${id}`, channel_id: "c", channel_name: "lantor-dev", thread_root_id: null,
    requester_agent_id: "a", requester_handle: "Vegapunk", task_id: null, task_number: null,
    title: `Question ${id}`, context: "", options: [], status, answer_option_id: null, answer_note: "",
    answer_message_id: null, resolved_at: null, created_at: hoursAgo(createdHoursAgo), updated_at: hoursAgo(createdHoursAgo),
  };
}

function task(number: number, status: string, updatedHoursAgo: number): Task {
  return {
    id: `t${number}`, number, message_id: `root-${number}`, channel_id: "c", title: `Task ${number}`, status,
    version: 1, channel_name: "lantor-dev", assignee_id: null, assignee_name: null,
    created_at: hoursAgo(updatedHoursAgo + 1), updated_at: hoursAgo(updatedHoursAgo),
  };
}

function activity(rootId: string, latestHoursAgo: number): ThreadActivity {
  return {
    thread_root_id: rootId, channel_id: "c", unread_count: 0,
    latest_message_id: "x", latest_activity_at: hoursAgo(latestHoursAgo),
  };
}

test("needs-you collects open decisions, reviews, and stalled tasks", () => {
  const result = deriveNeedsYou(
    [decision("old", "open", 30), decision("new", "open", 1), decision("done", "answered", 2)],
    [
      task(1, "in_review", 5),
      task(2, "in_review", 200),
      task(3, "in_progress", 100),
      task(4, "in_progress", 100),
      task(5, "todo", 10),
      task(6, "done", 500),
    ],
    [activity("root-4", 2)],
    NOW,
  );
  assert.deepEqual(result.decisions.map((item) => item.id), ["new", "old"]);
  assert.deepEqual(result.reviews.map((item) => item.task.number), [2, 1], "oldest review first");
  assert.deepEqual(result.stalled.map((item) => item.task.number), [3],
    "recent thread replies keep #4 active; #5 is too young");
  assert.equal(result.count, 4, "badge counts decisions and reviews, not stalled tasks");
});

test("needs-you tolerates bootstraps without decisions", () => {
  const result = deriveNeedsYou(undefined, [], [], NOW);
  assert.equal(result.count, 0);
  assert.equal(STALLED_TASK_AFTER_MS, 3 * 24 * 3_600_000);
});

test("idle labels switch from hours to days", () => {
  assert.equal(formatIdle(10 * 60_000), "just now");
  assert.equal(formatIdle(5 * 3_600_000), "5h idle");
  assert.equal(formatIdle(80 * 3_600_000), "3d idle");
});

test("decision store notifies only when a decision changes", () => {
  const store = new DecisionStore();
  let notifications = 0;
  store.subscribe(() => { notifications += 1; });
  const first = decision("a", "open", 1);
  store.set([first]);
  assert.equal(store.get("m-a"), first);
  store.set([first]);
  assert.equal(notifications, 1, "same objects do not notify");
  store.set([{ ...first, status: "answered" }]);
  assert.equal(notifications, 2);
  assert.equal(store.get("m-a")?.status, "answered");
  store.set(undefined);
  assert.equal(store.get("m-a"), undefined);
  assert.equal(notifications, 3);
});
