// The on-screen keyboard on iPhone shrinks neither the layout viewport nor
// `100dvh`. Safari pans the visual viewport instead so the focused field
// shows, which pushes the header off screen and makes the composer jump. While
// the keyboard is up, the phone layout sizes and places itself from the visual
// viewport, the part of the page the keyboard leaves visible.
//
// The values go onto the elements that use them, never onto a common
// ancestor: a custom property there makes every element below it recompute
// its style whenever the keyboard moves, which took 26ms with a 50-reply
// thread open in WebKit. The app and its pages hold every message, so they
// take concrete heights and positions. The composers and dialog backdrops are
// small, so they take the variables their CSS (and that of the panels inside
// them) reads. With no keyboard nothing is written and the CSS keeps `100dvh`.
export const VISUAL_VIEWPORT_HEIGHT = "--visual-viewport-height";
export const VISUAL_VIEWPORT_TOP = "--visual-viewport-top";
export const KEYBOARD_INSET = "--keyboard-inset";

type ViewportGeometry = Pick<VisualViewport, "height" | "offsetTop" | "scale">;
type ViewportSource = ViewportGeometry & Pick<VisualViewport, "addEventListener" | "removeEventListener">;
type ScrollingWindow = Pick<Window, "scrollY" | "scrollTo" | "addEventListener" | "removeEventListener">;

export type VisualViewportBox = { height: number; top: number; keyboardInset: number };
/** Applies a box to the page, or with null takes back what it applied. */
export type ViewportLayout = { apply(box: VisualViewportBox | null): void };

const isZoomed = (viewport: ViewportGeometry) => Math.abs(viewport.scale - 1) > 0.01;
const PHONE_LAYOUT = "(max-width: 760px)";

// The box the layout should occupy, or null while the page is pinch-zoomed:
// zooming shrinks the visual viewport too, and the layout keeps its size then.
// `top` is the pan from the layout viewport's top; the keyboard inset is the
// height the keyboard takes from the layout, wherever Safari panned to.
export function visualViewportBox(viewport: ViewportGeometry, layoutHeight: number): VisualViewportBox | null {
  if (isZoomed(viewport)) return null;
  const height = Math.round(viewport.height);
  return {
    height,
    top: Math.max(0, Math.round(viewport.offsetTop)),
    keyboardInset: Math.max(0, Math.round(layoutHeight) - height),
  };
}

const px = (value: number) => `${value}px`;

/** Each element of the phone layout that follows the box, and the styles it takes. */
export const VIEWPORT_FOLLOWERS: ReadonlyArray<{ selector: string; styles(box: VisualViewportBox): Record<string, string> }> = [
  { selector: ".app", styles: (box) => ({ height: px(box.height), "margin-top": px(box.top) }) },
  { selector: ".app .sidebar, .app .conversation", styles: (box) => ({ height: px(box.height) }) },
  { selector: ".thread, .agent-drawer", styles: (box) => ({ top: px(box.top), height: px(box.height) }) },
  {
    selector: ".composer, .reply-composer, .search-backdrop, .modal-backdrop",
    styles: (box) => ({
      [VISUAL_VIEWPORT_HEIGHT]: px(box.height),
      [VISUAL_VIEWPORT_TOP]: px(box.top),
      [KEYBOARD_INSET]: px(box.keyboardInset),
    }),
  },
];

// Writes the box to every follower on the page, including one that mounts
// while the keyboard is up, and removes exactly what it wrote.
export function pageViewportLayout(root: HTMLElement = document.body): ViewportLayout {
  const written = new Map<HTMLElement, string[]>();
  let box: VisualViewportBox | null = null;
  let observer: MutationObserver | null = null;
  const write = (scope: Element) => {
    if (!box) return;
    for (const { selector, styles } of VIEWPORT_FOLLOWERS) {
      const matches = [...(scope.matches(selector) ? [scope] : []), ...scope.querySelectorAll(selector)];
      for (const element of matches) {
        if (!(element instanceof HTMLElement)) continue;
        const values = styles(box);
        for (const [property, value] of Object.entries(values)) element.style.setProperty(property, value);
        written.set(element, Object.keys(values));
      }
    }
  };
  const clear = () => {
    for (const [element, properties] of written) for (const property of properties) element.style.removeProperty(property);
    written.clear();
  };
  return {
    apply(next) {
      box = next;
      if (!next) {
        observer?.disconnect();
        observer = null;
        clear();
        return;
      }
      write(root);
      if (observer || typeof MutationObserver === "undefined") return;
      observer = new MutationObserver((records) => {
        for (const record of records) for (const node of record.addedNodes) if (node instanceof Element) write(node);
      });
      observer.observe(root, { childList: true, subtree: true });
    },
  };
}

// The keyboard's own events come late: Safari reports the smaller visual
// viewport only once the keyboard has finished rising (about 300ms after the
// tap) and the full one only once it has gone (about 550ms after the field
// lost focus). Followed alone, the thread jumps up after the keyboard stops
// and the reply box floats over an empty space while it slides away. So the
// layout moves with the keyboard instead: from the moment a composer is
// tapped it eases towards the keyboard height measured last time, and from
// the moment it loses focus back to the full height; the real values replace
// the guess as soon as they arrive.
export const KEYBOARD_HEIGHT_KEY = "lantor:keyboard-height";
const KEYBOARD_SHOW_MS = 300;
const KEYBOARD_HIDE_MS = 250;
// No keyboard by then (a hardware keyboard, say): the layout goes back.
const KEYBOARD_EXPECT_MS = 900;
// Before any keyboard was measured: about what an iPhone's takes in portrait.
const KEYBOARD_GUESS_SHARE = 0.4;
// Smaller differences are a browser bar or rounding, not a keyboard.
const KEYBOARD_MIN_PX = 100;
// A touch that moved further or lasted longer is a drag or a long press.
const TAP_MAX_MOVE_PX = 10;
const TAP_MAX_MS = 500;

export const keyboardEase = (t: number) => 1 - (1 - Math.min(1, Math.max(0, t))) ** 3;

export type KeyboardEnvironment = {
  /** Where focus and touch events arrive (the document). */
  events: Pick<Document, "addEventListener" | "removeEventListener">;
  activeElement: () => Element | null;
  /** A text field of a composer, whose keyboard the layout moves with. */
  isComposerField: (target: EventTarget | null) => target is HTMLElement;
  storage: Pick<Storage, "getItem" | "setItem"> | null;
  frame: (callback: (now: number) => void) => number;
  cancelFrame: (id: number) => void;
  now: () => number;
  setTimer: (callback: () => void, ms: number) => number;
  clearTimer: (id: number) => void;
};

function browserKeyboardEnvironment(): KeyboardEnvironment {
  let storage: Storage | null = null;
  try { storage = window.localStorage; } catch { storage = null; }
  return {
    events: document,
    activeElement: () => document.activeElement,
    isComposerField: (target): target is HTMLElement =>
      target instanceof HTMLTextAreaElement && Boolean(target.closest(".composer, .reply-composer")),
    storage,
    frame: (callback) => window.requestAnimationFrame(callback),
    cancelFrame: (id) => window.cancelAnimationFrame(id),
    now: () => performance.now(),
    setTimer: (callback, ms) => window.setTimeout(callback, ms),
    clearTimer: (id) => window.clearTimeout(id),
  };
}

export function followVisualViewport(
  viewport: ViewportSource | null | undefined = window.visualViewport,
  layout: ViewportLayout = pageViewportLayout(),
  layoutHeight: () => number = () => document.documentElement.clientHeight,
  scrollingWindow: ScrollingWindow = window,
  isPhoneLayout: () => boolean = () => window.matchMedia(PHONE_LAYOUT).matches,
  keyboard: KeyboardEnvironment = browserKeyboardEnvironment(),
) {
  if (!viewport) return () => {};
  // The last box the visual viewport reported ("" for none), and the box the
  // layout shows now, which runs ahead of it while the keyboard moves.
  let applied = "";
  let real: VisualViewportBox | null = null;
  let shown: VisualViewportBox | null = null;
  let motion = 0;
  let motionTarget = 0;
  let motionEnd = 0;
  let expectTimer = 0;
  const show = (box: VisualViewportBox | null) => {
    shown = box;
    layout.apply(box);
  };
  const stopMotion = () => {
    if (motion) keyboard.cancelFrame(motion);
    motion = 0;
    if (expectTimer) keyboard.clearTimer(expectTimer);
    expectTimer = 0;
  };
  const apply = () => {
    const box = visualViewportBox(viewport, layoutHeight());
    if (!box) return;
    // A visual viewport that covers the layout is what `100dvh` already gives.
    const follow = isPhoneLayout() && (box.keyboardInset > 0 || box.top > 0) ? box : null;
    const key = follow ? `${follow.height}:${follow.top}:${follow.keyboardInset}` : "";
    if (key === applied) return;
    applied = key;
    real = follow;
    if (follow && follow.keyboardInset >= KEYBOARD_MIN_PX) keyboard.storage?.setItem(KEYBOARD_HEIGHT_KEY, String(follow.keyboardInset));
    // Safari can report the keyboard before it has finished moving: the
    // motion under way carries on to the real values instead of jumping.
    if (motion && follow && motionTarget > 0) {
      if (expectTimer) keyboard.clearTimer(expectTimer);
      expectTimer = 0;
      move(follow.keyboardInset, Math.max(60, motionEnd - keyboard.now()), () => show(follow));
      return;
    }
    if (motion && !follow && motionTarget === 0) return;
    stopMotion();
    show(follow);
  };
  // Eases the layout from the keyboard height it shows now to `to`, then
  // shows `done` (the full layout when the keyboard is going away).
  const move = (to: number, ms: number, done?: () => void) => {
    if (motion) keyboard.cancelFrame(motion);
    const full = layoutHeight();
    const from = shown?.keyboardInset ?? 0;
    const start = keyboard.now();
    motionTarget = to;
    motionEnd = start + ms;
    const step = (now: number) => {
      const t = Math.min(1, Math.max(0, (now - start) / ms));
      const inset = Math.round(from + (to - from) * keyboardEase(t));
      show(inset > 0 ? { height: full - inset, top: 0, keyboardInset: inset } : null);
      if (t < 1) motion = keyboard.frame(step);
      else { motion = 0; done?.(); }
    };
    motion = keyboard.frame(step);
  };
  const expectedKeyboard = () => {
    const stored = Number(keyboard.storage?.getItem(KEYBOARD_HEIGHT_KEY));
    return stored >= KEYBOARD_MIN_PX ? stored : Math.round(layoutHeight() * KEYBOARD_GUESS_SHARE);
  };
  const keyboardOpening = () => {
    if (!isPhoneLayout()) return;
    if (expectTimer) keyboard.clearTimer(expectTimer);
    expectTimer = 0;
    // Refocused before Safari reported the last keyboard gone: it stays.
    if (real) { const box = real; move(box.keyboardInset, KEYBOARD_SHOW_MS, () => show(box)); return; }
    move(expectedKeyboard(), KEYBOARD_SHOW_MS);
    expectTimer = keyboard.setTimer(() => {
      expectTimer = 0;
      if (!applied) move(0, KEYBOARD_HIDE_MS);
    }, KEYBOARD_EXPECT_MS);
  };
  const onFocusOut = (event: Event) => {
    if (!keyboard.isComposerField(event.target)) return;
    // Focus may be moving to another field, which keeps the keyboard.
    keyboard.setTimer(() => {
      const active = keyboard.activeElement();
      if (active && active.matches("input, textarea, [contenteditable]")) return;
      if (!shown) return;
      if (expectTimer) keyboard.clearTimer(expectTimer);
      expectTimer = 0;
      move(0, KEYBOARD_HIDE_MS);
    }, 0);
  };
  // Safari scrolls the whole page up by the keyboard's height to reveal a
  // field it takes to be covered, even one already above the keyboard, and
  // only gives it back after the page has visibly jumped. A tap on a composer
  // that is not focused yet therefore focuses it without scrolling, and the
  // layout makes room for the keyboard itself. Drags and long presses (paste)
  // keep the browser's own handling.
  let touch: { x: number; y: number; at: number } | null = null;
  const point = (event: Event) => (event as TouchEvent).changedTouches?.[0];
  const onTouchStart = (event: Event) => {
    const first = point(event);
    touch = first && keyboard.isComposerField(event.target) ? { x: first.clientX, y: first.clientY, at: keyboard.now() } : null;
  };
  const onTouchEnd = (event: Event) => {
    const field = event.target, last = point(event), start = touch;
    touch = null;
    if (!start || !last || !isPhoneLayout() || !keyboard.isComposerField(field) || keyboard.activeElement() === field) return;
    if (Math.hypot(last.clientX - start.x, last.clientY - start.y) > TAP_MAX_MOVE_PX || keyboard.now() - start.at > TAP_MAX_MS) return;
    if (!event.cancelable) return;
    event.preventDefault();
    field.focus({ preventScroll: true });
    // The tap no longer places the caret: put it after any draft.
    const text = field as Partial<HTMLTextAreaElement>;
    if (typeof text.value === "string" && typeof text.setSelectionRange === "function") text.setSelectionRange(text.value.length, text.value.length);
    keyboardOpening();
  };
  // The phone layout scrolls inside its own panes, never the document. Keeping
  // the document at the top makes the visual viewport's pan the only offset,
  // shared by the in-flow layout and the fixed panels. A pinch zoom pans freely.
  const keepDocumentAtTop = () => {
    if (scrollingWindow.scrollY !== 0 && !isZoomed(viewport)) scrollingWindow.scrollTo(0, 0);
  };
  viewport.addEventListener("resize", apply);
  viewport.addEventListener("scroll", apply);
  scrollingWindow.addEventListener("scroll", keepDocumentAtTop);
  keyboard.events.addEventListener("focusout", onFocusOut, true);
  keyboard.events.addEventListener("touchstart", onTouchStart, { capture: true, passive: true });
  keyboard.events.addEventListener("touchend", onTouchEnd, { capture: true, passive: false });
  apply();
  keepDocumentAtTop();
  return () => {
    viewport.removeEventListener("resize", apply);
    viewport.removeEventListener("scroll", apply);
    scrollingWindow.removeEventListener("scroll", keepDocumentAtTop);
    keyboard.events.removeEventListener("focusout", onFocusOut, true);
    keyboard.events.removeEventListener("touchstart", onTouchStart, { capture: true });
    keyboard.events.removeEventListener("touchend", onTouchEnd, { capture: true });
    stopMotion();
    if (shown) layout.apply(null);
  };
}
