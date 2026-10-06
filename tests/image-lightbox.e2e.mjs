// Image preview zoom in Chromium and WebKit: trackpad pinch (ctrl+wheel and
// Safari gesture events), touch pinch, panning, click/tap toggles and closing.
import assert from "node:assert/strict";
import { mkdtemp, mkdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { build, preview } from "vite";
import react from "@vitejs/plugin-react";
import { chromium, webkit } from "playwright";

const directory = await mkdtemp(join(tmpdir(), "lantor-image-lightbox-test-"));
const screenshots = process.env.LANTOR_UI_SCREENSHOTS;
let server, browser;

const near = (actual, expected, label, tolerance = 2) =>
  assert.ok(Math.abs(actual - expected) <= tolerance, `${label}: ${actual} is not within ${tolerance} of ${expected}`);

async function open(page, name = "Preview chart.svg") {
  await page.getByRole("button", { name, exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Image preview" });
  await dialog.waitFor();
  await page.locator(".attachment-lightbox img").evaluate((image) => image.decode());
  return dialog;
}
const imageRect = (page) => page.locator(".attachment-lightbox img").evaluate((image) => image.getBoundingClientRect().toJSON());
const zoomPressed = (page) => page.locator(".attachment-lightbox-zoom").getAttribute("aria-pressed");
const settle = (page) => page.waitForTimeout(300);

// Image fraction under a screen point, to check that zoom keeps it in place.
const fraction = (rect, x, y) => ({ x: (x - rect.x) / rect.width, y: (y - rect.y) / rect.height });
function assertUnder(rect, point, at, label) {
  near(rect.x + point.x * rect.width, at.x, `${label} x`);
  near(rect.y + point.y * rect.height, at.y, `${label} y`);
}

// Dispatches a WebKit gesture event as Safari does for a trackpad pinch.
const gesture = (page, type, scale, x, y) => page.locator(".attachment-lightbox-content").evaluate((stage, { type, scale, x, y }) => {
  const event = new Event(type, { bubbles: true, cancelable: true });
  Object.assign(event, { scale, clientX: x, clientY: y });
  stage.dispatchEvent(event);
  return event.defaultPrevented;
}, { type, scale, x, y });

// Touch pointers for engines without multi-touch input in Playwright.
const touch = (page, type, pointerId, x, y) => page.locator(".attachment-lightbox-content").evaluate((stage, { type, pointerId, x, y }) => {
  stage.dispatchEvent(new PointerEvent(type, { pointerId, pointerType: "touch", clientX: x, clientY: y, bubbles: true, cancelable: true, isPrimary: pointerId === 11 }));
}, { type, pointerId, x, y });

try {
  const config = { configFile: false, logLevel: "silent", root: resolve("tests/fixtures/image-lightbox"), publicDir: false,
    plugins: [react()], build: { outDir: directory } };
  await build(config);
  server = await preview({ ...config, preview: { host: "127.0.0.1", port: 0, strictPort: true } });
  const url = `http://127.0.0.1:${server.httpServer.address().port}`;
  if (screenshots) await mkdir(screenshots, { recursive: true });

  for (const [name, engine] of Object.entries({ chromium, webkit })) {
    browser = await engine.launch();
    const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
    page.setDefaultTimeout(6000);
    const errors = [];
    page.on("pageerror", (error) => errors.push(error.message));
    await page.goto(url);
    await page.evaluate(() => {
      window.__wheel = [];
      window.addEventListener("wheel", (event) => window.__wheel.push(event.defaultPrevented));
    });

    // The preview fills the screen and starts fitted.
    const dialog = await open(page);
    const box = await dialog.boundingBox();
    assert.deepEqual([box.width, box.height], [1200, 800]);
    const fit = await imageRect(page);
    near(fit.height, 800 - 84, "fitted height");
    near(fit.x + fit.width / 2, 600, "fitted centre");
    assert.equal(await zoomPressed(page), "false");

    // Trackpad pinch in Chromium/Firefox: ctrl+wheel zooms around the cursor.
    // (While narrower than the screen, an image stays centred, so the first
    // step already makes it larger than the screen.)
    const cursor = { x: fit.x + fit.width * 0.55, y: fit.y + fit.height * 0.45 };
    const anchor = fraction(fit, cursor.x, cursor.y);
    await page.mouse.move(cursor.x, cursor.y);
    await page.keyboard.down("Control");
    for (const delta of [-30, -20, -10]) await page.mouse.wheel(0, delta);
    await page.keyboard.up("Control");
    let rect = await imageRect(page);
    near(rect.width / fit.width, Math.exp(0.6), "ctrl+wheel follows the pinch", 0.01);
    assertUnder(rect, anchor, cursor, "ctrl+wheel zoom");
    assert.equal(await zoomPressed(page), "true");
    assert.ok((await page.evaluate(() => window.__wheel)).every(Boolean), "the page must not scroll or zoom");

    // Scrolling and dragging pan the zoomed image.
    await page.mouse.wheel(0, 100);
    let moved = await imageRect(page);
    near(moved.y, rect.y - 100, "scroll pans");
    near(moved.x, rect.x, "vertical scroll keeps x");
    await page.mouse.move(600, 400);
    await page.mouse.down();
    await page.mouse.move(560, 380, { steps: 4 });
    await page.mouse.move(520, 360, { steps: 4 });
    await page.mouse.up();
    rect = await imageRect(page);
    near(rect.x, moved.x - 80, "drag pans x");
    near(rect.y, moved.y - 40, "drag pans y");
    assert.equal(await zoomPressed(page), "true", "a drag is not a click");

    // Panning stops where the image edge meets the screen edge.
    await page.mouse.wheel(-5000, -5000);
    rect = await imageRect(page);
    near(rect.x, 0, "left edge");
    near(rect.y, 0, "top edge");

    // A click returns to the fitted image; another zooms to full size at the click.
    await page.mouse.click(400, 300);
    await settle(page);
    rect = await imageRect(page);
    near(rect.x, fit.x, "click fits x"); near(rect.width, fit.width, "click fits width");
    assert.equal(await zoomPressed(page), "false");
    const clickPoint = { x: fit.x + fit.width * 0.25, y: fit.y + fit.height * 0.5 };
    await page.mouse.click(clickPoint.x, clickPoint.y);
    await settle(page);
    rect = await imageRect(page);
    near(rect.width, fit.width * 2, "click zooms to 2x for an image that nearly fits", 2);
    assertUnder(rect, fraction(fit, clickPoint.x, clickPoint.y), clickPoint, "click zoom");
    await page.getByRole("button", { name: "Fit image to screen", exact: true }).click();
    await settle(page);
    near((await imageRect(page)).width, fit.width, "zoom button fits");
    await page.mouse.dblclick(clickPoint.x, clickPoint.y);
    await settle(page);
    near((await imageRect(page)).width, fit.width * 2, "a double click zooms once", 2);
    await page.waitForTimeout(400);
    await page.mouse.click(clickPoint.x, clickPoint.y);
    await settle(page);
    near((await imageRect(page)).width, fit.width, "a later click fits again");
    await page.getByRole("button", { name: "View image at full size", exact: true }).click();
    await settle(page);
    rect = await imageRect(page);
    near(rect.x + rect.width / 2, 600, "zoom button zooms around the centre");
    await page.getByRole("button", { name: "Fit image to screen", exact: true }).click();
    await settle(page);

    // Safari and the macOS desktop app report a trackpad pinch as gesture events.
    const pinchAt = { x: fit.x + fit.width * 0.45, y: fit.y + fit.height * 0.55 };
    assert.equal(await gesture(page, "gesturestart", 1, pinchAt.x, pinchAt.y), true);
    for (const scale of [1.4, 1.8, 2.5]) assert.equal(await gesture(page, "gesturechange", scale, pinchAt.x, pinchAt.y), true);
    await gesture(page, "gestureend", 2.5, pinchAt.x, pinchAt.y);
    rect = await imageRect(page);
    near(rect.width / fit.width, 2.5, "gesture scale", 0.01);
    assertUnder(rect, fraction(fit, pinchAt.x, pinchAt.y), pinchAt, "gesture zoom");
    // Pinching in past the fitted size stops there; the maximum is 4x here.
    await gesture(page, "gesturestart", 1, pinchAt.x, pinchAt.y);
    await gesture(page, "gesturechange", 0.1, pinchAt.x, pinchAt.y);
    await gesture(page, "gestureend", 0.1, pinchAt.x, pinchAt.y);
    near((await imageRect(page)).width, fit.width, "pinch in stops at fit");
    await gesture(page, "gesturestart", 1, pinchAt.x, pinchAt.y);
    await gesture(page, "gesturechange", 20, pinchAt.x, pinchAt.y);
    await gesture(page, "gestureend", 20, pinchAt.x, pinchAt.y);
    near((await imageRect(page)).width / fit.width, 4, "maximum zoom", 0.01);
    await page.getByRole("button", { name: "Fit image to screen", exact: true }).click();
    await settle(page);

    // A two-finger touch pinch zooms around and follows the midpoint; one finger then pans.
    const middle = { x: fit.x + fit.width * 0.5, y: fit.y + fit.height * 0.5 };
    // Ids 11 and 12 stay clear of the mouse pointer (id 1 in Chromium).
    await touch(page, "pointerdown", 11, middle.x - 50, middle.y);
    await touch(page, "pointerdown", 12, middle.x + 50, middle.y);
    await touch(page, "pointermove", 11, middle.x - 90, middle.y + 10);
    await touch(page, "pointermove", 12, middle.x + 110, middle.y + 10);
    rect = await imageRect(page);
    near(rect.width / fit.width, 2, "touch pinch scale", 0.01);
    assertUnder(rect, { x: 0.5, y: 0.5 }, { x: middle.x + 10, y: middle.y + 10 }, "touch pinch midpoint");
    await touch(page, "pointerup", 12, middle.x + 110, middle.y + 10);
    await touch(page, "pointermove", 11, middle.x - 120, middle.y + 10);
    await touch(page, "pointerup", 11, middle.x - 120, middle.y + 10);
    moved = await imageRect(page);
    near(moved.x, rect.x - 30, "one finger pans after a pinch");
    assert.equal(await zoomPressed(page), "true", "a pinch is not a tap");
    if (screenshots) await page.screenshot({ path: join(screenshots, `lightbox-zoomed-${name}.png`) });

    // A click outside the image closes the preview; so do Escape and Back.
    await page.getByRole("button", { name: "Fit image to screen", exact: true }).click();
    await settle(page);
    await page.mouse.click(20, 400);
    await dialog.waitFor({ state: "detached" });
    await open(page);
    await page.keyboard.press("Escape");
    await dialog.waitFor({ state: "detached" });
    await open(page);
    await page.evaluate(() => { history.pushState({}, ""); history.back(); });
    await dialog.waitFor({ state: "detached" });

    // The composer's draft preview uses the same viewer.
    await open(page, "Preview draft.svg");
    const draftFit = await imageRect(page);
    await page.mouse.move(draftFit.x + draftFit.width / 2, draftFit.y + draftFit.height / 2);
    await page.keyboard.down("Control");
    await page.mouse.wheel(0, -50);
    await page.keyboard.up("Control");
    near((await imageRect(page)).width / draftFit.width, Math.exp(0.5), "draft preview zooms", 0.01);
    await page.keyboard.press("Escape");
    await dialog.waitFor({ state: "detached" });
    assert.deepEqual(errors, []);

    // Phone: a real two-finger pinch zooms the image, not the page.
    if (name === "chromium") {
      const phone = await browser.newContext({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true, deviceScaleFactor: 3 });
      const mobile = await phone.newPage();
      mobile.setDefaultTimeout(6000);
      await mobile.goto(url);
      const phoneDialog = await open(mobile);
      const phoneFit = await imageRect(mobile);
      near(phoneFit.width, 358, "phone fitted width");
      const cdp = await phone.newCDPSession(mobile);
      const cx = phoneFit.x + phoneFit.width / 2;
      const cy = phoneFit.y + phoneFit.height / 2;
      const points = (spread) => [{ x: cx - spread, y: cy, id: 1 }, { x: cx + spread, y: cy, id: 2 }];
      await cdp.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: points(40) });
      for (const spread of [50, 60, 70, 80, 100]) await cdp.send("Input.dispatchTouchEvent", { type: "touchMove", touchPoints: points(spread) });
      await cdp.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
      rect = await imageRect(mobile);
      near(rect.width / phoneFit.width, 2.5, "real touch pinch scale", 0.02);
      near(rect.x + rect.width / 2, cx, "real touch pinch keeps the centre");
      assert.equal(await mobile.evaluate(() => visualViewport.scale), 1, "the page itself must not zoom");
      if (screenshots) await mobile.screenshot({ path: join(screenshots, "lightbox-phone-pinched.png") });
      await mobile.getByRole("button", { name: "Fit image to screen", exact: true }).tap();
      await settle(mobile);
      await mobile.touchscreen.tap(195, 120);
      await phoneDialog.waitFor({ state: "detached" });
      await phone.close();
    }

    console.log(`${name}: full-screen preview, ctrl+wheel and gesture pinch around the cursor, fit/max limits, scroll/drag/one-finger pan with edge limits, click/button toggles, touch pinch${name === "chromium" ? " (real touch on a phone viewport)" : ""}, close by outside click/Escape/Back, draft preview passed`);
    await browser.close(); browser = null;
  }
} finally {
  await browser?.close(); await server?.httpServer.close(); await rm(directory, { recursive: true, force: true });
}
