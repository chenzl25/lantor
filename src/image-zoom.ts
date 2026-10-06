// Geometry for the image lightbox. The image keeps its fitted CSS layout and
// is drawn with `translate(x, y) scale(scale)` from its top-left corner, so a
// view of { scale: 1, x: 0, y: 0 } is the fitted image.

export type ZoomView = { scale: number; x: number; y: number };

// The fitted image box and its stage, in stage pixels.
export type ZoomFrame = {
  stageWidth: number;
  stageHeight: number;
  left: number;
  top: number;
  width: number;
  height: number;
  maxScale: number;
};

export const FIT_VIEW: ZoomView = { scale: 1, x: 0, y: 0 };

function clamp(value: number, min: number, max: number) {
  return Math.min(Math.max(value, min), max);
}

// The deepest zoom shows the image at twice its natural size, within 4x-16x
// of the fitted size.
export function maxZoomScale(naturalWidth: number, fittedWidth: number) {
  if (fittedWidth <= 0) return 4;
  return clamp((2 * (naturalWidth || fittedWidth)) / fittedWidth, 4, 16);
}

// A click or the zoom button shows the image at its natural size, and at
// least twice the fitted size for images that already fit.
export function toggleZoomScale(frame: ZoomFrame, naturalWidth: number) {
  return clamp((naturalWidth || frame.width) / frame.width, 2, frame.maxScale);
}

// Along one axis, an image no longer than the stage stays as close to its
// fitted centre as the stage allows; a longer one pans until its edge meets
// the stage edge. Both limits meet when the image is as long as the stage.
function clampAxis(offset: number, start: number, fitted: number, scale: number, stage: number) {
  const length = fitted * scale;
  if (length <= stage) return clamp(start + (fitted - length) / 2, 0, stage - length) - start;
  return clamp(start + offset, stage - length, 0) - start;
}

export function clampView(view: ZoomView, frame: ZoomFrame): ZoomView {
  const scale = clamp(view.scale, 1, frame.maxScale);
  return {
    scale,
    x: clampAxis(view.x, frame.left, frame.width, scale, frame.stageWidth),
    y: clampAxis(view.y, frame.top, frame.height, scale, frame.stageHeight),
  };
}

// Moves the image so the image point that was under `from` lies under `to`
// at `scale`: a two-finger pinch that also pans.
export function pinchView(start: ZoomView, scale: number, from: { x: number; y: number }, to: { x: number; y: number }, frame: ZoomFrame): ZoomView {
  const next = clamp(scale, 1, frame.maxScale);
  const ratio = next / start.scale;
  return clampView({
    scale: next,
    x: to.x - frame.left - (from.x - frame.left - start.x) * ratio,
    y: to.y - frame.top - (from.y - frame.top - start.y) * ratio,
  }, frame);
}

// Zooms to `scale` while the image point under the stage point stays under it.
export function zoomAt(view: ZoomView, scale: number, point: { x: number; y: number }, frame: ZoomFrame): ZoomView {
  return pinchView(view, scale, point, point, frame);
}

export function panView(view: ZoomView, dx: number, dy: number, frame: ZoomFrame): ZoomView {
  return clampView({ scale: view.scale, x: view.x + dx, y: view.y + dy }, frame);
}

// Wheel deltas in pixels. Lines and pages use common browser sizes.
export function wheelPixels(delta: number, deltaMode: number) {
  return delta * (deltaMode === 1 ? 16 : deltaMode === 2 ? 400 : 1);
}

// Trackpad pinches arrive as ctrl+wheel events whose deltaY is
// -100 * ln(scale change) in Chromium, so this follows the fingers exactly.
// One mouse-wheel notch is capped to a moderate step.
export function wheelZoomFactor(deltaY: number, deltaMode: number) {
  return Math.exp(-clamp(wheelPixels(deltaY, deltaMode), -50, 50) / 100);
}

export function isZoomed(view: ZoomView) {
  return view.scale > 1.001;
}
