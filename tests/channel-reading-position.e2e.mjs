// Real Conversation + read-receipt hook; synthetic messages and intercepted API.
import assert from "node:assert/strict";
import { createServer } from "vite";
import react from "@vitejs/plugin-react";
import { fileURLToPath } from "node:url";
const root = fileURLToPath(new URL("./fixtures/channel-reading-position", import.meta.url));
const server = await createServer({ configFile: false, root, publicDir: false, plugins: [react()],
  server: { host: "127.0.0.1", port: 5194, strictPort: process.argv.includes("--serve"),
    fs: { allow: [fileURLToPath(new URL("..", import.meta.url))] } } });
await server.listen();
const url = `http://127.0.0.1:${server.httpServer.address().port}`;
if (process.argv.includes("--serve")) {
  console.log(`Reading-position fixture: ${url}`);
  process.on("SIGINT", async () => { await server.close(); process.exit(0); });
} else {
  const { chromium, webkit } = await import("playwright");
  try {
    for (const engine of [chromium, webkit]) {
      const browser = await engine.launch({ headless: true });
      try {
        const page = await browser.newPage({ viewport: { width: 1280, height: 850 } });
        const errors = []; page.on("pageerror", error => errors.push(error.message));
        const click = name => page.getByRole("button", { name, exact: true }).click();
        const state = async () => JSON.parse(await page.getByLabel("Reading diagnostics").textContent());
        const settled = () => page.waitForFunction(() => {
          const text = document.querySelector('[aria-label="Reading diagnostics"]')?.textContent;
          return text?.trim().startsWith("{") && JSON.parse(text).restoring === "false";
        });
        const bottom = async () => { await settled(); await page.waitForFunction(() => JSON.parse(document.querySelector('[aria-label="Reading diagnostics"]').textContent).distance <= 2); };
        await page.goto(`${url}/?reset`);
        await page.waitForTimeout(450);
        assert.equal((await state()).receipts.length, 0, "preview is not read");
        await click("Hydrate channel"); await bottom();
        await page.waitForTimeout(400);
        assert.equal((await state()).receipts.at(-1).throughSeq, 120);
        await click("Queue old scroll and switch"); await bottom();
        assert.equal((await state()).pages, 0, "late old-node scroll cannot load history");
        await click("Switch to channel"); await bottom();
        const tailAnchor = (await state()).anchor;
        await click("Switch to other"); await click("Add to channel while away"); await click("Switch to channel"); await settled();
        await page.waitForTimeout(450);
        assert.equal((await state()).anchor.messageId, tailAnchor.messageId, "previous tail is a reading point, not a command to read future messages");
        assert.equal((await state()).receipts.filter(r => r.channelId === "channel-0" && r.throughSeq > 120).length, 0);
        await click("Read middle"); await page.waitForTimeout(250);
        const anchor = (await state()).anchor;
        await click("Switch to other"); await click("Add to channel while away"); await click("Switch to channel"); await settled();
        await page.waitForTimeout(450);
        assert.equal((await state()).anchor.messageId, anchor.messageId);
        assert.ok(Math.abs((await state()).anchor.offset - anchor.offset) < 2);
        assert.equal((await state()).receipts.filter(r => r.channelId === "channel-0" && r.throughSeq > 120).length, 0);
        await click("Grow earlier row"); await page.waitForTimeout(200);
        assert.equal((await state()).anchor.messageId, anchor.messageId);
        await page.goto(`${url}/`); await click("Hydrate channel"); await settled();
        assert.equal((await state()).anchor.messageId, anchor.messageId, "reload restores persisted anchor");
        await click("Back to bottom"); await bottom();
        await click("Start stream"); await bottom();
        await click("Grow stream"); await bottom();
        await click("Read middle"); await page.waitForTimeout(200);
        const paused = (await state()).anchor;
        await click("Grow stream"); await page.waitForTimeout(250);
        assert.equal((await state()).anchor.messageId, paused.messageId);
        assert.ok(Math.abs((await state()).anchor.offset - paused.offset) < 2);
        for (const suffix of ["seed", "seed&context", "seed&deleted", "seed&fail"]) {
          await page.goto(`${url}/?${suffix}`); await click("Hydrate channel"); await settled();
          assert.equal((await state()).pages, 1, "restoration makes bounded progress");
          assert.equal((await state()).receipts.length, 0);
          if (suffix === "seed" || suffix === "seed&context") assert.equal((await state()).anchor.messageId, "0-5");
          if (suffix === "seed&fail") assert.equal((await state()).saved.find(([id]) => id === "channel-0")[1].anchor.messageId, "0-5", "transient failure retains the durable resume point");
        }
        await page.goto(`${url}/?reset&ready&delay=1000`); await bottom();
        await click("Read older history"); await click("Switch to other"); await bottom();
        await page.waitForTimeout(1200);
        assert.equal((await state()).channel, 1);
        assert.ok((await state()).distance <= 2, "late history response cannot move another channel");
        assert.equal((await state()).pages, 1);
        assert.deepEqual(errors, []);
        console.log(`${engine.name()}: channel restoration and read-receipt regressions passed`);
      } finally { await browser.close(); }
    }
  } finally { await server.close(); }
}
