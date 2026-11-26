import assert from "node:assert/strict";
import test from "node:test";
import {
  followVisualViewport, KEYBOARD_INSET, VISUAL_VIEWPORT_HEIGHT, VISUAL_VIEWPORT_TOP, visualViewportBox,
} from "../src/visual-viewport";

class FakeViewport extends EventTarget {
  height = 852;
  offsetTop = 0;
  scale = 1;
  move(change: Partial<Pick<FakeViewport, "height" | "offsetTop" | "scale">>, type = "resize") {
    Object.assign(this, change);
    this.dispatchEvent(new Event(type));
  }
}

class FakeWindow extends EventTarget {
  scrollY = 0;
  scrolls: number[] = [];
  scrollTo(_x: number, y: number) { this.scrolls.push(y); this.scrollY = y; }
  scrollBy(y: number) { this.scrollY += y; this.dispatchEvent(new Event("scroll")); }
}

function fakeStyle() {
  const values = new Map<string, string>();
  const writes: string[] = [];
  return {
    values,
    writes,
    setProperty(name: string, value: string | null) { writes.push(name); values.set(name, value ?? ""); },
    removeProperty(name: string) { values.delete(name); return ""; },
  };
}

test("the box is what the keyboard leaves visible, wherever Safari panned to", () => {
  assert.deepEqual(visualViewportBox({ height: 852, offsetTop: 0, scale: 1 }, 852), { height: 852, top: 0, keyboardInset: 0 });
  // Keyboard up, the visual viewport panned down to show a field at the bottom.
  assert.deepEqual(visualViewportBox({ height: 516.4, offsetTop: 335.6, scale: 1 }, 852), { height: 516, top: 336, keyboardInset: 336 });
  // A pinch zoom shrinks the visual viewport without a keyboard: keep the layout.
  assert.equal(visualViewportBox({ height: 426, offsetTop: 200, scale: 2 }, 852), null);
});

test("the layout follows the keyboard and the pan, ignores zoom, and cleans up", () => {
  const viewport = new FakeViewport();
  const style = fakeStyle();
  const stop = followVisualViewport(viewport, style, () => 852, new FakeWindow());
  assert.equal(style.values.get(VISUAL_VIEWPORT_HEIGHT), "852px");
  assert.equal(style.values.get(VISUAL_VIEWPORT_TOP), "0px");
  assert.equal(style.values.get(KEYBOARD_INSET), "0px");

  viewport.move({ height: 516 });
  assert.equal(style.values.get(VISUAL_VIEWPORT_HEIGHT), "516px");
  assert.equal(style.values.get(KEYBOARD_INSET), "336px");
  viewport.move({ offsetTop: 336 }, "scroll");
  assert.equal(style.values.get(VISUAL_VIEWPORT_TOP), "336px");
  assert.equal(style.values.get(KEYBOARD_INSET), "336px");

  // An unchanged box writes nothing.
  const writes = style.writes.length;
  viewport.move({}, "scroll");
  assert.equal(style.writes.length, writes);

  // Zooming in keeps the last unzoomed box.
  viewport.move({ height: 258, offsetTop: 400, scale: 2 });
  assert.equal(style.values.get(VISUAL_VIEWPORT_HEIGHT), "516px");
  assert.equal(style.values.get(VISUAL_VIEWPORT_TOP), "336px");

  viewport.move({ height: 852, offsetTop: 0, scale: 1 });
  assert.equal(style.values.get(VISUAL_VIEWPORT_HEIGHT), "852px");
  assert.equal(style.values.get(KEYBOARD_INSET), "0px");

  stop();
  assert.equal(style.values.size, 0);
  viewport.move({ height: 400 });
  assert.equal(style.values.size, 0, "no updates after stopping");
});

test("the document stays at the top unless the page is zoomed", () => {
  const viewport = new FakeViewport();
  const win = new FakeWindow();
  win.scrollY = 120;
  const stop = followVisualViewport(viewport, fakeStyle(), () => 852, win);
  assert.deepEqual(win.scrolls, [0], "a document scrolled before startup goes back to the top");
  win.scrollBy(80);
  assert.deepEqual(win.scrolls, [0, 0]);
  viewport.scale = 2;
  win.scrollBy(80);
  assert.deepEqual(win.scrolls, [0, 0], "a zoomed page pans freely");
  viewport.scale = 1;
  stop();
  win.scrollBy(80);
  assert.deepEqual(win.scrolls, [0, 0], "no scrolling after stopping");
});

test("a browser without visualViewport keeps the dvh layout", () => {
  const style = fakeStyle();
  const stop = followVisualViewport(null, style, () => 852, new FakeWindow());
  assert.equal(style.values.size, 0);
  stop();
});
