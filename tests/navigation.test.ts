import assert from "node:assert/strict";
import test from "node:test";

import {
  initialNavigationState,
  navigationReducer,
  type NavigationAction,
  type NavigationState,
} from "../src/navigation";

function run(...actions: NavigationAction[]): NavigationState {
  return actions.reduce(navigationReducer, initialNavigationState);
}

test("an agent opened from Home remembers it until the panel closes", () => {
  const opened = run({ type: "openAgent", agentId: "a", fromHome: true });
  assert.deepEqual(opened, { selectedAgentId: "a", agentOrigin: "home" });
  assert.deepEqual(navigationReducer(opened, { type: "closeAgent" }), initialNavigationState);
});

test("an agent opened anywhere else has no origin", () => {
  assert.deepEqual(run({ type: "openAgent", agentId: "a" }), { selectedAgentId: "a", agentOrigin: null });
  assert.deepEqual(
    run({ type: "openAgent", agentId: "a", fromHome: true }, { type: "closeAgent" }, { type: "openAgent", agentId: "a" }),
    { selectedAgentId: "a", agentOrigin: null },
  );
});

test("switching to another agent drops the Home origin; reopening the same agent keeps it", () => {
  const fromHome = run({ type: "openAgent", agentId: "a", fromHome: true });
  assert.deepEqual(navigationReducer(fromHome, { type: "openAgent", agentId: "b" }), {
    selectedAgentId: "b",
    agentOrigin: null,
  });
  assert.equal(navigationReducer(fromHome, { type: "openAgent", agentId: "a" }), fromHome);
});

test("closing a given agent leaves another agent's panel open", () => {
  const opened = run({ type: "openAgent", agentId: "a", fromHome: true });
  assert.equal(navigationReducer(opened, { type: "closeAgent", agentId: "b" }), opened);
  assert.deepEqual(navigationReducer(opened, { type: "closeAgent", agentId: "a" }), initialNavigationState);
});

test("restoring a history entry shows its agent without a Home origin", () => {
  const fromHome = run({ type: "openAgent", agentId: "a", fromHome: true });
  assert.deepEqual(navigationReducer(fromHome, { type: "restore", selectedAgentId: "b" }), {
    selectedAgentId: "b",
    agentOrigin: null,
  });
  assert.equal(navigationReducer(fromHome, { type: "restore", selectedAgentId: "a" }), fromHome);
  assert.deepEqual(navigationReducer(fromHome, { type: "restore", selectedAgentId: null }), initialNavigationState);
});

test("actions that change nothing return the same state, so React skips the render", () => {
  assert.equal(navigationReducer(initialNavigationState, { type: "closeAgent" }), initialNavigationState);
  assert.equal(
    navigationReducer(initialNavigationState, { type: "restore", selectedAgentId: null }),
    initialNavigationState,
  );
  const opened = run({ type: "openAgent", agentId: "a" });
  assert.equal(navigationReducer(opened, { type: "openAgent", agentId: "a" }), opened);
});
