import assert from "node:assert/strict";
import test from "node:test";
import { activeProgressByAgent, indexProgress } from "../src/components/ActivityProgressDock";
import { errorDetail, retainWorkItems, workItemFailure } from "../src/work-item-state";
import type { AgentActivity, AgentWorkItem } from "../src/types";

const time = "2026-09-01T12:00:00.000Z";
const work = (id: string, extra: Partial<AgentWorkItem> = {}): AgentWorkItem => ({
  id, agent_id: "agent", agent_handle: "Agent", channel_id: "channel", channel_name: "channel",
  thread_root_id: "thread", source_message_id: "thread", task_id: null, task_number: null,
  source_kind: "thread_followup", title: "Original request", context: "", status: "failed", run_id: "run",
  created_at: time, updated_at: time, completed_at: time, ...extra,
});
const terminal: AgentActivity = { id: "activity", agent_id: "agent", agent_handle: "Agent", run_id: "run",
  kind: "dispatch", phase: "work", status: "error", title: "Request failed", summary: "Request failed",
  detail: '{"error":{"message":"Provider connection closed"}}', metadata: {}, created_at: time };
const progress = (items: AgentWorkItem[], activities: AgentActivity[] = []) => activeProgressByAgent([], indexProgress(activities, [], items, []), "channel", "thread");

test("terminal failure remains visible with a reason after the settle window and after reload", () => {
  const [card] = progress([work("failed")], [terminal]);
  assert.equal(card.state, "failed");
  assert.equal(workItemFailure(card.workItem!, card.history), "Provider connection closed");
  const [reloaded] = progress([work("failed", { run_id: null, failure_detail: terminal.detail })]);
  assert.equal(reloaded.state, "failed");
  assert.equal(workItemFailure(reloaded.workItem!), "Provider connection closed");
});

test("retry replaces its failed card with the original-surface queued request", () => {
  const cards = progress([work("old", { retry_work_item_id: "retry" }), work("retry", { status: "queued", run_id: null })], [terminal]);
  assert.equal(cards.length, 1);
  assert.equal(cards[0].state, "queued");
  assert.equal(cards[0].queuedItems[0].id, "retry");
});

test("failure and stopping work on different agents have independent cards", () => {
  const cards = progress([work("failure"), work("stopping", { agent_id: "other", agent_handle: "Other", run_id: "other-run", status: "cancelling" })]);
  assert.deepEqual(cards.map((card) => [card.workItem?.id, card.state]).sort(), [["failure", "failed"], ["stopping", "stopping"]]);
  assert.equal(activeProgressByAgent([], indexProgress([], [], [work("failure")], []), "channel", "another-thread").length, 0);
});

test("new traffic retains old unresolved failures and active work beyond recent history", () => {
  const newest = Array.from({ length: 81 }, (_, i) => work(`new-${i}`, { status: "done", created_at: "2026-09-07T00:00:00Z" }));
  const failure = work("failure");
  const older = work("older-failure", { created_at: "2026-08-01T00:00:00Z" });
  const active = work("active", { status: "running" });
  const result = retainWorkItems([...newest, failure, older, active]);
  assert.equal(result.length, 82);
  assert.ok(result.includes(failure));
  assert.ok(result.includes(active));
  assert.ok(!result.includes(older));
});

test("structured errors select human-readable nested reasons without exposing metadata objects", () => {
  assert.equal(errorDetail({ error: { message: "Rate limit reached", request_id: "internal" } }), "Rate limit reached");
  assert.equal(errorDetail('{"reason":"Connection timed out","pid":123}'), "Connection timed out");
  assert.equal(errorDetail({ run_id: "internal", pid: 123 }), "");
});
