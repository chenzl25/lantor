import assert from "node:assert/strict";
import test from "node:test";
import {
  FIT_VIEW, clampView, isZoomed, maxZoomScale, panView, pinchView, toggleZoomScale, wheelZoomFactor, zoomAt,
  type ZoomFrame, type ZoomView,
} from "../src/image-zoom";

// A 400x300 image fitted in an 800x600 stage, 60px from the top.
const frame: ZoomFrame = { stageWidth: 800, stageHeight: 600, left: 200, top: 60, width: 400, height: 300, maxScale: 8 };
const screen = (view: ZoomView, x: number, y: number) => ({
  x: frame.left + view.x + x * view.scale,
  y: frame.top + view.y + y * view.scale,
});
const close = (actual: number, expected: number) => assert.ok(Math.abs(actual - expected) < 1e-6, `${actual} != ${expected}`);

test("zooming keeps the image point under the cursor", () => {
  const view = zoomAt(FIT_VIEW, 3, { x: 500, y: 200 }, frame);
  assert.equal(view.scale, 3);
  // Image point (300, 140) was under the cursor at (500, 200).
  const point = screen(view, 300, 140);
  close(point.x, 500);
  close(point.y, 200);
  const further = zoomAt(view, 6, { x: 500, y: 200 }, frame);
  close(screen(further, 300, 140).x, 500);
  close(screen(further, 300, 140).y, 200);
});

test("zoom stays between the fitted size and the maximum", () => {
  assert.deepEqual(zoomAt(FIT_VIEW, 0.4, { x: 400, y: 300 }, frame), FIT_VIEW);
  assert.equal(zoomAt(FIT_VIEW, 50, { x: 400, y: 300 }, frame).scale, 8);
  const zoomed = zoomAt(FIT_VIEW, 4, { x: 250, y: 100 }, frame);
  assert.deepEqual(zoomAt(zoomed, 1, { x: 700, y: 500 }, frame), FIT_VIEW, "zooming out returns to the fitted image");
  assert.equal(isZoomed(FIT_VIEW), false);
  assert.equal(isZoomed(zoomed), true);
});

test("a zoomed image pans until its edges meet the stage edges", () => {
  const view = zoomAt(FIT_VIEW, 4, { x: 400, y: 210 }, frame); // 1600x1200
  const left = panView(view, 10_000, 10_000, frame);
  close(frame.left + left.x, 0);
  close(frame.top + left.y, 0);
  const right = panView(view, -10_000, -10_000, frame);
  close(frame.left + right.x + 1600, 800);
  close(frame.top + right.y + 1200, 600);
});

test("an image smaller than the stage stays centred and visible, and growing past the stage does not jump", () => {
  // At 1.5x the image is 600x450: narrower and shorter than the stage.
  const view = zoomAt(FIT_VIEW, 1.5, { x: 600, y: 360 }, frame);
  close(frame.left + view.x, 100);
  close(frame.top + view.y, 0);
  assert.deepEqual(panView(view, 50, 50, frame), view, "an image smaller than the stage cannot pan");
  // The fitted image is centred 90px above the stage centre, so its top edge
  // stops at the stage edge; it keeps moving continuously as the image grows
  // past the stage height.
  let previous = frame.top;
  for (let scale = 1; scale <= 2.6; scale += 0.01) {
    // Each step grows the image by 3px.
    const top = frame.top + zoomAt(FIT_VIEW, scale, { x: 400, y: 210 }, frame).y;
    assert.ok(Math.abs(top - previous) <= 3.01, `top jumped from ${previous} to ${top} at ${scale}`);
    previous = top;
  }
});

test("a pinch zooms by the finger spread and follows the fingers", () => {
  const view = pinchView(FIT_VIEW, 3, { x: 400, y: 210 }, { x: 420, y: 250 }, frame);
  // The image point under the starting midpoint is under the current midpoint.
  close(screen(view, 200, 150).x, 420);
  close(screen(view, 200, 150).y, 250);
});

test("clamping fixes a view after the stage shrinks", () => {
  const view = zoomAt(FIT_VIEW, 4, { x: 600, y: 360 }, frame);
  const narrow = { ...frame, stageWidth: 400, left: 0 };
  const clamped = clampView(view, narrow);
  assert.ok(narrow.left + clamped.x <= 0);
  assert.ok(narrow.left + clamped.x + 1600 >= 400);
});

test("pinch and wheel steps", () => {
  close(wheelZoomFactor(-100 * Math.log(1.2), 0), 1.2);
  close(wheelZoomFactor(120, 0), Math.exp(-0.5));
  close(wheelZoomFactor(-3, 1), Math.exp(0.48));
  assert.equal(maxZoomScale(1170, 360), 6.5);
  assert.equal(maxZoomScale(200, 200), 4);
  assert.equal(maxZoomScale(9000, 300), 16);
  assert.equal(maxZoomScale(0, 300), 4, "images without a natural size still zoom");
  assert.equal(toggleZoomScale({ ...frame, width: 360, maxScale: 6.5 }, 1170), 3.25);
  assert.equal(toggleZoomScale(frame, 300), 2, "a small image still doubles");
});
