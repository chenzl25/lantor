import assert from "node:assert/strict";
import test from "node:test";
import { apiInvoke, apiInvokeMeasured, sendMessage } from "../src/apiClient";
import { checkWebSession, isWebSessionExpired, sessionFetch, subscribeWebSession } from "../src/web-session";

test("proxy expiry pauses requests, preserves mutations, and recovers only on a valid health response", async (t) => {
  const originalFetch = globalThis.fetch;
  const originalNow = Date.now;
  let now = 100_000;
  Date.now = () => now;
  t.after(() => { globalThis.fetch = originalFetch; Date.now = originalNow; });
  const requests: { url: string; init?: RequestInit }[] = [];
  let respond: () => Promise<Response> = async () => Response.json({ ok: true });
  globalThis.fetch = (async (url: string, init?: RequestInit) => {
    requests.push({ url, init });
    return respond();
  }) as typeof fetch;
  const states: boolean[] = [];
  const unsubscribe = subscribeWebSession(() => states.push(isWebSessionExpired()));
  t.after(unsubscribe);

  await apiInvoke("bootstrap");
  await apiInvokeMeasured("bootstrap");
  await apiInvoke("replay_ui_events", { cursor: 3 });
  await apiInvokeMeasured("replay_ui_events", { cursor: 3 });
  for (const request of requests) {
    const headers = new Headers(request.init?.headers);
    assert.equal(headers.get("X-Requested-With"), "XMLHttpRequest");
    if (request.init?.method === "POST") assert.equal(headers.get("content-type"), "application/json");
  }

  // An old successful probe must not clear a newer API authentication failure.
  let finishProbe: (response: Response) => void = () => {};
  respond = () => new Promise((resolve) => { finishProbe = resolve; });
  const oldProbe = checkWebSession();
  assert.strictEqual(checkWebSession(), oldProbe, "concurrent probes are shared");
  respond = async () => new Response("sign in", { status: 401 });
  await assert.rejects(apiInvoke("bootstrap"), /session has expired/);
  finishProbe(Response.json({ ok: true }));
  await oldProbe;
  assert.equal(isWebSessionExpired(), true);
  const beforeMutation = requests.length;
  await assert.rejects(sendMessage({ channelId: "channel", body: "unsent", asTask: false }, [new File(["bytes"], "draft.txt")]), /session has expired/);
  assert.equal(requests.length, beforeMutation, "expired uploads are not sent or retried");

  for (const response of [
    new Response("login HTML", { headers: { "content-type": "text/html" } }),
    Response.json({ ok: false }),
    new Response("forbidden", { status: 403 }),
    new Response("unavailable", { status: 503 }),
  ]) {
    now += 6_000;
    respond = async () => response;
    await checkWebSession();
    assert.equal(isWebSessionExpired(), true, "only authenticated Lantor health clears expiry");
  }
  now += 6_000;
  respond = async () => { throw new TypeError("Failed to fetch"); };
  await checkWebSession();
  assert.equal(isWebSessionExpired(), true);

  now += 6_000;
  respond = async () => Response.json({ ok: true });
  await checkWebSession();
  assert.equal(isWebSessionExpired(), false);
  assert.deepEqual(states, [true, false]);
  assert.equal(requests.at(-1)?.url, "/api/health");
  assert.equal(requests.at(-1)?.init?.cache, "no-store");

  // Offline/CORS/server/permission failures never trigger a login loop.
  for (const status of [403, 502, 503]) {
    respond = async () => new Response("error", { status });
    assert.equal((await sessionFetch("/api/probe")).status, status);
    now += 6_000;
    await checkWebSession();
    assert.equal(isWebSessionExpired(), false);
  }
  respond = async () => { throw new TypeError("Failed to fetch"); };
  await assert.rejects(sessionFetch("/api/probe"), /Failed to fetch/);
  now += 6_000;
  await checkWebSession();
  assert.equal(isWebSessionExpired(), false);

  now += 6_000;
  respond = async () => new Response("sign in", { status: 401 });
  await checkWebSession();
  assert.equal(isWebSessionExpired(), true, "SSE's health probe can discover expiry");
  const beforeThrottle = requests.length;
  await checkWebSession();
  assert.equal(requests.length, beforeThrottle);
  now += 6_000;
  respond = async () => Response.json({ ok: true });
  await checkWebSession();
  assert.deepEqual(states, [true, false, true, false]);
});
