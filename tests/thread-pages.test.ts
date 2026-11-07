import assert from "node:assert/strict";
import test from "node:test";
import { THREAD_PAGE_REPLIES, mergeThreadReplyStart, repliesInLoadedRun, threadPageRequest } from "../src/thread-pages";
import type { Message } from "../src/types";

const reply = (seq: number, id = `r${seq}`) => ({ id, seq, thread_root_id: "root" }) as Message;
const more = (seq: number) => ({ has_more: true, next_before_seq: seq });
const complete = { has_more: false, next_before_seq: null };

test("the loaded run only grows back toward the first reply", () => {
  assert.equal(mergeThreadReplyStart(undefined, more(70)), 70);
  assert.equal(mergeThreadReplyStart(70, more(20)), 20, "an earlier page extends the run");
  assert.equal(mergeThreadReplyStart(20, more(70)), 20, "a reload of the newest page never shortens it");
  assert.equal(mergeThreadReplyStart(20, complete), null, "no earlier reply means the whole thread is loaded");
  assert.equal(mergeThreadReplyStart(null, more(70)), null);
});

test("a first open asks for the newest page; later loads keep the whole run", () => {
  assert.deepEqual(threadPageRequest("root", undefined), { threadRootId: "root", limit: THREAD_PAGE_REPLIES });
  assert.deepEqual(threadPageRequest("root", 20), { threadRootId: "root", sinceSeq: 20, limit: THREAD_PAGE_REPLIES });
  assert.deepEqual(threadPageRequest("root", null), { threadRootId: "root", sinceSeq: 0, limit: THREAD_PAGE_REPLIES });
});

test("older replies already in memory stay hidden until their page loads", () => {
  const replies = [reply(5), reply(60), reply(70), reply(80), reply(0, "unsent")];
  assert.deepEqual(repliesInLoadedRun(replies, undefined).map((m) => m.id), ["r5", "r60", "r70", "r80", "unsent"]);
  assert.deepEqual(repliesInLoadedRun(replies, null).map((m) => m.id), ["r5", "r60", "r70", "r80", "unsent"]);
  assert.deepEqual(repliesInLoadedRun(replies, 70).map((m) => m.id), ["r70", "r80", "unsent"]);
  assert.deepEqual(repliesInLoadedRun(replies, 70, "r5").map((m) => m.id), ["r5", "r70", "r80", "unsent"], "a jump target stays visible");
});
