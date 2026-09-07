// Shared dialog contract in Chromium and WebKit, including native opener failures.
import assert from "node:assert/strict";
import { mkdtemp, mkdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { build, preview } from "vite";
import react from "@vitejs/plugin-react";
import { chromium, webkit } from "playwright";
const directory = await mkdtemp(join(tmpdir(), "lantor-dialog-test-"));
let server, browser;
try {
  const config = { configFile: false, logLevel: "silent", root: resolve("tests/fixtures/dialogs"), publicDir: false,
    plugins: [react()], build: { outDir: directory } };
  await build(config);
  server = await preview({ ...config, preview: { host: "127.0.0.1", port: 0, strictPort: true } });
  for (const [name, engine] of Object.entries({ chromium, webkit })) {
    browser = await engine.launch();
    const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
    page.setDefaultTimeout(6000);
    const errors = [];
    page.on("pageerror", error => errors.push(error.message));
    await page.goto(`http://127.0.0.1:${server.httpServer.address().port}`);
    const outer = page.getByRole("dialog", { name: "Outer dialog", exact: true });
    const nested = page.getByRole("dialog", { name: "Nested dialog", exact: true });
    const focused = () => page.evaluate(() => document.activeElement?.getAttribute("aria-label") || document.activeElement?.textContent);
    const opener = page.getByRole("button", { name: "Open fixture", exact: true });
    await opener.focus(); await opener.click();
    await outer.waitFor();
    assert.equal(await focused(), "First input");
    assert.equal(await page.locator("#root").evaluate(e => e.inert), true);
    assert.equal(await page.evaluate(() => document.body.style.overflow), "hidden");
    await outer.getByRole("button", { name: "Last button", exact: true }).focus();
    await page.keyboard.press("Tab"); assert.equal(await focused(), "Close");
    await page.keyboard.press("Shift+Tab"); assert.equal(await focused(), "Last button");
    // Scripted focus cannot escape to an inert background control.
    await opener.evaluate(e => e.focus()); assert.equal(await focused(), "Last button");
    const text = await outer.locator("p").first().boundingBox();
    await page.mouse.move(text.x + 10, text.y + 10); await page.mouse.down();
    await page.mouse.move(10, 10); await page.mouse.up();
    assert.equal(await outer.count(), 1, "text drag ending on backdrop must not dismiss");
    await outer.getByRole("button", { name: "Open nested", exact: true }).focus();
    await page.keyboard.press("Enter"); await nested.waitFor();
    assert.equal(await focused(), "Nested input");
    await page.keyboard.press("Escape"); await nested.waitFor({ state: "detached" });
    assert.equal(await focused(), "Open nested"); assert.equal(await outer.count(), 1);
    await page.keyboard.press("Enter"); await nested.waitFor();
    await nested.getByRole("button", { name: "Lock dismissal" }).click();
    await page.keyboard.press("Escape"); assert.equal(await nested.count(), 1);
    await page.mouse.click(10, 10); assert.equal(await nested.count(), 1);
    await nested.getByRole("button", { name: "Unlock dismissal" }).click();
    for (const extra of [{ isComposing: true }, { repeat: true }, { keyCode: 229 }]) {
      await page.evaluate(extra => window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, ...extra })), extra);
      assert.equal(await nested.count(), 1, "IME/repeated Escape must not dismiss");
    }
    await page.keyboard.press("Escape"); await nested.waitFor({ state: "detached" });
    const preview = outer.getByRole("button", { name: "Preview preview.svg", exact: true });
    await preview.focus(); await preview.click();
    await page.getByRole("dialog", { name: "Image preview" }).waitFor();
    assert.equal(await page.locator('.attachment-lightbox img').evaluate(e => e.getBoundingClientRect().width > 0), true);
    await page.keyboard.press("Escape");
    await page.getByRole("dialog", { name: "Image preview" }).waitFor({ state: "detached" });
    assert.equal(await focused(), "Preview preview.svg"); assert.equal(await outer.count(), 1);
    await outer.getByRole("button", { name: "Preview draft.svg", exact: true }).click();
    await page.getByRole("dialog", { name: "Image preview" }).waitFor();
    await page.keyboard.press("Escape");
    await page.getByRole("dialog", { name: "Image preview" }).waitFor({ state: "detached" });
    assert.equal(await focused(), "Preview draft.svg");
    // Mock only the desktop boundary; production Markdown/attachment handlers run unchanged.
    await page.evaluate(() => {
      window.__TAURI_INTERNALS__ = { invoke: () => Promise.reject(new Error("fixture opener denied")), convertFileSrc: () => "about:blank" };
    });
    await outer.getByRole("link", { name: "Broken link", exact: true }).click();
    await page.getByRole("alert").filter({ hasText: "Could not open link: fixture opener denied" }).waitFor();
    assert.equal(await page.getByRole("alert").evaluate(e => Boolean(e.closest('[role="dialog"]')) && !e.closest('[inert]')), true);
    await page.getByRole("button", { name: "Dismiss notification" }).click();
    await outer.getByRole("link", { name: "Open report.txt", exact: true }).click();
    await page.getByRole("alert").filter({ hasText: "Open failed: fixture opener denied" }).waitFor();
    await page.getByRole("button", { name: "Dismiss notification" }).click();
    await outer.getByRole("button", { name: "Trigger error", exact: true }).click();
    await page.evaluate(() => document.documentElement.dataset.theme = "dark");
    const toast = page.getByRole("alert");
    const visibleToast = await toast.evaluate(e => { const r = e.getBoundingClientRect(); return e.contains(document.elementFromPoint(r.x + 20, r.y + 20)); });
    assert.ok(visibleToast, "notice must paint above the dialog");
    if (process.env.LANTOR_UI_SCREENSHOTS) {
      await mkdir(process.env.LANTOR_UI_SCREENSHOTS, { recursive: true });
      await page.screenshot({ path: join(process.env.LANTOR_UI_SCREENSHOTS, `dialog-dark-${name}.png`) });
    }
    await page.getByRole("button", { name: "Dismiss notification" }).click();
    await page.keyboard.press("Escape"); await outer.waitFor({ state: "detached" });
    assert.equal(await focused(), "Open fixture");
    assert.equal(await page.locator("#root").evaluate(e => e.inert), false);
    assert.equal(await page.evaluate(() => document.body.style.overflow), "");
    await opener.click(); await outer.waitFor(); await page.mouse.click(10, 10);
    await outer.waitFor({ state: "detached" });
    await page.getByRole("button", { name: "Browse threads" }).focus();
    await page.keyboard.press("Enter"); await page.getByRole("dialog", { name: "Threads", exact: true }).waitFor();
    await page.keyboard.press("Escape");
    assert.equal(await focused(), "Browse threads");
    await page.getByRole("button", { name: "Open issue", exact: true }).click();
    const issue = page.getByRole("dialog", { name: "Example issue", exact: true }); await issue.waitFor();
    await page.keyboard.press("Tab"); assert.ok(await issue.evaluate(e => e.contains(document.activeElement)));
    await page.keyboard.press("Escape"); await issue.waitFor({ state: "detached" });
    assert.equal(await focused(), "Open issue");
    await page.getByRole("button", { name: "Show fatal style", exact: true }).click();
    await page.locator(".fatal-card").waitFor();
    const fatalBackground = await page.locator(".fatal-card").evaluate(e => getComputedStyle(e).backgroundColor);
    assert.ok(!fatalBackground.includes("255, 255, 255"), "fatal page uses dark surface tokens");
    if (process.env.LANTOR_UI_SCREENSHOTS) await page.screenshot({ path: join(process.env.LANTOR_UI_SCREENSHOTS, `fatal-dark-${name}.png`) });
    assert.deepEqual(errors, []);
    console.log(`${name}: focus loop/restore, inert, scroll lock, drag-safe backdrop, IME, nested image/locked dialog, draft/saved previews, issue drawer, desktop opener errors, dark notice/fatal page passed`);
    await browser.close(); browser = null;
  }
} finally {
  await browser?.close(); await server?.httpServer.close(); await rm(directory, { recursive: true, force: true });
}
