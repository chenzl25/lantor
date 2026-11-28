// The phone's swipe back from the left edge: the open thread or agent panel
// (or, with none open, the channel on its way to Home) follows the finger,
// and on release finishes or snaps back from where the finger left it.

/** A swipe starts this close to the left edge. */
export const BACK_SWIPE_EDGE_PX = 24;
/** More vertical travel than this before 72px across makes it a scroll. */
export const BACK_SWIPE_MAX_VERTICAL_PX = 48;
export const BACK_SWIPE_MIN_HORIZONTAL_PX = 72;
/** How far the surface moves as soon as the edge is touched. */
export const BACK_SWIPE_PEEK_PX = 18;
/** Released past this share of the width, the swipe completes. */
export const BACK_SWIPE_COMPLETE_FRACTION = 0.28;
/** A flick at least this fast, in px/ms, completes or cancels it. */
export const BACK_SWIPE_FLING_PX_PER_MS = 0.45;
/** How long the surface takes to finish or snap back after a swipe. */
export const BACK_SWIPE_SETTLE_MS = 180;
/** How long a page takes to slide in or out; matches --mobile-nav-duration. */
export const MOBILE_NAVIGATION_MS = 300;

export function prefersReducedMotion() {
  return typeof window !== "undefined" && window.matchMedia?.("(prefers-reduced-motion: reduce)").matches === true;
}
const VELOCITY_WINDOW_MS = 100;
const KEPT_SAMPLES = 8;

export type BackSwipeSample = { x: number; t: number };

export function addBackSwipeSample(samples: BackSwipeSample[], sample: BackSwipeSample) {
  samples.push(sample);
  if (samples.length > KEPT_SAMPLES) samples.splice(0, samples.length - KEPT_SAMPLES);
}

// The finger's speed over the last moments before `now`, in px/ms, positive to
// the right. A finger that stopped before lifting has no speed: touchmove only
// fires while it moves.
export function backSwipeReleaseVelocity(samples: readonly BackSwipeSample[], now: number) {
  const recent = samples.filter((sample) => now - sample.t <= VELOCITY_WINDOW_MS);
  if (recent.length < 2) return 0;
  const first = recent[0];
  const last = recent[recent.length - 1];
  return last.t > first.t ? (last.x - first.x) / (last.t - first.t) : 0;
}

export function completesBackSwipe(distance: number, width: number, velocity: number) {
  // A flick back towards the edge cancels however far the finger got.
  if (velocity <= -BACK_SWIPE_FLING_PX_PER_MS) return false;
  return distance >= width * BACK_SWIPE_COMPLETE_FRACTION || velocity >= BACK_SWIPE_FLING_PX_PER_MS;
}

export type BackSwipeSurface = "panel" | "sidebar";
/** The pages a back swipe moves: the panel over the channel, or Home under it. */
export type BackSwipePage = "panel" | "channel" | "home";

// Where each moving page sits while the swipe is `offset` px across a screen
// `width` px wide. The page underneath slides in from a third of the way
// back, as in a native navigation stack. These are written to each page's
// own style: a custom property on a common ancestor would make every element
// below it recompute its style on every frame.
export function backSwipeTransform(surface: BackSwipeSurface, page: BackSwipePage, offset: number, width: number) {
  const progress = Math.min(1, Math.max(0, offset / Math.max(1, width)));
  const behind = `${+(30 * (1 - progress)).toFixed(3)}vw`;
  if (surface === "panel") {
    return page === "panel" ? `translateX(${offset}px)` : `translateX(calc(-100vw - ${behind}))`;
  }
  return page === "home" ? `translateX(-${behind})` : `translateX(calc(-100vw + ${offset}px))`;
}

// --mobile-nav-ease, cubic-bezier(0.2, 0.8, 0.2, 1), at time `t` from 0 to 1:
// a finished or cancelled swipe eases into place on the curve page
// transitions use.
export function mobileNavEase(t: number) {
  if (t <= 0) return 0;
  if (t >= 1) return 1;
  const curve = (a: number, b: number, u: number) => 3 * a * u * (1 - u) ** 2 + 3 * b * u * u * (1 - u) + u ** 3;
  let low = 0, high = 1, u = t;
  for (let step = 0; step < 24; step += 1) {
    u = (low + high) / 2;
    if (curve(0.2, 0.2, u) < t) low = u;
    else high = u;
  }
  return curve(0.8, 1, u);
}
