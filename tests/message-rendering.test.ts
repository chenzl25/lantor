import assert from "node:assert/strict";
import { test } from "node:test";
import { retainEqual } from "../src/render-identity";
import { activeProgressByAgent, indexProgress } from "../src/components/ActivityProgressDock";
import type { AgentActivity, AgentRun, AgentWorkItem, Message } from "../src/types";

test("bootstrap structural sharing retains equal rows and nested attachments without ignoring edits", () => {
  const before = { a: { body: "plain", attachments: [{ id: "a", size: 3 }] }, b: { body: "old", metadata: { title: "before" } } };
  assert.equal(retainEqual(before, structuredClone(before)), before);
  const snapshot = structuredClone(before);
  snapshot.b.metadata.title = "after";
  const after = retainEqual(before, snapshot);
  assert.equal(after.a, before.a);
  assert.notEqual(after.b, before.b);
  assert.equal(after.b.metadata.title, "after");
  assert.equal(before.b.metadata.title, "before");
  const removed = retainEqual<Record<string, unknown>>(before, { b: snapshot.b });
  assert.deepEqual(Object.keys(removed), ["b"]);
  const key = JSON.parse('{"__proto__":{"safe":true}}');
  assert.equal(Object.prototype.hasOwnProperty.call(retainEqual({}, key), "__proto__"), true);
});

const activity = (id: string, run: string, title: string, minute: number): AgentActivity => ({
  id, agent_id: "agent", agent_handle: "Hancock", run_id: run, kind: "thinking", phase: "thinking", status: "running",
  title, summary: title, detail: "", metadata: {}, created_at: `2026-09-06T12:0${minute}:00Z`,
});
const work = (id: string, root: string | null, run: string | null, status = "running"): AgentWorkItem => ({
  id, agent_id: "agent", agent_handle: "Hancock", channel_id: "channel", channel_name: "test", thread_root_id: root,
  source_message_id: root, task_id: null, task_number: null, source_kind: "mention", title: "test", context: "", status, run_id: run,
  created_at: "2026-09-06T12:00:00Z", updated_at: "2026-09-06T12:00:00Z", completed_at: null,
});

test("one progress index isolates channels/roots and preserves useful, newest-first compact history", () => {
  const activities = [activity("a", "run-a", "First", 1), activity("b", "run-b", "Other thread", 3), activity("c", "run-a", "Latest", 4), activity("duplicate", "run-a", "Latest", 5), activity("hidden", "run-a", "Request acknowledged", 6)];
  const workItems = [work("wa", "a", "run-a"), work("wb", "b", "run-b"), work("root", null, null, "queued"), { ...work("foreign", "a", "run-b"), channel_id: "other-channel" }];
  const original = structuredClone(activities);
  const index = indexProgress(activities, [], workItems, []);
  const a = activeProgressByAgent([], index, "channel", "a");
  assert.equal(a.length, 1);
  assert.equal(a[0].latestActivity?.id, "duplicate");
  assert.deepEqual(a[0].history.map((item) => item.title), ["Latest", "First"]);
  assert.equal(a[0].workItem?.id, "wa");
  assert.equal(activeProgressByAgent([], index, "channel", "b")[0].latestActivity?.id, "b");
  assert.equal(activeProgressByAgent([], index, "channel", null)[0].state, "queued");
  assert.equal(activeProgressByAgent([], index, "channel", null)[0].queuedItems[0].id, "root");
  assert.deepEqual(activeProgressByAgent([], index, null, "a"), []);
  assert.deepEqual(activities, original, "indexing must not sort the input in place");
});

test("terminal progress clears running candidates while empty streams and queued work remain supported", () => {
  const runId = "00000000-0000-4000-8000-000000000001";
  const terminal = { ...activity("end", runId, "Completed", 6), phase: "runtime" };
  const item = work("w", "root", runId);
  const index = indexProgress([terminal], [], [item], []);
  assert.deepEqual(activeProgressByAgent([], index, "channel", "root"), []);
  const message = { sender_name: "Hancock", sender_role: "agent", delivery_state: "streaming", body: "", attachments: [], artifacts: [], stream_key: `${runId}:response`, updated_at: "2026-09-06T12:00:00Z" } as unknown as Message;
  const emptyIndex = indexProgress([], [], [], []);
  assert.equal(activeProgressByAgent([message], emptyIndex, "channel", null)[0].state, "working");
  assert.deepEqual(activeProgressByAgent([{ ...message, body: "Visible response" }], emptyIndex, "channel", null), []);
  const run = { id: runId, status: "running", started_at: "2026-09-06T12:00:00Z", stopped_at: null } as AgentRun;
  assert.equal(activeProgressByAgent([], indexProgress([], [run], [{ ...item, status: "done" }], []), "channel", "root").length, 1, "active run wins over a settled work item");
  assert.deepEqual(activeProgressByAgent([], indexProgress([], [], [{ ...item, status: "done", updated_at: "2000-01-01T00:00:00Z" }], []), "channel", "root"), [], "expired completion does not stay active");
});
