import assert from "node:assert/strict";
import test from "node:test";
import {
  cursorAt,
  findNeedle,
  findOffsets,
  findPosition,
  findTotal,
  foldFindText,
  mergeFindEntries,
  reconcileCursor,
  stepFind,
  type FindEntry,
} from "../src/thread-find";

const shown = (messageId: string, seq: number, count: number): FindEntry => ({ messageId, seq, count, loaded: true });
const hit = (message_id: string, seq: number, matches: number) => ({ message_id, seq, matches });

test("folding lowercases and turns whitespace into spaces without moving offsets", () => {
  assert.equal(foldFindText("RetryBudget\tNEW\nline"), "retrybudget new line");
  for (const text of ["İstanbul", "ẞ straße", "Σίσυφος", "tab\there　wide", "😀 Emoji", "重试预算"]) {
    assert.equal(foldFindText(text).length, text.length, text);
  }
  // Characters whose lowercase is longer keep their case, as the server does.
  assert.equal(foldFindText("İ ẞ Σ\tA"), "İ ß σ a");
});

test("a query matches as typed unless it is only whitespace", () => {
  assert.equal(findNeedle("  "), "");
  assert.equal(findNeedle(""), "");
  assert.equal(findNeedle(" Retry Budget"), " retry budget");
});

test("matches do not overlap", () => {
  assert.deepEqual(findOffsets("aaaa", "aa"), [0, 2]);
  assert.deepEqual(findOffsets(foldFindText("let retry = Retry::new(); // RETRY"), "retry"), [4, 12, 29]);
  assert.deepEqual(findOffsets("anything", ""), []);
});

test("replies that are not loaded come before the loaded run, in seq order", () => {
  const rendered = [shown("root", 1, 1), shown("r50", 50, 2), shown("r60", 60, 0), shown("r70", 70, 1)];
  const entries = mergeFindEntries(rendered, [hit("r20", 20, 3), hit("r10", 10, 1)]);
  assert.deepEqual(entries.map((entry) => [entry.messageId, entry.count, entry.loaded]), [
    ["root", 1, true], ["r10", 1, false], ["r20", 3, false], ["r50", 2, true], ["r70", 1, true],
  ]);
  assert.equal(findTotal(entries), 8);
});

test("a rendered reply is counted from the page, and a reply kept for a jump stays in seq order", () => {
  const rendered = [shown("root", 1, 0), shown("r15", 15, 1), shown("r50", 50, 1)];
  const entries = mergeFindEntries(rendered, [hit("r10", 10, 2), hit("r15", 15, 5), hit("r20", 20, 1)]);
  assert.deepEqual(entries.map((entry) => [entry.messageId, entry.count]), [
    ["r10", 2], ["r15", 1], ["r20", 1], ["r50", 1],
  ]);
});

test("unsent replies keep their place at the end", () => {
  const rendered = [shown("root", 1, 0), shown("r50", 50, 1), shown("unsent", 0, 1)];
  const entries = mergeFindEntries(rendered, [hit("r10", 10, 1)]);
  assert.deepEqual(entries.map((entry) => entry.messageId), ["r10", "r50", "unsent"]);
});

test("next and previous step through every match and wrap around", () => {
  const entries = mergeFindEntries([shown("root", 1, 1), shown("r50", 50, 2)], [hit("r10", 10, 1)]);
  const order: string[] = [];
  let cursor = stepFind(entries, null, 1);
  for (let step = 0; step < 5; step += 1) {
    order.push(`${cursor!.messageId}:${cursor!.index}:${findPosition(entries, cursor)}`);
    cursor = stepFind(entries, cursor, 1);
  }
  assert.deepEqual(order, ["root:0:1", "r10:0:2", "r50:0:3", "r50:1:4", "root:0:1"]);
  assert.deepEqual(stepFind(entries, { messageId: "root", index: 0 }, -1), { messageId: "r50", index: 1 });
  assert.deepEqual(stepFind(entries, null, -1), { messageId: "r50", index: 1 }, "previous with no match selected starts from the end");
  assert.equal(stepFind([], null, 1), null);
  assert.deepEqual(cursorAt(entries, 2), { messageId: "r50", index: 0 });
  assert.equal(cursorAt(entries, 9), null);
});

test("a recount keeps the cursor on its message, or moves on when that message stops matching", () => {
  const before = [shown("a", 1, 3), shown("b", 2, 1), shown("c", 3, 1)];
  assert.deepEqual(reconcileCursor([shown("a", 1, 2), shown("b", 2, 1)], { messageId: "a", index: 2 }, before), { messageId: "a", index: 1 });
  assert.deepEqual(reconcileCursor([shown("a", 1, 3), shown("c", 3, 1)], { messageId: "b", index: 0 }, before), { messageId: "c", index: 0 });
  assert.deepEqual(reconcileCursor([shown("a", 1, 3)], { messageId: "c", index: 0 }, before), { messageId: "a", index: 0 });
  assert.equal(reconcileCursor([], { messageId: "a", index: 0 }, before), null);
  assert.equal(reconcileCursor(before, null, before), null);
});
