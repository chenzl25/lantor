import assert from "node:assert/strict";
import test from "node:test";
import { splitTitle } from "../src/activity-feed";
import { apiErrorMessage, apiInvoke } from "../src/apiClient";
import { isServerUnreachable, serverUnreachableMessage, subscribeServerReachability } from "../src/web-reachability";
import { checkWebSession, isWebSessionExpired, sessionFetch } from "../src/web-session";

const tunnelPage = () => new Response("<!DOCTYPE html><html><body>error code: 1033</body></html>",
  { status: 530, headers: { "content-type": "text/html" } });

test("a sleeping Mac reads as out of reach, never as the proxy's error page", async (t) => {
  const originalFetch = globalThis.fetch;
  const originalNow = Date.now;
  let now = 100_000;
  Date.now = () => now;
  const online = Object.getOwnPropertyDescriptor(globalThis.navigator, "onLine");
  t.after(() => {
    globalThis.fetch = originalFetch;
    Date.now = originalNow;
    if (online) Object.defineProperty(globalThis.navigator, "onLine", online);
    else delete (globalThis.navigator as { onLine?: boolean }).onLine;
  });
  let respond: () => Promise<Response> = async () => Response.json({ ok: true });
  globalThis.fetch = (async () => respond()) as typeof fetch;
  const states: boolean[] = [];
  t.after(subscribeServerReachability(() => states.push(isServerUnreachable())));

  respond = async () => tunnelPage();
  await assert.rejects(apiInvoke("bootstrap"), (error: Error) => error.message === serverUnreachableMessage);
  assert.equal(isServerUnreachable(), true);
  assert.equal(isWebSessionExpired(), false, "an outage is not a sign-in problem");

  // Lantor answering at all, even with its own JSON error, means the Mac is back.
  respond = async () => Response.json({ message: "no such channel" }, { status: 404 });
  await assert.rejects(apiInvoke("bootstrap"), /no such channel/);
  assert.equal(isServerUnreachable(), false);

  respond = async () => { throw new TypeError("Load failed"); };
  await assert.rejects(sessionFetch("/api/probe"), (error: Error) => error.message === serverUnreachableMessage);
  assert.equal(isServerUnreachable(), true);
  respond = async () => Response.json({ ok: true });
  await sessionFetch("/api/probe");
  assert.equal(isServerUnreachable(), false);

  // The status bar's health probe finds both the outage and the recovery.
  for (const [response, unreachable] of [
    [async () => tunnelPage(), true],
    [async () => Response.json({ ok: true }), false],
    [async () => { throw new TypeError("Load failed"); }, true],
    [async () => Response.json({ ok: true }), false],
  ] as const) {
    now += 6_000;
    respond = response;
    await checkWebSession();
    assert.equal(isServerUnreachable(), unreachable);
  }
  assert.deepEqual(states, [true, false, true, false, true, false, true, false]);

  // A cancelled request says nothing about the Mac.
  respond = async () => { throw new DOMException("aborted", "AbortError"); };
  await assert.rejects(sessionFetch("/api/probe"), { name: "AbortError" });
  assert.equal(isServerUnreachable(), false);

  // With the phone itself offline, the browser's own banner explains it.
  Object.defineProperty(globalThis.navigator, "onLine", { configurable: true, get: () => false });
  respond = async () => { throw new TypeError("Load failed"); };
  await assert.rejects(sessionFetch("/api/probe"), /You’re offline/);
  assert.equal(isServerUnreachable(), false);
});

test("API errors show the server's message but never markup", () => {
  assert.equal(apiErrorMessage({ message: "Channel not found" }, 404, "load failed"), "Channel not found");
  assert.equal(apiErrorMessage("Agent is busy", 409, "send failed"), "Agent is busy");
  assert.equal(apiErrorMessage("<html><body>Forbidden</body></html>", 403, "send_message failed"), "send_message failed (HTTP 403)");
  assert.equal(apiErrorMessage("x".repeat(400), 500, "bootstrap failed"), "bootstrap failed (HTTP 500)");
  assert.equal(apiErrorMessage("", 500, "bootstrap failed"), "bootstrap failed (HTTP 500)");
});

test("an Activity row's preview continues after its title", () => {
  assert.deepEqual(splitTitle("Decision needed: ship it?\nTests pass.\nOne open question."),
    { title: "Decision needed: ship it?", excerpt: "Tests pass.\nOne open question." });
  assert.deepEqual(splitTitle("  One line only  "), { title: "One line only", excerpt: "" });
});
