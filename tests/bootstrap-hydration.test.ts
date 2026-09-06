import assert from "node:assert/strict";
import test from "node:test";
import { mergeHydratedRows } from "../src/bootstrap-hydration";

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
