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
        const opensAtDivider = async (messageId, message) => {
          await settled(); await page.waitForTimeout(300);
          const current = await state();
          assert.equal(current.divider, messageId, message);
          // A date divider right above it stays on screen too.
          assert.ok(current.dividerTop >= 0 && current.dividerTop < 80, `divider opens at the top (${current.dividerTop})`);
          assert.ok(current.distance > 2, "unread messages below the screen are not read yet");
          return current;
        };
        const noNewReceipts = async (after) => assert.equal((await state()).receipts.filter(r => r.channelId === "channel-0" && r.throughSeq > after).length, 0);
        await page.goto(`${url}/?reset`);
        await page.waitForTimeout(450);
        assert.equal((await state()).receipts.length, 0, "preview is not read");
        await click("Hydrate channel"); await bottom();
        await page.waitForTimeout(400);
        assert.equal((await state()).receipts.at(-1).throughSeq, 120);
        assert.equal((await state()).divider, null, "nothing unread opens at the latest message without a divider");
        await click("Queue old scroll and switch"); await bottom();
        assert.equal((await state()).pages, 0, "late old-node scroll cannot load history");
        await click("Switch to channel"); await bottom();

        // Messages that arrived while away open at the first unread one.
        await click("Switch to other"); await click("Add long messages to channel"); await click("Switch to channel");
        let current = await opensAtDivider("0-121", "a channel opens at its first unread message");
        assert.match(current.backToBottom, /^\d+ new messages?$/, "back-to-bottom counts the messages below");
        await noNewReceipts(120);
        await click(current.backToBottom); await bottom();
        await page.waitForTimeout(400);
        assert.equal((await state()).receipts.at(-1).throughSeq, 126);
        assert.equal((await state()).firstUnread, null);
        assert.equal((await state()).divider, "0-121", "the divider stays while the channel is open");
        await click("Read middle"); await page.waitForTimeout(250);
        assert.equal((await state()).backToBottom, "Back to bottom", "messages already shown are not counted as new");
        await click("Switch to other"); await click("Switch to channel"); await page.waitForTimeout(300); await settled();
        assert.equal((await state()).divider, null, "a read channel reopens without a divider");

        // Without unread messages, a position read earlier this session is kept.
        await page.waitForTimeout(250);
        const anchor = (await state()).anchor;
        await click("Switch to other"); await click("Switch to channel"); await settled();
        await page.waitForTimeout(450);
        assert.equal((await state()).anchor.messageId, anchor.messageId);
        assert.ok(Math.abs((await state()).anchor.offset - anchor.offset) < 2);
        await noNewReceipts(126);
        await click("Grow earlier row"); await page.waitForTimeout(200);
        assert.equal((await state()).anchor.messageId, anchor.messageId);
        await click("Add to channel while away"); await page.waitForTimeout(250);
        assert.equal((await state()).backToBottom, "1 new message", "a message arriving below is counted");

        // A reload opens at the server's read position, not a stale local one.
        await page.goto(`${url}/`); await click("Hydrate channel"); await bottom();
        assert.equal((await state()).divider, null);
        await click("Start stream"); await bottom();
        await click("Grow stream"); await bottom();
        await click("Read middle"); await page.waitForTimeout(200);
        const paused = (await state()).anchor;
        await click("Grow stream"); await page.waitForTimeout(250);
        assert.equal((await state()).anchor.messageId, paused.messageId);
        assert.ok(Math.abs((await state()).anchor.offset - paused.offset) < 2);

        // An old first unread message loads earlier history first.
        for (const [suffix, expected] of [["unread=5", "0-5"], ["unread=5&context", "0-5"], ["unread=5&deleted", "0-6"], ["unread=5&fail", "0-41"]]) {
          await page.goto(`${url}/?${suffix}`); await click("Hydrate channel");
          await opensAtDivider(expected, `${suffix} opens at ${expected}`);
          assert.equal((await state()).pages, 1, "restoration makes bounded progress");
          assert.equal((await state()).receipts.length, 0);
        }

        // Returning from the background moves a reader who was at the latest
        // message to what arrived meanwhile, after the catch-up finishes.
        await page.goto(`${url}/?ready`); await bottom();
        const tail = (await state()).anchor;
        await click("Go to background"); await click("Add long messages to channel"); await click("Return and sync");
        await page.waitForTimeout(700);
        current = await state();
        assert.equal(current.anchor.messageId, tail.messageId, "the view holds still while catching up");
        assert.ok(current.syncingShown, "catching up shows Updating…");
        await noNewReceipts(120);
        await click("Finish sync");
        await opensAtDivider("0-121", "returning opens at the first message that arrived meanwhile");
        assert.equal((await state()).syncingShown, false);
        await noNewReceipts(120);
        // Read on another device meanwhile: return straight to the latest message.
        await page.goto(`${url}/?ready`); await bottom();
        await click("Go to background"); await click("Add long messages to channel"); await click("Read on another device");
        await click("Return and sync"); await click("Finish sync"); await bottom();
        assert.equal((await state()).divider, null);
        // Touching the list while catching up keeps the reader in control.
        await page.goto(`${url}/?ready`); await bottom();
        await click("Go to background"); await click("Add long messages to channel"); await click("Return and sync");
        await click("Read middle"); await page.waitForTimeout(250); const held = (await state()).anchor;
        await click("Finish sync"); await page.waitForTimeout(400);
        assert.equal((await state()).anchor.messageId, held.messageId, "no jump after the reader scrolled");

        await page.goto(`${url}/?reset&ready&delay=1000`); await bottom();
        await click("Read older history"); await click("Switch to other"); await bottom();
        await page.waitForTimeout(1200);
        assert.equal((await state()).channel, 1);
        assert.ok((await state()).distance <= 2, "late history response cannot move another channel");
        assert.equal((await state()).pages, 1);
        await page.goto(`${url}/?reset&ready&snapshot-chain`);
        await page.waitForFunction(() => {
          const text = document.querySelector('[aria-label="Reading diagnostics"]')?.textContent;
          return text?.trim().startsWith("{") && JSON.parse(text).location?.latestRootId === "0-180";
        });
        await bottom();
        assert.equal((await state()).rowCount, 140, "parent snapshot reconciliation finishes without nested render updates");
        await page.waitForTimeout(400);
        assert.equal((await state()).receipts.at(-1).throughSeq, 180, "read receipt follows the settled final snapshot");
        assert.deepEqual(errors, []);
        console.log(`${engine.name()}: channel opening, unread divider and read-receipt regressions passed`);
      } finally { await browser.close(); }
    }
  } finally { await server.close(); }
}
