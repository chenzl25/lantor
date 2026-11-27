import assert from "node:assert/strict";
import test from "node:test";
import {
  RESUME_CACHE_MAX_AGE_MS, RESUME_RECENT_LIMIT, RESUME_VIEW_MAX_AGE_MS, rememberRecent, resumeSnapshotUse, workspaceForResume,
} from "../src/resume-cache";
import type { Bootstrap, Message } from "../src/types";

const message = (id: string, seq: number, channel_id: string, extra: Partial<Message> = {}): Message => ({
  id, seq, channel_id, thread_root_id: null, sender_agent_id: null, sender_name: "Dylan", sender_role: "owner",
  body: `message ${id}`, is_task: false, thread_followed: false, delivery_state: "complete", stream_key: "",
  task_number: null, task_status: null, attachments: [], artifacts: [], created_at: "", updated_at: "", ...extra,
} as Message);

const workspace = (messages: Message[]) => ({ channels: [], messages, ui_event_cursor: 9, __perf: { total_ms: 1 } } as unknown as Bootstrap);

test("recent channels and threads are most recent first, without repeats, capped", () => {
  let list: string[] = [];
  for (const id of ["a", "b", "a", "c"]) list = rememberRecent(list, id);
  assert.deepEqual(list, ["c", "a", "b"]);
  assert.deepEqual(rememberRecent(list, null), list);
  for (let index = 0; index < 20; index += 1) list = rememberRecent(list, `x${index}`);
  assert.equal(list.length, RESUME_RECENT_LIMIT);
});

test("only recent channels' newest roots and recent threads' newest replies are kept", () => {
  const roots = Array.from({ length: 40 }, (_, index) => message(`r${index}`, index + 1, "c1"));
  const replies = Array.from({ length: 60 }, (_, index) => message(`p${index}`, 100 + index, "c1", { thread_root_id: "r0" }));
  const elsewhere = message("o1", 500, "c2");
  const otherThread = message("q1", 501, "c1", { thread_root_id: "r5" });
  const kept = workspaceForResume(workspace([...roots, ...replies, elsewhere, otherThread]), { channelIds: ["c1"], threadIds: ["r0"] });
  const ids = new Set(kept.messages.map((row) => row.id));
  assert.equal([...ids].filter((id) => id.startsWith("r")).length, 31, "the newest 30 roots, plus the open thread's old root");
  assert.ok(ids.has("r39") && ids.has("r10") && !ids.has("r9") && ids.has("r0"));
  assert.equal([...ids].filter((id) => id.startsWith("p")).length, 50, "the newest 50 replies");
  assert.ok(ids.has("p59") && !ids.has("p9"));
  assert.ok(!ids.has("o1"), "a channel not opened lately is not kept");
  assert.ok(!ids.has("q1"), "a thread not opened lately is not kept");
  assert.equal(kept.ui_event_cursor, 9);
  assert.equal("__perf" in kept, false);
});

test("unsent messages and local file previews never leave the page", () => {
  const attachment = { id: "f", message_id: "m2", original_name: "a.png", mime_type: "image/png", size_bytes: 1, storage_path: "a.png", local_url: "blob:x", created_at: "" };
  const kept = workspaceForResume(workspace([
    message("m1", 0, "c1", { delivery_state: "sending" }),
    message("m2", 3, "c1", { attachments: [attachment] }),
  ]), { channelIds: ["c1"], threadIds: [] });
  assert.deepEqual(kept.messages.map((row) => row.id), ["m2"]);
  assert.equal("local_url" in kept.messages[0].attachments[0], false);
});

test("a copy is shown for a week by the build that wrote it, and reopens its view for an hour", () => {
  const now = 10 * RESUME_CACHE_MAX_AGE_MS;
  assert.deepEqual(resumeSnapshotUse({ version: "v1", savedAt: now - 60_000 }, "v1", now), { usable: true, reopenView: true });
  assert.deepEqual(resumeSnapshotUse({ version: "v1", savedAt: now - RESUME_VIEW_MAX_AGE_MS - 1 }, "v1", now), { usable: true, reopenView: false });
  assert.deepEqual(resumeSnapshotUse({ version: "v1", savedAt: now - RESUME_CACHE_MAX_AGE_MS - 1 }, "v1", now), { usable: false, reopenView: false });
  assert.deepEqual(resumeSnapshotUse({ version: "v0", savedAt: now }, "v1", now), { usable: false, reopenView: false });
  assert.deepEqual(resumeSnapshotUse({ version: "v1", savedAt: now + 1 }, "v1", now), { usable: false, reopenView: false }, "a copy from the future is not trusted");
});
