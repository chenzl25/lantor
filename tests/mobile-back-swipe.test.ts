import assert from "node:assert/strict";
import test from "node:test";
import {
  addBackSwipeSample, backSwipeReleaseVelocity, backSwipeTransform, completesBackSwipe, mobileNavEase, type BackSwipeSample,
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

test("the panel follows the finger while the page under it slides in from a third of the way back", () => {
  assert.equal(backSwipeTransform("panel", "panel", 120, 400), "translateX(120px)");
  assert.equal(backSwipeTransform("panel", "channel", 0, 400), "translateX(calc(-100vw - 30vw))");
  assert.equal(backSwipeTransform("panel", "channel", 100, 400), "translateX(calc(-100vw - 22.5vw))");
  assert.equal(backSwipeTransform("panel", "channel", 400, 400), "translateX(calc(-100vw - 0vw))");
  assert.equal(backSwipeTransform("sidebar", "channel", 120, 400), "translateX(calc(-100vw + 120px))");
  assert.equal(backSwipeTransform("sidebar", "home", 100, 400), "translateX(-22.5vw)");
  assert.equal(backSwipeTransform("sidebar", "home", 400, 400), "translateX(-0vw)");
});

test("a settling swipe eases out on the page transition curve", () => {
  assert.equal(mobileNavEase(0), 0);
  assert.equal(mobileNavEase(1), 1);
  // cubic-bezier(0.2, 0.8, 0.2, 1) covers most of the way early.
  assert.ok(Math.abs(mobileNavEase(0.5) - 0.9461) < 0.001, String(mobileNavEase(0.5)));
  let previous = 0;
  for (let t = 0.05; t <= 1; t += 0.05) {
    assert.ok(mobileNavEase(t) >= previous, `rises at ${t}`);
    previous = mobileNavEase(t);
  }
});
