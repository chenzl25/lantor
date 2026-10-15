import assert from "node:assert/strict";
import test from "node:test";

import { sendMessage } from "../src/apiClient";

test("web attachment sends raw bytes as multipart form data", async () => {
  const originalFetch = globalThis.fetch;
  let requestUrl = "";
  let requestInit: RequestInit | undefined;
  globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
    requestUrl = String(input);
    requestInit = init;
    return new Response("{}", {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  }) as typeof fetch;

  try {
    const file = new File([new Uint8Array([0, 1, 254, 255])], "probe.bin", {
      type: "application/octet-stream",
    });
    await sendMessage({
      channelId: "00000000-0000-0000-0000-000000000001",
      threadRootId: null,
      body: "hello",
      asTask: false,
    }, [file]);

    assert.equal(requestUrl, "/api/send_message");
    assert.equal(requestInit?.method, "POST");
    assert.equal(new Headers(requestInit?.headers).get("X-Requested-With"), "XMLHttpRequest");
    assert.equal(new Headers(requestInit?.headers).get("content-type"), null, "browser supplies the multipart boundary");
    assert.ok(requestInit?.body instanceof FormData);
    const formData = requestInit.body;
    assert.deepEqual(JSON.parse(String(formData.get("request"))), {
      channelId: "00000000-0000-0000-0000-000000000001",
      threadRootId: null,
      body: "hello",
      asTask: false,
    });
    const attachment = formData.get("attachments");
    assert.ok(attachment instanceof File);
    assert.equal(attachment.name, "probe.bin");
    assert.equal(attachment.type, "application/octet-stream");
    assert.deepEqual(
      [...new Uint8Array(await attachment.arrayBuffer())],
      [0, 1, 254, 255],
    );
  } finally {
    globalThis.fetch = originalFetch;
  }
});
