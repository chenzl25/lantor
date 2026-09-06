// Real panels, synthetic bootstrap replacements, production React Profiler.
// Probes are injected into a temporary build, never the shipped application.
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { build, preview } from "vite";
import react from "@vitejs/plugin-react";
import { chromium } from "playwright";
const directory = await mkdtemp(join(tmpdir(), "lantor-rows-test-"));
const id = (n) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
let server, browser;
try {
  const config = {
    configFile: false, logLevel: "silent", root: resolve("tests/fixtures/message-rows"), publicDir: false,
    plugins: [{
      name: "message-row-profiler", enforce: "pre",
      transform(source, path) {
        if (path.endsWith("/MessageRow.tsx")) {
          assert.ok(source.includes("  return <>\n"));
          return 'import { Profiler } from "react";\n' + source
            .replace("  return <>\n", '  return <Profiler id={`${variant}:${message.id}`} onRender={(id, phase, actualDuration) => window.__rowProbe.commits.push({ id, phase, actualDuration })}>\n')
            .replace("  </>;\n});", "  </Profiler>;\n});");
        }
        if (path.endsWith("/MessageMarkdown.tsx")) {
          const anchor = "function MessageMarkdownContent(props: MessageMarkdownProps) {";
          assert.ok(source.includes(anchor));
          return source.replace(anchor, anchor + "\nwindow.__rowProbe.markdown.push(props.scrollKey);");
        }
        if (path.endsWith("/ActivityProgressDock.tsx")) {
          const anchor = "  const activitiesByRun = new Map<string, AgentActivity[]>();";
          assert.ok(source.includes(anchor));
          return source.replace(anchor, "window.__rowProbe.indexes += 1;\n" + anchor);
        }
      },
    }, react()],
    resolve: { alias: [{ find: /^react-dom\/client$/, replacement: "react-dom/profiling" }] },
    build: { outDir: directory },
  };
  await build(config);
  server = await preview({ ...config, preview: { host: "127.0.0.1", port: 0, strictPort: true } });
  browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({ viewport: { width: 1600, height: 1000 } });
  page.setDefaultTimeout(10000);
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.addInitScript(() => { window.__rowProbe = { commits: [], markdown: [], events: [], indexes: 0, parentCommits: 0 }; });
  await page.goto(`http://127.0.0.1:${server.httpServer.address().port}`, { waitUntil: "networkidle" });
  await page.locator(".katex").first().waitFor();
  const settle = () => page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
  const reset = async () => { await settle(); await page.evaluate(() => { window.__rowProbe.commits = []; window.__rowProbe.markdown = []; window.__rowProbe.indexes = 0; }); };
  const stats = async () => { await settle(); return page.evaluate(() => ({ commits: window.__rowProbe.commits, markdown: window.__rowProbe.markdown, indexes: window.__rowProbe.indexes })); };
  const row = (variant, n) => page.locator(`${variant === "channel" ? ".conversation" : ".thread"} article[data-message-id="${id(n)}"]`);
  const changed = (result) => [...new Set(result.commits.map((entry) => entry.id))].sort();
  const expectRows = (result, expected, label) => assert.deepEqual(changed(result), expected.sort(), label);
  const initial = await stats();
  assert.ok(initial.commits.length >= 41, "Profiler inside memo rows is active");
  assert.equal(initial.indexes, 2, "one history index per panel, shared by all active roots");

  await reset();
  await page.evaluate(() => window.__rowProbe.refresh());
  const refreshed = await stats();
  expectRows(refreshed, [], "equal bootstrap replacement commits no existing rows");
  assert.deepEqual(refreshed.markdown, []);
  assert.equal(refreshed.indexes, 2);

  await reset();
  await page.evaluate(() => window.__rowProbe.send());
  const sent = await stats();
  expectRows(sent, [`channel:${id(31)}`], "send plus bootstrap only mounts the new row (including channels unread change)");
  assert.equal(sent.markdown.length, 1);

  // Inline callbacks from the parent were replaced by refresh/send. Stable row
  // callbacks must still call their latest implementations.
  await row("channel", 2).locator('a[href="/lantor/agent/Hancock"]').click();
  assert.equal(await page.evaluate(() => window.__rowProbe.events.at(-1).generation), 2);
  await row("channel", 2).locator(".message-reference-card.message").first().click();
  assert.deepEqual(await page.evaluate(() => window.__rowProbe.events.at(-1)), { kind: "reference", generation: 2, source: id(2), target: id(3) });

  await page.mouse.move(1590, 5);
  await reset();
  await row("channel", 3).locator(".agent-avatar-profile-anchor").first().hover();
  await page.getByRole("tooltip").waitFor();
  const hover = await stats();
  expectRows(hover, [`channel:${id(3)}`], "avatar hover only commits its own row subtree");
  assert.deepEqual(hover.markdown, []);
  await page.mouse.move(1590, 5);
  await page.keyboard.press("Escape");

  await reset();
  await row("channel", 4).click({ button: "right" });
  await page.getByRole("menu").waitFor();
  const menu = await stats();
  expectRows(menu, [], "opening the context menu does not commit any message row");
  assert.deepEqual(menu.markdown, []);
  await page.keyboard.press("Escape");

  await reset();
  await row("channel", 4).getByRole("button", { name: "Show more", exact: true }).click();
  expectRows(await stats(), [`channel:${id(4)}`], "expansion only commits the target row");
  assert.equal(await row("channel", 4).locator(".message-long-preview.collapsed").count(), 0);

  await reset();
  await row("reply", 102).hover();
  await row("reply", 102).getByRole("button", { name: "Save message", exact: true }).last().click();
  expectRows(await stats(), [`reply:${id(102)}`], "saving a reply only updates its row");
  assert.ok(await row("reply", 102).getAttribute("class").then((c) => c.includes("saved")));

  await reset();
  await page.evaluate(() => window.__rowProbe.edit(31, { delivery_state: "streaming", body: "Streaming start" }));
  expectRows(await stats(), [`channel:${id(31)}`], "streaming update is isolated");
  for (let index = 0; index < 5; index++) {
    await reset();
    await page.evaluate((n) => window.__rowProbe.edit(31, { body: `Streaming token ${n}` }), index);
    const result = await stats();
    expectRows(result, [`channel:${id(31)}`], "streaming siblings bail out");
    assert.equal(result.indexes, 0, "message delta does not sort activity history");
  }
  await reset();
  await page.evaluate(() => window.__rowProbe.edit(110, { delivery_state: "streaming", body: "Thread stream" }));
  expectRows(await stats(), [`reply:${id(110)}`], "reply stream does not redraw root summary or siblings");

  await reset();
  await page.evaluate(() => window.__rowProbe.activity());
  const activity = await stats();
  expectRows(activity, [`channel:${id(7)}`], "progress only updates the owning root");
  assert.deepEqual(activity.markdown, [], "progress update does not reparse unchanged Markdown");
  assert.equal(activity.indexes, 2);

  await reset();
  await page.evaluate(() => window.__rowProbe.edit(3, { body: "Updated reference target" }));
  expectRows(await stats(), [`channel:${id(2)}`, `channel:${id(3)}`], "reference targets remain reactive");
  await row("channel", 2).locator(".message-reference-card.message").first().hover();
  await page.getByText("Updated reference target", { exact: true }).last().waitFor();
  await page.mouse.move(1590, 5);

  await reset();
  await page.evaluate(() => window.__rowProbe.add(999));
  expectRows(await stats(), [`channel:${id(2)}`, `channel:${id(999)}`], "missing reference hydration updates the referring row");
  assert.equal(await row("channel", 2).locator(".message-reference-card.missing").count(), 0);
  await reset();
  await page.evaluate(() => window.__rowProbe.rename());
  expectRows(await stats(), [`channel:${id(2)}`], "channel rename updates reference labels only");
  assert.ok(await row("channel", 2).textContent().then((text) => text.includes("#renamed-channel")));

  await reset();
  await page.evaluate(() => window.__rowProbe.add(111, 1));
  expectRows(await stats(), [`channel:${id(1)}`, `channel:${id(2)}`, `reply:${id(111)}`], "new replies update only the owning summary, thread reference, and new reply");

  await page.locator(".conversation textarea").fill("Existing draft");
  await row("channel", 4).hover();
  await row("channel", 4).getByRole("button", { name: "Reference message", exact: true }).click();
  assert.equal(await page.evaluate(() => window.__rowProbe.draft), `Existing draft\n[[message:${id(4)}]]\n`, "stable action reads the latest draft");

  await page.evaluate(() => window.__rowProbe.profile());
  await row("channel", 2).locator('a[href="/lantor/agent/Hancock"]').click();
  assert.equal(await page.evaluate(() => window.__rowProbe.events.at(-1).description), "Updated profile");
  await page.evaluate(() => window.__rowProbe.removeAgent());
  await row("channel", 3).locator('[data-agent-deleted="true"]').waitFor();
  assert.deepEqual(errors, []);
  console.log(JSON.stringify({ initialRows: changed(initial).length, bootstrapExistingCommits: refreshed.commits.length, sendCommits: changed(sent), hoverCommits: changed(hover), menuCommits: menu.commits.length, progressCommits: changed(activity), checks: "stream isolation, expansion, save, live callbacks, references, hydration, channel rename, deleted avatar" }));
} finally {
  await browser?.close();
  if (server) await new Promise((done, reject) => server.httpServer.close((error) => error ? reject(error) : done()));
  await rm(directory, { recursive: true, force: true });
}
