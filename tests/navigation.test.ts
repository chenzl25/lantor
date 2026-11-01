import assert from "node:assert/strict";
import test from "node:test";

import {
  initialNavigationState,
  navigationReducer,
  type NavigationAction,
  type NavigationState,
} from "../src/navigation";

const desktop = initialNavigationState(false);
const phone = initialNavigationState(true);

function run(start: NavigationState, ...actions: NavigationAction[]): NavigationState {
  return actions.reduce(navigationReducer, start);
}

test("phones start on Home, desktops on the channel", () => {
  assert.deepEqual(phone, {
    selectedAgentId: null,
    agentOrigin: null,
    threadRequested: false,
    showMobileSidebar: true,
    activeModal: null,
  });
  assert.equal(desktop.showMobileSidebar, false);
});

test("an agent opened from Home remembers it until the panel closes", () => {
  const opened = run(phone, { type: "hideHome" }, { type: "openAgent", agentId: "a", fromHome: true });
  assert.equal(opened.selectedAgentId, "a");
  assert.equal(opened.agentOrigin, "home");
  const closed = navigationReducer(opened, { type: "closeAgent" });
  assert.equal(closed.selectedAgentId, null);
  assert.equal(closed.agentOrigin, null);
});

test("an agent opened anywhere else has no origin", () => {
  assert.equal(run(desktop, { type: "openAgent", agentId: "a" }).agentOrigin, null);
  const reopened = run(
    desktop,
    { type: "openAgent", agentId: "a", fromHome: true },
    { type: "closeAgent" },
    { type: "openAgent", agentId: "a" },
  );
  assert.equal(reopened.agentOrigin, null);
});

test("switching to another agent drops the Home origin; reopening the same agent keeps it", () => {
  const fromHome = run(desktop, { type: "openAgent", agentId: "a", fromHome: true });
  const switched = navigationReducer(fromHome, { type: "openAgent", agentId: "b" });
  assert.equal(switched.selectedAgentId, "b");
  assert.equal(switched.agentOrigin, null);
  assert.equal(navigationReducer(fromHome, { type: "openAgent", agentId: "a" }), fromHome);
});

test("closing a given agent leaves another agent's panel open", () => {
  const opened = run(desktop, { type: "openAgent", agentId: "a" });
  assert.equal(navigationReducer(opened, { type: "closeAgent", agentId: "b" }), opened);
  assert.equal(navigationReducer(opened, { type: "closeAgent", agentId: "a" }).selectedAgentId, null);
});

test("revealing a thread closes the agent panel; restoring a channel's thread does not", () => {
  const withAgent = run(desktop, { type: "openAgent", agentId: "a", fromHome: true });
  const revealed = navigationReducer(withAgent, { type: "revealThread" });
  assert.equal(revealed.threadRequested, true);
  assert.equal(revealed.selectedAgentId, null);
  assert.equal(revealed.agentOrigin, null);

  const restored = navigationReducer(withAgent, { type: "restoreThreadPanel", open: true });
  assert.equal(restored.threadRequested, true);
  assert.equal(restored.selectedAgentId, "a");
  assert.equal(navigationReducer(restored, { type: "hideThread" }).threadRequested, false);
});

test("showing a channel leaves Home and the agent panel but keeps the full-screen panel", () => {
  const state = run(phone, { type: "openPanel", modal: "needs" }, { type: "openAgent", agentId: "a" }, { type: "showSidebar" });
  const shown = navigationReducer(state, { type: "showChannel" });
  assert.equal(shown.showMobileSidebar, false);
  assert.equal(shown.selectedAgentId, null);
  assert.equal(shown.activeModal, "needs");
});

test("Home closes the thread and agent panels; returning Home also closes the full-screen panel", () => {
  const busy = run(
    phone,
    { type: "hideHome" },
    { type: "revealThread" },
    { type: "openAgent", agentId: "a" },
    { type: "openPanel", modal: "search" },
  );
  const home = navigationReducer(busy, { type: "showHome" });
  assert.equal(home.showMobileSidebar, true);
  assert.equal(home.threadRequested, false);
  assert.equal(home.selectedAgentId, null);
  assert.equal(home.activeModal, "search");

  assert.deepEqual(navigationReducer(busy, { type: "returnHome" }), phone);
});

test("showing the sidebar overlay leaves the rest open", () => {
  const busy = run(desktop, { type: "revealThread" }, { type: "openAgent", agentId: "a" });
  assert.deepEqual(navigationReducer(busy, { type: "showSidebar" }), { ...busy, showMobileSidebar: true });
});

test("a full-screen panel replaces the open one and leaves Home", () => {
  const search = run(phone, { type: "openPanel", modal: "search" });
  assert.equal(search.activeModal, "search");
  assert.equal(search.showMobileSidebar, false);
  assert.equal(navigationReducer(search, { type: "openPanel", modal: "activity" }).activeModal, "activity");
});

test("closing a given panel leaves another panel open", () => {
  const saved = run(desktop, { type: "openPanel", modal: "saved" });
  assert.equal(navigationReducer(saved, { type: "closePanel", modal: "needs" }), saved);
  assert.equal(navigationReducer(saved, { type: "closePanel", modal: "saved" }).activeModal, null);
  assert.equal(navigationReducer(saved, { type: "closePanel" }).activeModal, null);
});

test("restoring a history entry shows its view; the agent's Home origin survives only for the same agent", () => {
  const fromHome = run(desktop, { type: "openAgent", agentId: "a", fromHome: true });
  const entry = { selectedAgentId: "b", threadRequested: true, showMobileSidebar: false, activeModal: "saved" } as const;
  assert.deepEqual(navigationReducer(fromHome, { type: "restore", ...entry }), { ...entry, agentOrigin: null });

  const sameAgent = navigationReducer(fromHome, {
    type: "restore",
    selectedAgentId: "a",
    threadRequested: false,
    showMobileSidebar: false,
    activeModal: null,
  });
  assert.equal(sameAgent, fromHome);
});

test("actions that change nothing return the same state, so React skips the render", () => {
  const unchanged: NavigationAction[] = [
    { type: "closeAgent" },
    { type: "hideThread" },
    { type: "restoreThreadPanel", open: false },
    { type: "showChannel" },
    { type: "hideHome" },
    { type: "closePanel" },
    { type: "restore", selectedAgentId: null, threadRequested: false, showMobileSidebar: false, activeModal: null },
  ];
  for (const action of unchanged) {
    assert.equal(navigationReducer(desktop, action), desktop, action.type);
  }
  assert.equal(navigationReducer(phone, { type: "returnHome" }), phone);
  assert.equal(navigationReducer(phone, { type: "showHome" }), phone);
  assert.equal(navigationReducer(phone, { type: "showSidebar" }), phone);
});
