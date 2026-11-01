import assert from "node:assert/strict";
import test from "node:test";

import { apiInvoke, sendMessage } from "../src/apiClient";

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

type DesktopCall = {
  command: string;
  body: Uint8Array;
  headers: Record<string, string>;
};

/** Runs `run` as the desktop window, answering `api_request` with `reply`. */
async function asDesktop(
  reply: (call: DesktopCall) => unknown,
  run: () => Promise<void>,
): Promise<DesktopCall[]> {
  const calls: DesktopCall[] = [];
  const scope = globalThis as { window?: unknown };
  const originalWindow = scope.window;
  scope.window = {
    __TAURI_INTERNALS__: {
      invoke: async (
        command: string,
        body: Uint8Array,
        options?: { headers?: Record<string, string> },
      ) => {
        const call = { command, body, headers: options?.headers ?? {} };
        calls.push(call);
        return reply(call);
      },
    },
  };
  try {
    await run();
  } finally {
    scope.window = originalWindow;
  }
  return calls;
}

function jsonBytes(value: unknown): ArrayBuffer {
  return new TextEncoder().encode(JSON.stringify(value)).buffer as ArrayBuffer;
}

test("desktop API calls go through the shared routes over IPC", async () => {
  let result: unknown;
  const calls = await asDesktop(
    () => jsonBytes({ cursor: 7, replayGap: false, events: [] }),
    async () => {
      result = await apiInvoke("replay_ui_events", { cursor: 3 });
    },
  );

  assert.deepEqual(result, { cursor: 7, replayGap: false, events: [] });
  assert.equal(calls.length, 1);
  assert.equal(calls[0].command, "api_request");
  assert.deepEqual(calls[0].headers, {
    "lantor-method": "POST",
    "lantor-path": "/api/replay_ui_events",
    "lantor-content-type": "application/json",
  });
  assert.deepEqual(JSON.parse(new TextDecoder().decode(calls[0].body)), { cursor: 3 });
});

test("desktop bootstrap is a GET with its query", async () => {
  const calls = await asDesktop(
    () => jsonBytes({}),
    async () => {
      await apiInvoke("bootstrap", { currentChannelOnly: true, channelId: "c1" });
    },
  );

  assert.equal(calls[0].headers["lantor-method"], "GET");
  assert.equal(calls[0].headers["lantor-path"], "/api/bootstrap?currentChannelOnly=true&channelId=c1");
  assert.equal(calls[0].body.length, 0);
});

test("desktop attachments are sent as multipart form data", async () => {
  const calls = await asDesktop(
    () => jsonBytes({}),
    async () => {
      const file = new File([new Uint8Array([0, 1, 254, 255])], "probe.bin", {
        type: "application/octet-stream",
      });
      await sendMessage({
        channelId: "00000000-0000-0000-0000-000000000001",
        threadRootId: null,
        body: "hello",
        asTask: false,
      }, [file]);
    },
  );

  assert.equal(calls[0].headers["lantor-path"], "/api/send_message");
  const contentType = calls[0].headers["lantor-content-type"];
  assert.match(contentType, /^multipart\/form-data; boundary=/);
  const formData = await new Response(calls[0].body, {
    headers: { "content-type": contentType },
  }).formData();
  assert.equal(JSON.parse(String(formData.get("request"))).body, "hello");
  const attachment = formData.get("attachments");
  assert.ok(attachment instanceof File);
  assert.equal(attachment.name, "probe.bin");
  assert.deepEqual([...new Uint8Array(await attachment.arrayBuffer())], [0, 1, 254, 255]);
});

test("desktop API errors reject with the backend message", async () => {
  await asDesktop(
    () => {
      throw "task does not exist";
    },
    async () => {
      await assert.rejects(
        apiInvoke("update_task_title", { taskId: "t1", title: "renamed" }),
        (error: unknown) => error instanceof Error && error.message === "task does not exist",
      );
    },
  );
});

test("desktop API responses also arrive as a byte array", async () => {
  let result: unknown;
  await asDesktop(
    () => [...new Uint8Array(jsonBytes({ cursor: 9, replayGap: false, events: [] }))],
    async () => {
      result = await apiInvoke("replay_ui_events", { cursor: 0 });
    },
  );
  assert.deepEqual(result, { cursor: 9, replayGap: false, events: [] });
});
