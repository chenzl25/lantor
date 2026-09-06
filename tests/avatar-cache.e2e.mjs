import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { build, preview } from "vite";
import react from "@vitejs/plugin-react";
import { chromium } from "playwright";

const directory = await mkdtemp(join(tmpdir(), "lantor-avatar-test-"));
let server;
let browser;
try {
  // Test-only counters plus React's production profiling build. The app bundle
  // is not instrumented or overwritten; all fixture data is synthetic.
  const instrumentation = {
    name: "avatar-cache-counters", enforce: "pre",
    transform(source, id) {
      const probes = id.endsWith("/avatar-rendering.ts") ? [
        ["async function renderDiceBearAvatar({ style, seed }: DiceBearSpec) {", "diceBear"],
        ["function generateIdenticon(seedText: string) {", "identicon"],
      ] : id.endsWith("/AgentAvatar.tsx") ? [
        ["const seedText = agent.id", "image"],
      ] : [];
      for (const [anchor, counter] of probes) {
        assert.ok(source.includes(anchor), `Missing instrumentation anchor: ${anchor}`);
        const probe = `window.__renderStats.${counter} += 1;`;
        source = source.replace(anchor, anchor.startsWith("const") ? `${probe}\n${anchor}` : `${anchor}\n${probe}`);
      }
      return probes.length ? source : null;
    },
  };
  const config = {
    configFile: false, logLevel: "silent", root: resolve("tests/fixtures/avatar-cache"), publicDir: false,
    plugins: [instrumentation, react()],
    resolve: { alias: [{ find: /^react-dom\/client$/, replacement: "react-dom/profiling" }] },
    build: { outDir: directory },
  };
  await build(config);
  server = await preview({ ...config, preview: { host: "127.0.0.1", port: 0, strictPort: true } });
  browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.addInitScript(() => {
    window.__renderStats = { image: 0, identicon: 0, diceBear: 0, formatter: 0 };
    Intl.DateTimeFormat = new Proxy(Intl.DateTimeFormat, {
      construct(target, args) { window.__renderStats.formatter += 1; return Reflect.construct(target, args); },
    });
  });
  await page.goto(`http://127.0.0.1:${server.httpServer.address().port}`, { waitUntil: "networkidle" });
  await page.waitForFunction(() => document.querySelectorAll(".stage img").length === 30);
  const initial = await page.evaluate(() => ({ ...window.__renderStats }));
  assert.equal(initial.diceBear, 1, "30 concurrent mounts share one generated URI");
  assert.equal(initial.identicon, 1);
  for (let index = 0; index < 10; index += 1) {
    await page.evaluate(() => window.__avatarProbe.rerender());
    await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
  }
  assert.deepEqual(await page.evaluate(() => window.__renderStats), initial, "unchanged image props bail out despite new agent objects");
  for (let index = 0; index < 3; index += 1) {
    await page.evaluate(() => window.__avatarProbe.remount());
    await page.waitForFunction((key) => window.__avatarProbe.snapshots.at(-1).key === key, index + 1);
    assert.equal(await page.evaluate(() => window.__avatarProbe.snapshots.at(-1).blanks), 0, "warm remount first layout contains no fallback pixels");
  }
  await page.locator(".stage .agent-avatar-profile-anchor").first().hover();
  await page.getByRole("tooltip").waitFor();
  await page.locator(".stage").evaluate((node) => { node.scrollTop = 100; });
  await page.keyboard.press("Escape");
  const warm = await page.evaluate(() => ({ stats: window.__renderStats, commits: window.__avatarProbe.commits }));
  assert.equal(warm.stats.identicon, initial.identicon);
  assert.equal(warm.stats.diceBear, initial.diceBear);
  assert.equal(warm.stats.formatter, initial.formatter);
  assert.ok(warm.commits.length > 10, "React Profiler must be active");
  console.log(JSON.stringify({ cold: initial, afterRerendersRemountsHoverScroll: warm.stats, profilerCommits: warm.commits.length }));

  await page.evaluate(() => window.__avatarProbe.change("dicebear:dylan:late"));
  await page.waitForFunction(() => document.querySelectorAll(".stage .agent-avatar-pixels").length === 30);
  await page.evaluate(() => window.__avatarProbe.change("😀"));
  await page.waitForFunction(() => document.querySelectorAll(".stage .agent-avatar-glyph").length === 30);
  await page.evaluate(() => window.__avatarProbe.resolveLate());
  await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
  assert.equal(await page.locator(".stage img").count(), 0, "late request must not replace a newer avatar");
  assert.deepEqual(errors, []);
  console.log("PASS warm remounts have no fallback frame; stale async avatar cannot overwrite a newer glyph");
} finally {
  await browser?.close();
  if (server) await new Promise((resolve, reject) => server.httpServer.close((error) => error ? reject(error) : resolve()));
  await rm(directory, { recursive: true, force: true });
}
