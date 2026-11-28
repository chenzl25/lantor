import assert from "node:assert/strict";
import test from "node:test";
import {
  followVisualViewport, KEYBOARD_HEIGHT_KEY, KEYBOARD_INSET, keyboardEase, VIEWPORT_FOLLOWERS, VISUAL_VIEWPORT_HEIGHT,
  VISUAL_VIEWPORT_TOP, visualViewportBox, type KeyboardEnvironment, type VisualViewportBox,
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

// Focus and touch events, frames, timers and storage the test drives itself.
function fakeKeyboard(stored?: number) {
  const listeners = new Map<string, (event: Event) => void>();
  const frames = new Map<number, (now: number) => void>();
  const timers = new Map<number, { at: number; run: () => void }>();
  const values = new Map<string, string>(stored ? [[KEYBOARD_HEIGHT_KEY, String(stored)]] : []);
  let now = 0, ids = 0, active: unknown = null;
  const field = { composer: true, value: "draft", focused: 0, caret: -1,
    focus(options: FocusOptions) { assert.deepEqual(options, { preventScroll: true }); this.focused += 1; active = this; },
    setSelectionRange(start: number) { this.caret = start; },
    matches: () => true };
  const env: KeyboardEnvironment = {
    events: {
      addEventListener: (type: string, listener: EventListenerOrEventListenerObject) => { listeners.set(type, listener as (event: Event) => void); },
      removeEventListener: (type: string) => { listeners.delete(type); },
    } as KeyboardEnvironment["events"],
    activeElement: () => active as Element | null,
    isComposerField: (target): target is HTMLElement => Boolean((target as { composer?: boolean } | null)?.composer),
    storage: { getItem: (key) => values.get(key) ?? null, setItem: (key, value) => { values.set(key, value); } },
    frame: (callback) => { frames.set(++ids, callback); return ids; },
    cancelFrame: (id) => { frames.delete(id); },
    now: () => now,
    setTimer: (run, ms) => { timers.set(++ids, { at: now + ms, run }); return ids; },
    clearTimer: (id) => { timers.delete(id); },
  };
  const fire = (type: string, event: Partial<Event> & { changedTouches?: Array<{ clientX: number; clientY: number }> }) =>
    listeners.get(type)?.(event as unknown as Event);
  let prevented = 0;
  return {
    env, field, values, listeners,
    get prevented() { return prevented; },
    tap(target: unknown = field, move = 0, hold = 80) {
      fire("touchstart", { target: target as EventTarget, changedTouches: [{ clientX: 100, clientY: 700 }] });
      now += hold;
      fire("touchend", { target: target as EventTarget, cancelable: true, preventDefault: () => { prevented += 1; },
        changedTouches: [{ clientX: 100 + move, clientY: 700 }] });
    },
    blur() { active = null; fire("focusout", { target: field as unknown as EventTarget }); },
    // Runs frames and timers due within `ms`, one 16ms frame at a time.
    advance(ms: number) {
      const end = now + ms;
      while (now < end) {
        now = Math.min(end, now + 16);
        for (const [id, timer] of [...timers]) if (timer.at <= now) { timers.delete(id); timer.run(); }
        const due = [...frames]; frames.clear();
        for (const [, callback] of due) callback(now);
      }
    },
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
  const layout = fakeLayout();
  const stop = followVisualViewport(viewport, layout, () => 852, new FakeWindow(), phone, fakeKeyboard().env);
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
  const stop = followVisualViewport(viewport, layout, () => 852, new FakeWindow(), () => false, fakeKeyboard().env);
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
  const stop = followVisualViewport(viewport, fakeLayout(), () => 852, win, phone, fakeKeyboard().env);
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
  const stop = followVisualViewport(null, layout, () => 852, new FakeWindow(), phone, fakeKeyboard().env);
  stop();
  assert.deepEqual(layout.applied, []);
});

test("a tapped composer makes room for the keyboard as it rises, and gives it back as it falls", () => {
  const viewport = new FakeViewport();
  const layout = fakeLayout();
  const keyboard = fakeKeyboard(310);
  const stop = followVisualViewport(viewport, layout, () => 852, new FakeWindow(), phone, keyboard.env);
  keyboard.tap();
  assert.equal(keyboard.prevented, 1, "the browser does not focus (and scroll to) the field itself");
  assert.equal(keyboard.field.focused, 1);
  assert.equal(keyboard.field.caret, "draft".length, "the caret goes after the draft");
  keyboard.advance(150);
  const midway = layout.applied.at(-1)!;
  assert.ok(midway.keyboardInset > 0 && midway.keyboardInset < 310 && midway.top === 0 && midway.height === 852 - midway.keyboardInset,
    `midway ${JSON.stringify(midway)}`);
  keyboard.advance(200);
  assert.deepEqual(layout.applied.at(-1), { height: 542, top: 0, keyboardInset: 310 }, "it reaches the measured keyboard");
  const insets = layout.applied.map(box => box?.keyboardInset ?? 0);
  assert.deepEqual(insets, [...insets].sort((a, b) => a - b), "it only rises");
  // Safari's report replaces the guess and is remembered.
  viewport.move({ height: 530 });
  assert.deepEqual(layout.applied.at(-1), { height: 530, top: 0, keyboardInset: 322 });
  assert.equal(keyboard.values.get(KEYBOARD_HEIGHT_KEY), "322");

  const count = layout.applied.length;
  keyboard.blur();
  keyboard.advance(120);
  const falling = layout.applied.at(-1)!;
  assert.ok(layout.applied.length > count && falling.keyboardInset < 322 && falling.keyboardInset > 0, `falling ${JSON.stringify(falling)}`);
  keyboard.advance(200);
  assert.equal(layout.applied.at(-1), null, "the full layout is back before Safari reports it");
  viewport.move({ height: 852 });
  assert.equal(layout.applied.at(-1), null);
  stop();
});

test("without a keyboard measured yet the guess is 40% of the screen, and no keyboard takes it back", () => {
  const viewport = new FakeViewport();
  const layout = fakeLayout();
  const keyboard = fakeKeyboard();
  const stop = followVisualViewport(viewport, layout, () => 852, new FakeWindow(), phone, keyboard.env);
  keyboard.tap();
  keyboard.advance(400);
  assert.equal(layout.applied.at(-1)?.keyboardInset, 341);
  // A hardware keyboard: Safari never reports one.
  keyboard.advance(900);
  assert.equal(layout.applied.at(-1), null);
  stop();
});

test("drags, long presses, focused fields and wide screens keep the browser's handling", () => {
  for (const [name, act, wide] of [
    ["drag", (k: ReturnType<typeof fakeKeyboard>) => k.tap(undefined, 30), false],
    ["long press", (k: ReturnType<typeof fakeKeyboard>) => k.tap(undefined, 0, 700), false],
    ["other target", (k: ReturnType<typeof fakeKeyboard>) => k.tap({ composer: false }), false],
    ["focused", (k: ReturnType<typeof fakeKeyboard>) => { k.tap(); k.advance(400); k.tap(); }, false],
    ["wide", (k: ReturnType<typeof fakeKeyboard>) => k.tap(), true],
  ] as const) {
    const keyboard = fakeKeyboard(310);
    const layout = fakeLayout();
    const stop = followVisualViewport(new FakeViewport(), layout, () => 852, new FakeWindow(), () => !wide, keyboard.env);
    act(keyboard);
    keyboard.advance(400);
    const expected = name === "focused" ? 1 : 0;
    assert.equal(keyboard.prevented, expected, `${name}: taps the browser handles`);
    if (expected === 0) assert.deepEqual(layout.applied, [], `${name}: the layout waits for Safari`);
    stop();
  }
});

test("the keyboard motion eases out", () => {
  assert.equal(keyboardEase(0), 0);
  assert.equal(keyboardEase(1), 1);
  assert.ok(keyboardEase(0.5) > 0.8);
});

test("a keyboard reported before it stops moving is eased to, not jumped to", () => {
  const viewport = new FakeViewport();
  const layout = fakeLayout();
  const keyboard = fakeKeyboard(286);
  const stop = followVisualViewport(viewport, layout, () => 852, new FakeWindow(), phone, keyboard.env);
  keyboard.tap();
  keyboard.advance(96);
  const before = layout.applied.at(-1)!.keyboardInset;
  viewport.move({ height: 542 });
  keyboard.advance(16);
  const after = layout.applied.at(-1)!.keyboardInset;
  assert.ok(after > before && after < 310 - 20, `it keeps easing: ${before} then ${after}`);
  keyboard.advance(300);
  assert.deepEqual(layout.applied.at(-1), { height: 542, top: 0, keyboardInset: 310 });
  stop();
});
