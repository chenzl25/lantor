import assert from "node:assert/strict";
import test from "node:test";
import { mergeHydratedRows, mergeThreadActivities } from "../src/bootstrap-hydration";

test("lazy history fills summaries without undoing live edits or deletes", () => {
  const summary = { id: "summary", body: "" };
  const edited = { id: "edited", body: "before" };
  const deleted = { id: "deleted", body: "before" };
  const live = { id: "edited", body: "new SSE body" };
  const arrived = { id: "arrived", body: "new SSE row" };
  const full = { id: "summary", body: "detail" };
  const history = { id: "history", body: "older reply" };
  const result = mergeHydratedRows([summary, live, arrived], [full, edited, deleted, history],
    new Map([summary, edited, deleted].map((row) => [row.id, row])));
  assert.deepEqual(result, [full, live, arrived, history]);
  assert.equal(result[1], live);
  assert.equal(result[2], arrived);
});

test("page thread counts cannot overwrite a newer receipt or resurrect removed metadata", () => {
  const old = { thread_root_id: "root", reply_count: 4, unread_count: 4 };
  const removed = { thread_root_id: "removed", reply_count: 1, unread_count: 1 };
  const read = { ...old, unread_count: 0 };
  const page = { thread_root_id: "older", reply_count: 8, unread_count: 0 };
  assert.deepEqual(mergeThreadActivities([read], [old, removed, page], new Map([old, removed].map(row => [row.thread_root_id, row]))), [read, page]);
});
