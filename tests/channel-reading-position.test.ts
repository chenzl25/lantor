import assert from "node:assert/strict";
import test from "node:test";
import { countUnseenRoots, firstUnreadRoot, readChannelPosition, rememberChannelPosition } from "../src/channel-reading-position";
import type { Message } from "../src/types";

const root = (seq: number, sender_role: Message["sender_role"] = "agent") => ({ id: `m${seq}`, seq, sender_role }) as Message;
const roots = [root(10), root(12), root(14, "owner"), root(15), root(18)];

test("a channel opens at the first unread message the reader has not seen", () => {
  assert.equal(firstUnreadRoot(roots, null), null);
  assert.equal(firstUnreadRoot(roots, 12)?.id, "m12");
  assert.equal(firstUnreadRoot(roots, 11)?.id, "m12", "a missing seq resolves to the next message");
  assert.equal(firstUnreadRoot(roots, 13)?.id, "m15", "the reader's own message is never the first unread");
  assert.equal(firstUnreadRoot(roots, 12, 15)?.id, "m18", "messages this page showed stay read");
  assert.equal(firstUnreadRoot(roots, 12, 18), null);
});

test("the back-to-bottom count includes only unseen messages from others", () => {
  assert.equal(countUnseenRoots(roots, 18), 0);
  assert.equal(countUnseenRoots(roots, 12), 2);
  assert.equal(countUnseenRoots(roots, 0), 4);
});

test("reading positions last for the page session and skip unsent messages", () => {
  const position = { anchor: { messageId: "m12", seq: 12, offset: -4 }, atBottom: false, latestRootId: "m18", seenSeq: 18 };
  rememberChannelPosition("channel", position);
  assert.deepEqual(readChannelPosition("channel"), position);
  rememberChannelPosition("channel", { ...position, anchor: { ...position.anchor, seq: 0 } });
  assert.deepEqual(readChannelPosition("channel"), position);
});
