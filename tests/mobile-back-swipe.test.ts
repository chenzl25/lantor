import assert from "node:assert/strict";
import test from "node:test";
import {
  addBackSwipeSample, backSwipeReleaseVelocity, completesBackSwipe, type BackSwipeSample,
} from "../src/mobile-back-swipe";

test("release speed comes from the last moments of the swipe", () => {
  const samples: BackSwipeSample[] = [];
  // A slow drag, then a quick flick just before lifting.
  for (const [x, t] of [[10, 0], [30, 200], [50, 400], [60, 560], [100, 640], [140, 660]]) addBackSwipeSample(samples, { x, t });
  assert.equal(backSwipeReleaseVelocity(samples, 665), 2, "(140 - 100) / 20 ms");
  // The same finger held still for a while before lifting has no speed.
  assert.equal(backSwipeReleaseVelocity(samples, 900), 0);
  assert.equal(backSwipeReleaseVelocity([{ x: 10, t: 0 }], 5), 0, "one sample has no speed");
});

test("only the latest samples are kept", () => {
  const samples: BackSwipeSample[] = [];
  for (let index = 0; index < 20; index += 1) addBackSwipeSample(samples, { x: index, t: index });
  assert.equal(samples.length, 8);
  assert.deepEqual(samples[0], { x: 12, t: 12 });
});

test("a swipe completes past a quarter of the width or with a flick, and a flick back cancels it", () => {
  const width = 400;
  assert.equal(completesBackSwipe(80, width, 0), false, "a short slow drag snaps back");
  assert.equal(completesBackSwipe(120, width, 0), true, "past 28% completes");
  assert.equal(completesBackSwipe(30, width, 0.6), true, "a short fast flick completes");
  assert.equal(completesBackSwipe(300, width, -0.6), false, "a flick back towards the edge cancels");
  assert.equal(completesBackSwipe(300, width, -0.2), true, "a slight drift back does not");
});
