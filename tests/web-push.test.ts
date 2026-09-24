import assert from "node:assert/strict";
import test from "node:test";

import { decodeBase64Url, isPushTarget, openTargetHash, parseOpenTargetHash } from "../src/web-push";

test("notification targets round-trip through the cold-start hash", () => {
  const target = { channel_id: "44e8cb28-8e80", thread_root_id: "79be35ec-d39c", message_id: "c46b5f3d-ddfd" };
  assert.deepEqual(parseOpenTargetHash(openTargetHash(target)), target);

  const root = { channel_id: "c/1", thread_root_id: null, message_id: "m 1" };
  assert.equal(openTargetHash(root), "#/open/c%2F1//m%201");
  assert.deepEqual(parseOpenTargetHash(openTargetHash(root)), root);

  assert.equal(parseOpenTargetHash("#/message/abc"), null);
  assert.equal(parseOpenTargetHash("#/open/only-channel"), null);
});

test("service worker messages are validated before navigation", () => {
  assert.ok(isPushTarget({ channel_id: "c", thread_root_id: null, message_id: "m" }));
  assert.ok(!isPushTarget({ channel_id: "c", message_id: 3, thread_root_id: null }));
  assert.ok(!isPushTarget({ channel_id: "c", message_id: "m" }));
  assert.ok(!isPushTarget(null));
});

test("VAPID keys decode from unpadded base64url", () => {
  const key = decodeBase64Url("BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8");
  assert.equal(key.length, 65);
  assert.equal(key[0], 0x04);
  assert.deepEqual([...decodeBase64Url("-_8")], [0xfb, 0xff]);
});
