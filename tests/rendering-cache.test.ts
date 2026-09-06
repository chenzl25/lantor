import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { cachedIdenticon, createDiceBearCache, diceBearAvatarCache, parseDiceBearAvatar } from "../src/avatar-rendering";
import { AgentAvatar } from "../src/components/AgentAvatar";
import { MarkdownRenderer } from "../src/components/MarkdownRenderer";
import { shouldCollapseMessage, DESKTOP_MESSAGE_PREVIEW_CHARS, DESKTOP_MESSAGE_PREVIEW_LINES } from "../src/message-preview";
import { agentForMessageSender, formatClockTime, formatDateDivider, formatRelativeTime, formatTime } from "../src/ui-utils";
import type { Agent, Message } from "../src/types";

test("DiceBear deduplicates in-flight work, caches successes and retries failures", async () => {
  let calls = 0;
  let reject = false;
  const cache = createDiceBearCache(async ({ seed }) => {
    calls += 1;
    if (reject) throw new Error("transient load error");
    return `uri:${seed}`;
  });
  const spec = { style: "dylan", seed: "shared" } as const;
  const first = cache.load(spec);
  assert.equal(cache.load({ ...spec }), first);
  assert.equal(await first, "uri:shared");
  assert.equal(cache.get(spec), "uri:shared");
  await cache.load(spec);
  assert.equal(calls, 1);
  const other = { ...spec, seed: "retry" };
  reject = true;
  await assert.rejects(cache.load(other), /transient/);
  assert.equal(cache.get(other), undefined);
  reject = false;
  assert.equal(await cache.load(other), "uri:retry");
  assert.equal(calls, 3);
});

test("avatar caches are bounded and isolate style/seed keys", async () => {
  let calls = 0;
  const cache = createDiceBearCache(async ({ style, seed }) => `${style}:${seed}:${++calls}`, 2);
  const a = { style: "dylan", seed: "a" } as const;
  const b = { style: "dylan", seed: "b" } as const;
  const c = { style: "initials", seed: "a" } as const;
  await cache.load(a); await cache.load(b);
  cache.get(a); // refresh a's recency
  await cache.load(c);
  assert.equal(cache.get(b), undefined);
  assert.notEqual(cache.get(a), cache.get(c));
  const icon = cachedIdenticon("stable");
  assert.equal(cachedIdenticon("stable"), icon);
  for (let index = 0; index < 1100; index += 1) cachedIdenticon(`evict-${index}`);
  assert.notEqual(cachedIdenticon("stable"), icon);
  assert.deepEqual(cachedIdenticon("stable"), icon, "eviction must not change pixels/colors");
});

test("normalized DiceBear specs and warm first render preserve avatar identity", async () => {
  assert.deepEqual(parseDiceBearAvatar("dicebear:BotttsNeutral:a:b", "fallback"), { style: "bottts-neutral", seed: "a:b" });
  assert.deepEqual(parseDiceBearAvatar("dicebear:__proto__:", "fallback"), { style: "dylan", seed: "fallback" });
  assert.equal(parseDiceBearAvatar("😀", "fallback"), null);
  const spec = { style: "dylan", seed: "warm-avatar-test" } as const;
  const uri = await diceBearAvatarCache.load(spec);
  const agent = { id: "warm", handle: "test", display_name: "Test", status: "idle", avatar: `dicebear:${spec.style}:${spec.seed}` };
  const firstRender = renderToStaticMarkup(createElement(AgentAvatar, { agent }));
  assert.ok(firstRender.includes(uri));
  assert.ok(!firstRender.includes("agent-avatar-pixels"), "cached image must be present before effects run");
});

test("formatters reuse locale/options instances without caching relative labels", (t) => {
  const now = new Date(2026, 8, 6, 12);
  t.mock.timers.enable({ apis: ["Date"], now });
  const Original = Intl.DateTimeFormat;
  let constructions = 0;
  Intl.DateTimeFormat = new Proxy(Original, {
    construct(target, args) { constructions += 1; return Reflect.construct(target, args); },
  });
  try {
    const today = now.toISOString();
    const yesterday = new Date(2026, 8, 5, 12).toISOString();
    const old = new Date(2025, 0, 2, 9, 5).toISOString();
    const sameYear = new Date(2026, 0, 2, 9, 5).toISOString();
    const time = new Original("en", { hour: "2-digit", minute: "2-digit" }).format(now);
    assert.equal(formatTime(today), `Today ${time}`);
    assert.equal(formatTime(yesterday), `Yesterday ${time}`);
    assert.equal(formatTime(old), new Original("en", { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }).format(new Date(old)));
    assert.equal(formatRelativeTime(old), new Original("en", { month: "short", day: "numeric", year: "numeric" }).format(new Date(old)));
    assert.equal(formatRelativeTime(sameYear), new Original("en", { month: "short", day: "numeric" }).format(new Date(sameYear)));
    assert.equal(formatClockTime(old), new Original("en", { hour: "2-digit", minute: "2-digit", hourCycle: "h23" }).format(new Date(old)));
    assert.equal(formatDateDivider(old), new Original("en", { weekday: "long", month: "long", day: "numeric" }).format(new Date(old)));
    assert.equal(constructions, 6);
    for (let index = 0; index < 200; index += 1) {
      formatTime(today); formatTime(old); formatRelativeTime(old); formatRelativeTime(sameYear); formatClockTime(old); formatDateDivider(old);
    }
    assert.equal(constructions, 6, "warm rendering creates no more Intl formatters");
    t.mock.timers.setTime(new Date(2026, 8, 7, 12).getTime());
    assert.equal(formatTime(today), `Yesterday ${time}`);
    assert.equal(formatDateDivider(today), "Yesterday");
    for (const format of [formatTime, formatRelativeTime, formatDateDivider]) assert.equal(format("invalid"), "invalid");
    assert.throws(() => formatClockTime("invalid"), RangeError);
  } finally { Intl.DateTimeFormat = Original; }
});

test("collapse scanning preserves the previous trimmed line/character thresholds", () => {
  const bodies = ["", " \n ", "a".repeat(8000), "a".repeat(8001), ...Array.from({ length: 100 }, (_, index) => ` \n${"line\n".repeat(index)}end\n `)];
  for (const body of bodies) {
    const text = body.trim();
    assert.equal(shouldCollapseMessage(body), Boolean(text) && (text.split("\n").length > DESKTOP_MESSAGE_PREVIEW_LINES || text.length > DESKTOP_MESSAGE_PREVIEW_CHARS));
  }
});

test("Map sender lookup preserves owner/system/deleted-agent behavior", () => {
  const agent = { id: "agent" } as Agent;
  const agents = new Map([[agent.id, agent]]);
  const msg = { sender_agent_id: agent.id, sender_role: "agent" } as Message;
  assert.equal(agentForMessageSender(msg, agents), agent);
  for (const sender_role of ["owner", "system"]) assert.equal(agentForMessageSender({ ...msg, sender_role }, agents), null);
  assert.equal(agentForMessageSender({ ...msg, sender_agent_id: null }, agents), null);
  assert.equal(agentForMessageSender({ ...msg, sender_agent_id: "missing" }, agents), null);
});

test("shared mention regexes do not leak lastIndex across segments or renders", () => {
  const props = { body: "@Hancock `@Hancock` @Hancock\n\n#general task #99\n\n```text\n@Hancock\n```" };
  const first = renderToStaticMarkup(createElement(MarkdownRenderer, props));
  for (let index = 0; index < 20; index += 1) assert.equal(renderToStaticMarkup(createElement(MarkdownRenderer, props)), first);
  assert.equal((first.match(/href="\/lantor\/agent\/Hancock"/g) ?? []).length, 2);
  assert.equal((first.match(/href="\/lantor\/channel\/general"/g) ?? []).length, 1);
  assert.equal((first.match(/href="\/lantor\/task\/99"/g) ?? []).length, 1);
});
