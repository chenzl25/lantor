// Home Screen web app attachments: nothing may navigate, open a window, or
// download, because standalone mode has no browser chrome to come back from.
import assert from "node:assert/strict";
import { mkdtemp, mkdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { build, preview } from "vite";
import react from "@vitejs/plugin-react";
import { chromium, devices, webkit } from "playwright";

const markdown = "# Deploy notes\n\nThe switch completed cleanly.\n";
const files = {
  markdown: { type: "text/markdown", body: Buffer.from(markdown) },
  pdf: { type: "application/pdf", body: Buffer.concat([Buffer.from("%PDF-1.4\n"), Buffer.alloc(1191, 32)]) },
  image: { type: "image/svg+xml", body: Buffer.from('<svg xmlns="http://www.w3.org/2000/svg" width="320" height="160"><rect width="320" height="160" fill="teal"/></svg>') },
  flaky: { type: "text/plain", body: Buffer.from("hello world") },
};
const standalone = ({ share = true } = {}) => `
  Object.defineProperty(navigator, "standalone", { configurable: true, value: true });
  window.__shared = [];
  if (${share}) {
    navigator.canShare = (data) => Array.isArray(data?.files) && data.files.length > 0;
    navigator.share = async (data) => {
      for (const file of data.files) window.__shared.push({ name: file.name, type: file.type, size: file.size, text: await file.text() });
    };
  } else {
    delete Navigator.prototype.share; delete Navigator.prototype.canShare;
  }`;

const directory = await mkdtemp(join(tmpdir(), "lantor-attachment-sheet-"));
let server, browser;
try {
  const config = { configFile: false, logLevel: "silent", root: resolve("tests/fixtures/attachment-sheet"), publicDir: false,
    plugins: [react()], build: { outDir: directory } };
  await build(config);
  server = await preview({ ...config, preview: { host: "127.0.0.1", port: 0, strictPort: true } });
  const fixtureUrl = `http://127.0.0.1:${server.httpServer.address().port}/`;
  const engines = { chromium: [chromium, { viewport: { width: 1280, height: 900 } }], webkit: [webkit, devices["iPhone 14"]] };
  for (const [name, [engine, device]] of Object.entries(engines)) {
    browser = await engine.launch();
    let flakyHealthy = false;
    async function open(contextOptions, initScript) {
      const context = await browser.newContext({ ...device, ...contextOptions });
      if (initScript) await context.addInitScript(initScript);
      const leaks = [];
      const page = await context.newPage();
      context.on("page", (popup) => leaks.push(`window ${popup.url()}`));
      page.setDefaultTimeout(6000);
      const errors = [];
      page.on("pageerror", (error) => errors.push(error.message));
      page.on("download", (download) => leaks.push(`download ${download.suggestedFilename()}`));
      page.on("framenavigated", (frame) => { if (frame === page.mainFrame() && frame.url() !== fixtureUrl) leaks.push(`navigation ${frame.url()}`); });
      await context.route("**/api/attachments/*", (route) => {
        const id = new URL(route.request().url()).pathname.split("/").pop();
        if (id === "flaky" && !flakyHealthy) return route.fulfill({ status: 500, body: "boom" });
        return route.fulfill({ status: 200, contentType: files[id].type, body: files[id].body });
      });
      await page.goto(fixtureUrl);
      return { context, page, leaks, errors };
    }
    const sheet = (page, title) => page.getByRole("dialog", { name: title, exact: true });
    const shared = async (page, count) => {
      await page.waitForFunction((count) => window.__shared.length === count, count);
      return page.evaluate(() => window.__shared);
    };
    const focused = (page) => page.evaluate(() => document.activeElement?.getAttribute("aria-label") || document.activeElement?.textContent);

    // Home Screen app with share support (iOS 15+).
    {
      const { context, page, leaks, errors } = await open({}, standalone());
      // Position links must resolve to the same snapshot and stay in the PWA.
      for (const name of ["line notes", "hash notes"]) {
        await page.getByRole("link", { name, exact: true }).click();
        const preview = sheet(page, "implementation.md");
        await preview.getByRole("heading", { name: "Deploy notes" }).waitFor();
        await preview.getByRole("button", { name: "Close", exact: true }).click();
        await preview.waitFor({ state: "detached" });
      }
      // File card: Markdown renders in place and shares the exact bytes.
      await page.getByRole("link", { name: "Open implementation.md", exact: true }).click();
      const notes = sheet(page, "implementation.md");
      await notes.getByRole("heading", { name: "Deploy notes" }).waitFor();
      await notes.getByRole("button", { name: "Share" }).click();
      assert.deepEqual(await shared(page, 1), [{ name: "implementation.md", type: "text/markdown", size: markdown.length, text: markdown }]);
      if (process.env.LANTOR_UI_SCREENSHOTS) {
        await mkdir(process.env.LANTOR_UI_SCREENSHOTS, { recursive: true });
        await page.screenshot({ path: join(process.env.LANTOR_UI_SCREENSHOTS, `sheet-markdown-${name}.png`) });
      }
      await notes.getByRole("button", { name: "Close", exact: true }).click();
      await notes.waitFor({ state: "detached" });
      assert.equal(await focused(page), "Open implementation.md");

      // Download button and an agent's snapshot link open the same sheet.
      await page.getByRole("button", { name: "Download report.pdf", exact: true }).click();
      const report = sheet(page, "report.pdf");
      await report.getByText("No preview for this file here.").waitFor();
      if (process.env.LANTOR_UI_SCREENSHOTS) {
        await page.evaluate(() => document.documentElement.dataset.theme = "dark");
        await page.screenshot({ path: join(process.env.LANTOR_UI_SCREENSHOTS, `sheet-pdf-dark-${name}.png`) });
        await page.evaluate(() => delete document.documentElement.dataset.theme);
      }
      await report.getByRole("button", { name: "Share" }).click();
      assert.equal((await shared(page, 2))[1].size, files.pdf.body.length);
      await page.keyboard.press("Escape");
      await report.waitFor({ state: "detached" });
      await page.getByRole("link", { name: "the report", exact: true }).click();
      await report.waitFor();
      // A back gesture closes the sheet instead of leaving the app.
      await page.evaluate(() => history.pushState({ fixture: true }, ""));
      await page.evaluate(() => history.back());
      await report.waitFor({ state: "detached" });

      // The image lightbox stays in-app; its download goes through the sheet.
      await page.getByRole("button", { name: "Preview chart.svg", exact: true }).click();
      const lightbox = page.getByRole("dialog", { name: "Image preview" });
      await lightbox.getByRole("button", { name: "Download chart.svg", exact: true }).click();
      const chart = sheet(page, "chart.svg");
      await chart.locator("img.attachment-sheet-media").waitFor();
      await chart.getByRole("button", { name: "Close", exact: true }).click();
      await chart.waitFor({ state: "detached" });
      await lightbox.getByRole("button", { name: "Close image preview" }).click();
      await lightbox.waitFor({ state: "detached" });

      // Load failures stay in the sheet and can be retried.
      await page.getByRole("link", { name: "Open flaky.txt", exact: true }).click();
      const flaky = sheet(page, "flaky.txt");
      await flaky.getByText("Could not load this file: HTTP 500").waitFor();
      assert.equal(await flaky.getByRole("button", { name: "Share" }).isDisabled(), true);
      flakyHealthy = true;
      await flaky.getByRole("button", { name: "Retry" }).click();
      await flaky.getByText("hello world").waitFor();
      await page.keyboard.press("Escape");
      flakyHealthy = false;

      assert.deepEqual(leaks, [], "standalone attachments must never navigate, open windows, or download");
      assert.deepEqual(errors, []);
      await context.close();
    }

    // Home Screen app without Web Share (desktop PWAs): the sheet downloads instead.
    {
      const { context, page, leaks, errors } = await open({ acceptDownloads: true }, standalone({ share: false }));
      await page.getByRole("link", { name: "Open implementation.md", exact: true }).click();
      const notes = sheet(page, "implementation.md");
      const [download] = await Promise.all([page.waitForEvent("download"), notes.getByRole("button", { name: "Download" }).click()]);
      assert.equal(download.suggestedFilename(), "implementation.md");
      assert.deepEqual(leaks, ["download implementation.md"]);
      assert.deepEqual(errors, []);
      await context.close();
    }

    // Ordinary browser tabs keep opening files in a new tab.
    if (name === "chromium") {
      const { context, page } = await open({});
      const [popup] = await Promise.all([context.waitForEvent("page"), page.getByRole("link", { name: "Open implementation.md", exact: true }).click()]);
      assert.equal(new URL(popup.url()).pathname, "/api/attachments/markdown");
      assert.equal(await sheet(page, "implementation.md").count(), 0);
      await context.close();
    }

    console.log(`${name}: standalone card/download/link/lightbox open the in-app sheet, share exact bytes, close via button/Escape/back, retry after errors, download fallback without Web Share, browser tabs unchanged`);
    await browser.close(); browser = null;
  }
} finally {
  await browser?.close(); await server?.httpServer.close(); await rm(directory, { recursive: true, force: true });
}
