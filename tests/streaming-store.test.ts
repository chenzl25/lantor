import assert from "node:assert/strict";
import { test } from "node:test";
import { streamedMessage, StreamingMessageStore } from "../src/streaming-message-store";
import { MessageReferenceStore } from "../src/message-reference-store";
import type { Channel, Message } from "../src/types";
const message = (id: string, body = "", delivery_state = "streaming") => ({ id, body, delivery_state, channel_id: "channel", sender_name: "Agent", created_at: "2026-01-01T00:00:00Z", thread_root_id: null }) as Message;

test("stream subscriptions notify only the changed id and retain identical snapshots", () => {
  const store = new StreamingMessageStore();
  let first = 0, second = 0;
  const stop = store.subscribe("a", () => first++);
  store.subscribe("b", () => second++);
  store.publish("a", { body: "🦊", delivery_state: "streaming" });
  const snapshot = store.get("a");
  store.publish("a", { body: "🦊", delivery_state: "streaming" });
  assert.equal(store.get("a"), snapshot);
  assert.equal(first, 1); assert.equal(second, 0);
  stop();
  store.publish("a", { body: "🦊🧪", delivery_state: "streaming" });
  assert.equal(first, 1);
  store.clear(); assert.equal(store.get("a"), undefined);
});

test("stale bootstrap prefixes cannot truncate a stream; final upserts and removal clear overlays", () => {
  const store = new StreamingMessageStore();
  const base = message("a", "old");
  store.publish("a", { body: "old and newer text", delivery_state: "streaming" });
  store.reconcile([base]);
  assert.equal(store.body("a", base.body), "old and newer text");
  assert.equal(streamedMessage(base, store.get("a")).body, "old and newer text");
  const newer = message("a", "old and newer text from server");
  assert.equal(streamedMessage(newer, store.get("a")), newer);
  store.reconcile([newer]);
  assert.equal(store.body("a", "old"), newer.body);
  const final = message("a", "Corrected final", "complete");
  assert.equal(streamedMessage(final, store.get("a")), final);
  store.reconcile([final]); assert.equal(store.get("a"), undefined);
  store.publish("b", { body: "pending", delivery_state: "streaming" });
  store.reconcile([]); assert.equal(store.get("b"), undefined);
});

test("terminal deltas show accumulated text until the master state acknowledges them", () => {
  const store = new StreamingMessageStore();
  const base = message("a", "prefix");
  store.publish("a", { body: "prefix plus full response", delivery_state: "error" });
  const final = streamedMessage(base, store.get("a"));
  assert.equal(final.delivery_state, "error");
  assert.equal(final.body, "prefix plus full response");
  store.reconcile([base]); assert.equal(store.get("a")?.delivery_state, "error");
  store.reconcile([final]); assert.equal(store.get("a"), undefined);
});

test("live stream reference chips invalidate by target, not by unrelated message or channel counters", () => {
  const channel = { id: "channel", name: "test", unread_count: 0 } as Channel;
  const target = message("target", "preview", "complete");
  const unrelated = message("other", "unrelated", "complete");
  const reference = { kind: "thread" as const, id: target.id, token: "[[thread:target]]" };
  const store = new MessageReferenceStore([target, unrelated], [channel]);
  const first = store.get(reference);
  let changes = 0;
  const stop = store.subscribe(reference, () => changes++);
  store.update([structuredClone(target), { ...unrelated, body: "edited" }], [{ ...channel, unread_count: 5 }]);
  assert.equal(store.get(reference), first); assert.equal(changes, 0);
  store.update([{ ...target, body: "edited preview" }, { ...unrelated, thread_root_id: target.id }], [{ ...channel, name: "renamed" }]);
  assert.equal(changes, 1);
  assert.equal(store.get(reference).message?.body, "edited preview");
  assert.equal(store.get(reference).channel?.name, "renamed");
  assert.equal(store.get(reference).replyCount, 1);
  store.update([], [channel]); assert.equal(store.get(reference).message, null);
  stop();
});
