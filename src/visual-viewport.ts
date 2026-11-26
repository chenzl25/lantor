// The on-screen keyboard on iPhone shrinks neither the layout viewport nor
// `100dvh`. Safari pans the visual viewport instead so the focused field
// shows, which pushes the header off screen and makes the composer jump. The
// phone layout sizes and places itself from the visual viewport, the part of
// the page the keyboard leaves visible, through these CSS variables.
export const VISUAL_VIEWPORT_HEIGHT = "--visual-viewport-height";
export const VISUAL_VIEWPORT_TOP = "--visual-viewport-top";
export const KEYBOARD_INSET = "--keyboard-inset";

type ViewportGeometry = Pick<VisualViewport, "height" | "offsetTop" | "scale">;
type ViewportSource = ViewportGeometry & Pick<VisualViewport, "addEventListener" | "removeEventListener">;
type StyleTarget = Pick<CSSStyleDeclaration, "setProperty" | "removeProperty">;
type ScrollingWindow = Pick<Window, "scrollY" | "scrollTo" | "addEventListener" | "removeEventListener">;

export type VisualViewportBox = { height: number; top: number; keyboardInset: number };

const isZoomed = (viewport: ViewportGeometry) => Math.abs(viewport.scale - 1) > 0.01;

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

export function followVisualViewport(
  viewport: ViewportSource | null | undefined = window.visualViewport,
  style: StyleTarget = document.documentElement.style,
  layoutHeight: () => number = () => document.documentElement.clientHeight,
  scrollingWindow: ScrollingWindow = window,
) {
  if (!viewport) return () => {};
  let applied = "";
  const apply = () => {
    const box = visualViewportBox(viewport, layoutHeight());
    if (!box) return;
    const key = `${box.height}:${box.top}:${box.keyboardInset}`;
    if (key === applied) return;
    applied = key;
    style.setProperty(VISUAL_VIEWPORT_HEIGHT, `${box.height}px`);
    style.setProperty(VISUAL_VIEWPORT_TOP, `${box.top}px`);
    style.setProperty(KEYBOARD_INSET, `${box.keyboardInset}px`);
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
    style.removeProperty(VISUAL_VIEWPORT_HEIGHT);
    style.removeProperty(VISUAL_VIEWPORT_TOP);
    style.removeProperty(KEYBOARD_INSET);
  };
}
