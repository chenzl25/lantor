import assert from "node:assert/strict";
import test from "node:test";
import {
  COMPOSER_DRAFTS_STORAGE_KEY,
  MAX_STORED_DRAFTS,
  STORED_DRAFT_MAX_AGE_MS,
  composerDraftChanges,
  loadComposerDrafts,
  saveComposerDrafts,
} from "../src/composer-drafts";

class MemoryStorage implements Storage {
  private values = new Map<string, string>();
  failWrites = false;
  get length() { return this.values.size; }
  clear() { this.values.clear(); }
  getItem(key: string) { return this.values.get(key) ?? null; }
  key(index: number) { return Array.from(this.values.keys())[index] ?? null; }
  removeItem(key: string) { this.values.delete(key); }
  setItem(key: string, value: string) {
    if (this.failWrites) throw new DOMException("quota", "QuotaExceededError");
    this.values.set(key, value);
  }
}

test("drafts come back per channel and per thread", () => {
  const storage = new MemoryStorage();
  saveComposerDrafts(new Map([["channel:c1", "root text"], ["thread:t1", "reply text"]]), storage, 1_000);
  assert.deepEqual(loadComposerDrafts("channel", storage, 2_000), { c1: "root text" });
  assert.deepEqual(loadComposerDrafts("thread", storage, 2_000), { t1: "reply text" });
});

test("an empty draft removes the stored one", () => {
  const storage = new MemoryStorage();
  saveComposerDrafts(new Map([["channel:c1", "sent soon"]]), storage, 1_000);
  saveComposerDrafts(new Map([["channel:c1", ""]]), storage, 2_000);
  assert.deepEqual(loadComposerDrafts("channel", storage, 3_000), {});
  assert.equal(storage.getItem(COMPOSER_DRAFTS_STORAGE_KEY), null);
});

test("a save keeps drafts another tab wrote for other composers", () => {
  const storage = new MemoryStorage();
  saveComposerDrafts(new Map([["thread:t1", "from tab A"]]), storage, 1_000);
  saveComposerDrafts(new Map([["channel:c1", "from tab B"]]), storage, 2_000);
  assert.deepEqual(loadComposerDrafts("thread", storage, 3_000), { t1: "from tab A" });
  assert.deepEqual(loadComposerDrafts("channel", storage, 3_000), { c1: "from tab B" });
});

test("old drafts expire and only the newest are kept", () => {
  const storage = new MemoryStorage();
  saveComposerDrafts(new Map([["channel:old", "stale"]]), storage, 0);
  const now = STORED_DRAFT_MAX_AGE_MS + 1;
  for (let index = 0; index <= MAX_STORED_DRAFTS; index += 1) {
    saveComposerDrafts(new Map([[`thread:t${index}`, `draft ${index}`]]), storage, now + index);
  }
  const threads = loadComposerDrafts("thread", storage, now + MAX_STORED_DRAFTS);
  assert.equal(Object.keys(threads).length, MAX_STORED_DRAFTS);
  assert.equal(threads.t0, undefined, "the oldest draft past the cap is dropped");
  assert.equal(threads[`t${MAX_STORED_DRAFTS}`], `draft ${MAX_STORED_DRAFTS}`);
  assert.deepEqual(loadComposerDrafts("channel", storage, now + MAX_STORED_DRAFTS), {});
});

test("unreadable or full storage does not throw", () => {
  const storage = new MemoryStorage();
  storage.setItem(COMPOSER_DRAFTS_STORAGE_KEY, "{not json");
  assert.deepEqual(loadComposerDrafts("channel", storage, 0), {});
  storage.setItem(COMPOSER_DRAFTS_STORAGE_KEY, JSON.stringify({ "channel:c1": { text: 7, savedAt: 0 }, "channel:c2": { text: "ok", savedAt: 0 } }));
  assert.deepEqual(loadComposerDrafts("channel", storage, 0), { c2: "ok" });
  storage.failWrites = true;
  assert.doesNotThrow(() => saveComposerDrafts(new Map([["channel:c3", "kept in memory"]]), storage, 0));
});

test("only changed drafts are written", () => {
  const previous = new Map([["channel:c1", "same"], ["channel:c2", "before"], ["thread:t1", "gone"]]);
  const next = new Map([["channel:c1", "same"], ["channel:c2", "after"], ["thread:t2", "new"]]);
  assert.deepEqual(Object.fromEntries(composerDraftChanges(previous, next)), {
    "channel:c2": "after",
    "thread:t2": "new",
    "thread:t1": "",
  });
});
