import assert from "node:assert/strict";
import test from "node:test";
import { parseReadingPositions } from "../src/channel-reading-position";

const position = { anchor: { messageId: "message-a", seq: 123, offset: -42.5 }, atBottom: false, latestRootId: "message-z" };
test("reading anchors round-trip without retaining arbitrary stored data", () => {
  assert.deepEqual([...parseReadingPositions(JSON.stringify([["channel", { ...position, body: "discard" }]]))], [["channel", position]]);
});
test("corrupt, oversized and invalid reading state is ignored", () => {
  for (const raw of [null, "broken", "{}", " ".repeat(65537), JSON.stringify([["channel", { ...position, anchor: { ...position.anchor, seq: -1 } }]]),
    JSON.stringify([["channel", { ...position, anchor: { ...position.anchor, seq: 0 } }]]),
    JSON.stringify([["channel", { ...position, anchor: { ...position.anchor, offset: "42" } }]])]) {
    assert.equal(parseReadingPositions(raw).size, 0);
  }
  assert.equal(parseReadingPositions(JSON.stringify(Array.from({ length: 140 }, (_, i) => [String(i), position]))).size, 128);
});
