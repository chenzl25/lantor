import assert from "node:assert/strict";
import test from "node:test";

import {
  initialNavigationState,
  navigationReducer,
  type NavigationAction,
  type NavigationState,
} from "../src/navigation";

const desktop = initialNavigationState({ activeChannelId: "c1", showMobileSidebar: false });
const phone = initialNavigationState({ activeChannelId: "c1", showMobileSidebar: true });

function run(start: NavigationState, ...actions: NavigationAction[]): NavigationState {
  return actions.reduce(navigationReducer, start);
}

test("phones start on Home, desktops on the channel", () => {
  assert.deepEqual(phone, {
    activeChannelId: "c1",
    activeThreadId: null,
    activeTab: "chat",
    channelThreadMemory: {},
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
  const entry = {
    activeChannelId: "c2",
    activeThreadId: "t2",
    activeTab: "tasks",
    selectedAgentId: "b",
    threadRequested: true,
    showMobileSidebar: false,
    activeModal: "saved",
  } as const;
  assert.deepEqual(navigationReducer(fromHome, { type: "restore", ...entry }), {
    ...entry,
    channelThreadMemory: { c2: "t2" },
    agentOrigin: null,
  });

  const sameAgent = navigationReducer(fromHome, {
    type: "restore",
    activeChannelId: "c1",
    activeThreadId: null,
    activeTab: "chat",
    selectedAgentId: "a",
    threadRequested: false,
    showMobileSidebar: false,
    activeModal: null,
  });
  assert.equal(sameAgent.selectedAgentId, "a");
  assert.equal(sameAgent.agentOrigin, "home");
  // Restoring a channel also remembers the entry's thread for it.
  assert.deepEqual(sameAgent.channelThreadMemory, { c1: null });
});

test("actions that change nothing return the same state, so React skips the render", () => {
  const unchanged: NavigationAction[] = [
    { type: "closeAgent" },
    { type: "hideThread" },
    { type: "restoreThreadPanel", open: false },
    { type: "showChannel" },
    { type: "hideHome" },
    { type: "closePanel" },
    {
      type: "restore",
      activeChannelId: null,
      activeThreadId: null,
      activeTab: "chat",
      selectedAgentId: null,
      threadRequested: false,
      showMobileSidebar: false,
      activeModal: null,
    },
    { type: "goToChannel", channelId: "c1" },
    { type: "resolveChannel", channels: [{ id: "c0" }, { id: "c1" }] },
    { type: "switchTab", tab: "chat" },
    { type: "clearThread" },
    { type: "forgetChannelThread", channelId: "c9" },
  ];
  for (const action of unchanged) {
    assert.equal(navigationReducer(desktop, action), desktop, action.type);
  }
  assert.equal(navigationReducer(phone, { type: "returnHome" }), phone);
  assert.equal(navigationReducer(phone, { type: "showHome" }), phone);
  assert.equal(navigationReducer(phone, { type: "showSidebar" }), phone);
});

test("going to a channel switches the tab only when one is given", () => {
  const tasks = run(desktop, { type: "switchTab", tab: "tasks" });
  const kept = navigationReducer(tasks, { type: "goToChannel", channelId: "c2" });
  assert.equal(kept.activeChannelId, "c2");
  assert.equal(kept.activeTab, "tasks");
  assert.equal(navigationReducer(tasks, { type: "goToChannel", channelId: "c2", tab: "chat" }).activeTab, "chat");
});

test("resolving the channel keeps it while it exists, else the preferred or first channel", () => {
  const channels = [{ id: "c0" }, { id: "c1" }];
  assert.equal(navigationReducer(desktop, { type: "resolveChannel", channels }), desktop);
  assert.equal(
    navigationReducer(desktop, { type: "resolveChannel", channels, preferredChannelId: "c0" }).activeChannelId,
    "c0",
  );
  assert.equal(navigationReducer(desktop, { type: "resolveChannel", channels: [{ id: "c5" }] }).activeChannelId, "c5");
  assert.equal(navigationReducer(desktop, { type: "resolveChannel", channels: [] }).activeChannelId, "");
});

test("selecting a thread remembers it for its channel; clearing it does not", () => {
  const selected = run(desktop, { type: "selectThread", threadId: "t1", channelId: "c1" });
  assert.equal(selected.activeThreadId, "t1");
  assert.deepEqual(selected.channelThreadMemory, { c1: "t1" });

  const cleared = navigationReducer(selected, { type: "clearThread" });
  assert.equal(cleared.activeThreadId, null);
  assert.deepEqual(cleared.channelThreadMemory, { c1: "t1" });

  const closed = navigationReducer(selected, { type: "selectThread", threadId: null, channelId: "c1" });
  assert.deepEqual(closed.channelThreadMemory, { c1: null });

  const noChannel = navigationReducer(selected, { type: "selectThread", threadId: "t2", channelId: null });
  assert.equal(noChannel.activeThreadId, "t2");
  assert.equal(noChannel.channelThreadMemory, selected.channelThreadMemory);
});

test("forgetting a channel's thread removes only that channel", () => {
  const remembered = run(
    desktop,
    { type: "selectThread", threadId: "t1", channelId: "c1" },
    { type: "selectThread", threadId: "t2", channelId: "c2" },
  );
  assert.deepEqual(
    navigationReducer(remembered, { type: "forgetChannelThread", channelId: "c1" }).channelThreadMemory,
    { c2: "t2" },
  );
});

test("a history entry without a channel keeps the current channel and thread", () => {
  const inThread = run(desktop, { type: "selectThread", threadId: "t1", channelId: "c1" });
  const restored = navigationReducer(inThread, {
    type: "restore",
    activeChannelId: null,
    activeThreadId: "ignored",
    activeTab: "wiki",
    selectedAgentId: null,
    threadRequested: false,
    showMobileSidebar: false,
    activeModal: null,
  });
  assert.equal(restored.activeChannelId, "c1");
  assert.equal(restored.activeThreadId, "t1");
  assert.equal(restored.activeTab, "wiki");
});
