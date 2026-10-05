import assert from "node:assert/strict";
import test from "node:test";
import { pairSentImagePreviews } from "../src/sent-image-previews";
import type { MessageAttachment } from "../src/types";

const attachment = (id: string, original_name: string, size_bytes: number, mime_type: string, local_url?: string): MessageAttachment => ({
  id,
  message_id: "message",
  original_name,
  mime_type,
  size_bytes,
  storage_path: local_url ? "" : `/attachments/${id}`,
  created_at: "2026-10-05T00:00:00Z",
  ...(local_url ? { local_url } : {}),
});

test("sent images pair with their local copies by name and size, regardless of order", () => {
  const local = [
    attachment("local-1", "shot.png", 100, "image/png", "blob:1"),
    attachment("local-2", "shot.png", 200, "image/png", "blob:2"),
    attachment("local-3", "notes.txt", 50, "text/plain", "blob:3"),
  ];
  const persisted = [
    attachment("server-2", "shot.png", 200, "image/png"),
    attachment("server-3", "notes.txt", 50, "text/plain"),
    attachment("server-1", "shot.png", 100, "image/png"),
  ];
  const { pairs, unpaired } = pairSentImagePreviews(local, persisted);
  assert.deepEqual(
    pairs.map(({ attachment, objectUrl }) => [attachment.id, objectUrl]),
    [["server-2", "blob:2"], ["server-1", "blob:1"]],
  );
  // Non-image files switch to the server URL at once.
  assert.deepEqual(unpaired, ["blob:3"]);
});

test("identical sent images each keep their own local copy", () => {
  const local = [
    attachment("local-1", "same.png", 100, "image/png", "blob:1"),
    attachment("local-2", "same.png", 100, "image/png", "blob:2"),
  ];
  const persisted = [attachment("server-1", "same.png", 100, "image/png"), attachment("server-2", "same.png", 100, "image/png")];
  const { pairs, unpaired } = pairSentImagePreviews(local, persisted);
  assert.deepEqual(pairs.map(({ objectUrl }) => objectUrl), ["blob:1", "blob:2"]);
  assert.deepEqual(unpaired, []);
});

test("a failed send or a persisted row without attachments releases every local copy", () => {
  const local = [attachment("local-1", "shot.png", 100, "image/png", "blob:1")];
  assert.deepEqual(pairSentImagePreviews(local, []), { pairs: [], unpaired: ["blob:1"] });
  const renamed = [attachment("server-1", "other.png", 100, "image/png")];
  assert.deepEqual(pairSentImagePreviews(local, renamed).unpaired, ["blob:1"]);
});
