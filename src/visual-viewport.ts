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

export function followVisualViewport(
  viewport: ViewportSource | null | undefined = window.visualViewport,
  layout: ViewportLayout = pageViewportLayout(),
  layoutHeight: () => number = () => document.documentElement.clientHeight,
  scrollingWindow: ScrollingWindow = window,
  isPhoneLayout: () => boolean = () => window.matchMedia(PHONE_LAYOUT).matches,
) {
  if (!viewport) return () => {};
  let applied = "";
  const apply = () => {
    const box = visualViewportBox(viewport, layoutHeight());
    if (!box) return;
    // A visual viewport that covers the layout is what `100dvh` already gives.
    const follow = isPhoneLayout() && (box.keyboardInset > 0 || box.top > 0) ? box : null;
    const key = follow ? `${follow.height}:${follow.top}:${follow.keyboardInset}` : "";
    if (key === applied) return;
    applied = key;
    layout.apply(follow);
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
  apply();
  keepDocumentAtTop();
  return () => {
    viewport.removeEventListener("resize", apply);
    viewport.removeEventListener("scroll", apply);
    scrollingWindow.removeEventListener("scroll", keepDocumentAtTop);
    if (applied) layout.apply(null);
  };
}
