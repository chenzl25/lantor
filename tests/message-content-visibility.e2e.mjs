// Real panels and complete Markdown DOM, production React profiling build.
// ?baseline disables only containment for a like-for-like browser comparison.
import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { build, preview } from "vite";
import react from "@vitejs/plugin-react";
import { chromium, webkit } from "playwright";
const directory = await mkdtemp(join(tmpdir(), "lantor-long-list-"));
const id = (n) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const results = [];
let server, browser;
try {
  const config = {
    configFile: false, logLevel: "silent", root: resolve("tests/fixtures/message-content-visibility"), publicDir: false,
    plugins: [react()], resolve: { alias: [{ find: /^react-dom\/client$/, replacement: "react-dom/profiling" }] },
    build: { outDir: directory },
  };
  await build(config);
  server = await preview({ ...config, preview: { host: "127.0.0.1", port: 0, strictPort: true } });
  const engine = process.env.TEST_BROWSER === "webkit" ? webkit : chromium;
  browser = await engine.launch({ headless: true });
  const metrics = async (cdp) => cdp ? Object.fromEntries((await cdp.send("Performance.getMetrics")).metrics.map(({name, value}) => [name, value])) : null;
  const difference = (before, after) => before && after ? ({ scriptingMs: Math.max(0, (after.ScriptDuration - before.ScriptDuration) * 1000), renderingMs: Math.max(0, (after.LayoutDuration + after.RecalcStyleDuration - before.LayoutDuration - before.RecalcStyleDuration) * 1000) }) : null;
  const runs = Number(process.env.BENCH_RUNS || 3);
  for (const thread of [false, true]) for (let run = 0; run < runs; run++) for (const baseline of [true, false]) {
    const page = await browser.newPage({ viewport: { width: 1200, height: 900 } });
    page.setDefaultTimeout(20000);
    const errors = [];
    page.on("pageerror", (e) => errors.push(e.message));
    await page.addInitScript(() => { window.__rowProbe = { commits: [], events: [], parentCommits: 0 }; });
    const cdp = engine === chromium ? await page.context().newCDPSession(page) : null;
    await cdp?.send("Performance.enable");
    const initial = await metrics(cdp);
    const container = page.locator(thread ? ".thread-scroll" : ".message-list");
    const row = (n) => container.locator(`article[data-message-id="${id(n)}"]`);
    await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/?${thread ? "thread&" : ""}${baseline ? "baseline" : ""}`, { waitUntil: "networkidle" });
    await page.waitForTimeout(150);
    const mounted = await metrics(cdp);
    assert.equal(await container.locator("article[data-message-id]").count(), thread ? 2201 : 2200, "all rows retain their complete DOM");
    assert.ok((await row(201).textContent()).includes("Message 201"));
    const containment = await row(201).evaluate((node) => ({ active: getComputedStyle(node).contentVisibility, supported: CSS.supports("overflow-anchor", "auto") && CSS.supports("overflow-clip-margin", "100px") && CSS.supports("selector(:has(*))") }));
    assert.equal(containment.active, !baseline && containment.supported ? "auto" : "visible");
    const mount = await page.evaluate(() => window.__rowProbe.commits.find((c) => c.phase === "mount"));
    assert.ok(mount.actualDuration > 0, "production profiler enabled");
    const distance = () => container.evaluate((node) => node.scrollHeight - node.scrollTop - node.clientHeight);
    assert.ok(await distance() <= 2, "initial bottom-follow settles after intrinsic heights resolve");

    // Scroll exactly one viewport over 45 animation frames, with no row reads.
    const scrollOneScreen = async () => {
      const beforeScroll = await metrics(cdp);
      const frames = await container.evaluate((node) => new Promise((done) => {
        const firstTop = node.scrollTop, height = node.clientHeight, intervals = [];
        node.dispatchEvent(new WheelEvent("wheel", {deltaY:-1, bubbles:true}));
        let previous, frame = 0;
        const tick = (time) => {
          if (previous !== undefined) intervals.push(time - previous);
          previous = time;
          node.scrollTop = firstTop - height * (++frame / 45);
          if (frame < 45) requestAnimationFrame(tick); else requestAnimationFrame(() => done(intervals));
        };
        requestAnimationFrame(tick);
      }));
      const scrolled = await metrics(cdp);
      frames.sort((a,b) => a-b);
      return { cpu: difference(beforeScroll, scrolled), frameP95Ms: frames[Math.floor(frames.length * .95)], slowFrames: frames.filter((ms) => ms > 25).length };
    };
    const firstScroll = await scrollOneScreen();
    const record = { engine: engine.name(), thread, baseline, run, containment:containment.active, mountReactMs: mount.actualDuration, mountCommitMs: mount.commitTime - mount.startTime,
      initial: difference(initial, mounted), scroll: firstScroll };
    results.push(record);
    if (!baseline && run === runs - 1) {
      // Reference navigation into a never-rendered old row, followed by explicit
      // notification-style focus and clearing the highlight.
      await container.evaluate((node) => { node.scrollTop = node.scrollHeight; });
      await row(2400).locator(".message-reference-card").click();
      await page.waitForTimeout(250);
      const visibleTarget = async (n) => row(n).evaluate((node) => {
        const viewport = node.closest(".message-list,.thread-scroll").getBoundingClientRect();
        const rect = node.getBoundingClientRect();
        return rect.top >= viewport.top - 2 && rect.top < viewport.bottom && rect.bottom > viewport.top;
      });
      assert.ok(await visibleTarget(220), "reference jumps to real old-row geometry");
      await page.evaluate(() => window.__rowProbe.jump(222));
      await page.waitForTimeout(250);
      assert.equal(await row(222).getAttribute("data-jump-focused"), "true");
      assert.ok(await visibleTarget(222), "focused notification jump is visible");
      await page.evaluate(() => window.__rowProbe.clearFocus());
      await page.waitForTimeout(150);
      assert.ok(await visibleTarget(222));

      // Hover tools live above the article border: containment must not clip them.
      await row(222).hover();
      if (process.env.BENCH_SCREENSHOT) await page.screenshot({path:`${process.env.BENCH_SCREENSHOT}-${thread ? "thread" : "channel"}-actions.png`});
      const action = row(222).getByRole("button", {name:"Reference message", exact:true});
      await action.click();
      assert.ok((await page.evaluate(() => window.__rowProbe[location.search.includes("thread") ? "replyDraft" : "draft"])).includes(id(222)));

      await row(222).getByRole("button", {name:"Preview sample.svg",exact:true}).click();
      const dialog = page.getByRole("dialog", {name:"Image preview"});
      await dialog.waitFor();
      await page.mouse.move(5,5);
      await page.evaluate(() => document.activeElement?.blur());
      const bounds = await dialog.boundingBox();
      assert.equal(bounds.width, 1200, "lightbox escapes row containment even without focus/hover");
      assert.equal(bounds.height, 900);
      if (process.env.BENCH_SCREENSHOT) await page.screenshot({path:`${process.env.BENCH_SCREENSHOT}-${thread ? "thread" : "channel"}-lightbox.png`});
      await dialog.getByRole("button", {name:"Close image preview",exact:true}).last().click();

      assert.ok(await page.evaluate(() => window.find("Message 1201: short note.",false,false,true)), "find-in-page sees skipped Markdown");
      assert.equal(await page.evaluate(() => getSelection()?.anchorNode?.parentElement?.closest("article")?.dataset.messageId), id(1201));
      // window.find selects but does not scroll (also true with containment off).
      // Reveal the result separately, as the browser's find UI does.
      await page.evaluate(() => getSelection()?.anchorNode?.parentElement?.scrollIntoView({block:"center"}));
      await page.waitForTimeout(250);
      assert.ok(await visibleTarget(1201), "selected offscreen content can be revealed");
      await page.evaluate(() => window.getSelection()?.removeAllRanges());

      if (!thread) {
        for (let n = 0; n < 5; n++) {
          await page.evaluate((delay) => { window.__rowProbe.loadDelay = delay; }, n === 2 ? 200 : 0);
          await container.evaluate((node) => { node.dispatchEvent(new WheelEvent("wheel", {deltaY:-1,bubbles:true})); node.scrollTop = 0; });
          if (n === 2) {
            await page.waitForTimeout(80);
            await container.evaluate((node) => { node.scrollTop = 80; });
          }
          await page.waitForFunction((pages) => window.__rowProbe.pages >= pages, n + 1);
          await page.waitForTimeout(300);
          const anchor = await page.evaluate(() => window.__rowProbe.prependAnchor);
          const offset = await container.locator(`article[data-message-id="${anchor.id}"]`).evaluate((node) => node.getBoundingClientRect().top - node.closest(".message-list").getBoundingClientRect().top);
          assert.ok(Math.abs(offset - (anchor.offset - (n === 2 ? 80 : 0))) <= 2, `page ${n + 1} prepend retains the visible anchor: ${anchor.offset} -> ${offset}`);
        }
        assert.equal(await container.locator("article[data-message-id]").count(), 2400);
      }
      await container.evaluate((node) => { node.scrollTop = node.scrollHeight; });
      await page.waitForTimeout(150);
      record[thread ? "afterNavigation" : "afterFivePages"] = await scrollOneScreen();
      // Report timing rather than assert hardware/engine-dependent frame rates.
      // Exercise #102 against the same long history, with many skipped rows.
      await container.evaluate((node) => { node.scrollTop = node.scrollHeight; });
      await page.waitForTimeout(150);
      await page.evaluate(() => window.__rowProbe.stream());
      await row(9001).waitFor();
      await page.waitForTimeout(200);
      const commits = await page.evaluate(() => window.__rowProbe.parentCommits);
      let streamed = "Streaming start";
      for (let n = 0; n < 12; n++) {
        streamed += `\n\nStreamed paragraph ${n}.`;
        await page.evaluate((body) => window.__rowProbe.chunk(body), streamed);
        await page.waitForTimeout(60);
      }
      assert.ok(await distance() <= 2, "long list follows streaming height changes");
      // The panel tree's Profiler also counts subscribed descendants; the SSE
      // regression separately asserts no parent function renders per delta.
      assert.ok(await page.evaluate(() => window.__rowProbe.parentCommits) > commits);
      await container.hover(); await page.mouse.wheel(0,-350); await page.waitForTimeout(200);
      const pausedTop = await container.evaluate((node) => node.scrollTop);
      for (let n = 0; n < 4; n++) {
        streamed += `\n\nPaused paragraph ${n}.`;
        await page.evaluate((body) => window.__rowProbe.chunk(body), streamed);
        await page.waitForTimeout(60);
      }
      assert.ok(Math.abs(await container.evaluate((node) => node.scrollTop) - pausedTop) <= 2, "long-list manual reading stays anchored during streaming");
      await container.evaluate((node) => { node.scrollTop = node.scrollHeight; });
      await page.waitForTimeout(150);
      await page.evaluate((body) => window.__rowProbe.chunk(body), streamed + "\n\nFollow again.");
      await page.waitForTimeout(150);
      assert.ok(await distance() <= 2);
      await page.emulateMedia({media:"print"});
      assert.equal(await row(201).evaluate((node) => getComputedStyle(node).contentVisibility), "visible", "print renders every row");
      await page.emulateMedia({media:"screen"});
      assert.deepEqual(errors, []);
    }
    console.log(JSON.stringify(record));
    await page.close();
  }
  if (process.env.BENCH_REPORT) await writeFile(process.env.BENCH_REPORT, JSON.stringify({browser:browser.version(),viewport:[1200,900],messages:2200,results},null,2));
} finally {
  await browser?.close();
  await new Promise((done) => server ? server.httpServer.close(done) : done());
  await rm(directory, {recursive:true,force:true});
}
