import assert from "node:assert/strict";
import test from "node:test";
import {
  followVisualViewport, KEYBOARD_INSET, VIEWPORT_FOLLOWERS, VISUAL_VIEWPORT_HEIGHT, VISUAL_VIEWPORT_TOP, visualViewportBox,
  type VisualViewportBox,
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

function fakeLayout() {
  const applied: Array<VisualViewportBox | null> = [];
  return { applied, apply(box: VisualViewportBox | null) { applied.push(box); } };
}
const phone = () => true;

test("the box is what the keyboard leaves visible, wherever Safari panned to", () => {
  assert.deepEqual(visualViewportBox({ height: 852, offsetTop: 0, scale: 1 }, 852), { height: 852, top: 0, keyboardInset: 0 });
  // Keyboard up, the visual viewport panned down to show a field at the bottom.
  assert.deepEqual(visualViewportBox({ height: 516.4, offsetTop: 335.6, scale: 1 }, 852), { height: 516, top: 336, keyboardInset: 336 });
  // A pinch zoom shrinks the visual viewport without a keyboard: keep the layout.
  assert.equal(visualViewportBox({ height: 426, offsetTop: 200, scale: 2 }, 852), null);
});

test("the layout follows the keyboard and the pan, ignores zoom, and cleans up", () => {
  const viewport = new FakeViewport();
  const layout = fakeLayout();
  const stop = followVisualViewport(viewport, layout, () => 852, new FakeWindow(), phone);
  assert.deepEqual(layout.applied, [], "with no keyboard the CSS keeps 100dvh");

  viewport.move({ height: 516 });
  viewport.move({ offsetTop: 336 }, "scroll");
  assert.deepEqual(layout.applied, [{ height: 516, top: 0, keyboardInset: 336 }, { height: 516, top: 336, keyboardInset: 336 }]);

  // An unchanged box, and a zoomed one, apply nothing: zoom keeps the last box.
  viewport.move({}, "scroll");
  viewport.move({ height: 258, offsetTop: 400, scale: 2 });
  assert.equal(layout.applied.length, 2);

  viewport.move({ height: 852, offsetTop: 0, scale: 1 });
  assert.equal(layout.applied.at(-1), null, "the keyboard going down takes the box back");

  viewport.move({ height: 516 });
  stop();
  assert.equal(layout.applied.at(-1), null, "stopping takes the box back");
  const count = layout.applied.length;
  viewport.move({ height: 400 });
  assert.equal(layout.applied.length, count, "no updates after stopping");
});

test("above the phone breakpoint the layout ignores the keyboard", () => {
  const viewport = new FakeViewport();
  const layout = fakeLayout();
  const stop = followVisualViewport(viewport, layout, () => 852, new FakeWindow(), () => false);
  viewport.move({ height: 516, offsetTop: 336 });
  stop();
  assert.deepEqual(layout.applied, []);
});

test("the app and its pages take concrete sizes; small followers take the variables", () => {
  const box = { height: 516, top: 336, keyboardInset: 336 };
  assert.deepEqual(VIEWPORT_FOLLOWERS.map(({ selector, styles }) => [selector, styles(box)]), [
    [".app", { height: "516px", "margin-top": "336px" }],
    [".app .sidebar, .app .conversation", { height: "516px" }],
    [".thread, .agent-drawer", { top: "336px", height: "516px" }],
    [".composer, .reply-composer, .search-backdrop, .modal-backdrop",
      { [VISUAL_VIEWPORT_HEIGHT]: "516px", [VISUAL_VIEWPORT_TOP]: "336px", [KEYBOARD_INSET]: "336px" }],
  ]);
});

test("the document stays at the top unless the page is zoomed", () => {
  const viewport = new FakeViewport();
  const win = new FakeWindow();
  win.scrollY = 120;
  const stop = followVisualViewport(viewport, fakeLayout(), () => 852, win, phone);
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
  const layout = fakeLayout();
  const stop = followVisualViewport(null, layout, () => 852, new FakeWindow(), phone);
  stop();
  assert.deepEqual(layout.applied, []);
});
