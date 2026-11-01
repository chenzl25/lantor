import assert from "node:assert/strict";
import test from "node:test";

import {
  dismissAppModalHistoryEntry,
  isAppModal,
  resolveAppModalHistoryPop,
  shouldPopAppModalHistory,
  shouldReplaceActiveAppModalHistory,
  shouldReplaceAppModalHistory,
  withoutAppModal,
} from "../src/app-modal-history";
import { shouldDismissOnEscape, type EscapeDismissEvent } from "../src/escape-dismiss";

function escapeEvent(overrides: Partial<EscapeDismissEvent> = {}): EscapeDismissEvent {
  return {
    key: "Escape",
    defaultPrevented: false,
    isComposing: false,
    keyCode: 27,
    repeat: false,
    ...overrides,
  };
}

test("switching between app modals replaces the current history entry", () => {
  assert.equal(shouldReplaceAppModalHistory("activity", "search"), true);
  assert.equal(shouldReplaceAppModalHistory("search", "search"), false);
  assert.equal(shouldReplaceAppModalHistory(null, "search"), false);
});

test("background changes replace the current history entry while a modal stays open", () => {
  const input = {
    activeModal: "activity" as const,
    currentIndex: 4,
    historyState: { index: 4, activeModal: "activity" as const },
  };

  assert.equal(shouldReplaceActiveAppModalHistory(input), true);
  assert.equal(
    shouldReplaceActiveAppModalHistory({
      ...input,
      historyState: { index: 3, activeModal: "activity" },
    }),
    false,
  );
  assert.equal(
    shouldReplaceActiveAppModalHistory({
      ...input,
      historyState: { index: 4, activeModal: "search" },
    }),
    false,
  );
  assert.equal(shouldReplaceActiveAppModalHistory({ ...input, activeModal: null }), false);
});

test("dismissing a modal preserves its current background surface", () => {
  const current = {
    index: 5,
    activeModal: "activity" as const,
    activeChannelId: "channel-current",
    activeThreadId: "thread-current",
    showThread: true,
  };

  assert.deepEqual(dismissAppModalHistoryEntry(current, 4), {
    ...current,
    index: 4,
    activeModal: null,
  });
});

test("one history pop closes Activity even when the target entry also has Activity open", () => {
  const current = {
    index: 5,
    activeModal: "activity" as const,
    activeChannelId: "channel-current",
    activeThreadId: "thread-current",
    showThread: true,
  };
  const staleTarget = {
    index: 4,
    activeModal: "activity" as const,
    activeChannelId: "channel-old",
    activeThreadId: "thread-old",
    showThread: true,
  };

  assert.deepEqual(resolveAppModalHistoryPop(current, staleTarget), {
    ...current,
    index: staleTarget.index,
    activeModal: null,
  });
  assert.equal(resolveAppModalHistoryPop({ ...current, activeModal: null }, staleTarget), staleTarget);
});

test("closing an app modal only pops its matching current history entry", () => {
  const input = {
    activeModal: "search" as const,
    expectedModal: "search" as const,
    canNavigateBack: true,
    currentIndex: 3,
    historyState: { index: 3, activeModal: "search" as const },
  };

  assert.equal(shouldPopAppModalHistory(input), true);
  assert.equal(
    shouldPopAppModalHistory({
      ...input,
      historyState: { index: 3, activeModal: "activity" },
    }),
    false,
  );
  assert.equal(
    shouldPopAppModalHistory({
      ...input,
      historyState: { index: 2, activeModal: "search" },
    }),
    false,
  );
  assert.equal(
    shouldPopAppModalHistory({ ...input, activeModal: "activity" }),
    false,
  );
  assert.equal(shouldPopAppModalHistory({ ...input, canNavigateBack: false }), false);
});

test("Escape dismisses one surface only for an unhandled initial keydown", () => {
  assert.equal(shouldDismissOnEscape(escapeEvent()), true);
  assert.equal(shouldDismissOnEscape(escapeEvent({ repeat: true })), false);
  assert.equal(shouldDismissOnEscape(escapeEvent({ defaultPrevented: true })), false);
  assert.equal(shouldDismissOnEscape(escapeEvent({ isComposing: true })), false);
  assert.equal(shouldDismissOnEscape(escapeEvent({ keyCode: 229 })), false);
  assert.equal(shouldDismissOnEscape(escapeEvent({ key: "Enter" })), false);
});

test("dismissing a panel closes it only when it is the open one", () => {
  assert.equal(withoutAppModal("search", "search"), null);
  assert.equal(withoutAppModal("activity", "search"), "activity");
  assert.equal(withoutAppModal(null, "saved"), null);
});

test("only the four app panels are app modals", () => {
  for (const modal of ["search", "activity", "saved", "needs"]) assert.ok(isAppModal(modal), modal);
  for (const value of [null, undefined, "", "settings", 1]) assert.ok(!isAppModal(value), String(value));
});
