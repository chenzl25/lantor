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
/** How long the surface takes to finish or snap back; matches the CSS. */
export const BACK_SWIPE_SETTLE_MS = 180;
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
