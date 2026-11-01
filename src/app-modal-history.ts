export const APP_MODALS = ["search", "activity", "saved", "needs"] as const;

/** The full-screen app panels. At most one is open at a time. */
export type AppModal = (typeof APP_MODALS)[number];

export function isAppModal(value: unknown): value is AppModal {
  return typeof value === "string" && (APP_MODALS as readonly string[]).includes(value);
}

/**
 * The open panel after `modal` is dismissed: dismissing a panel that is not
 * the open one leaves the open one alone.
 */
export function withoutAppModal(current: AppModal | null, modal: AppModal): AppModal | null {
  return current === modal ? null : current;
}

export type AppModalHistoryEntry = {
  index: number;
  activeModal: AppModal | null;
};

type ShouldPopAppModalHistoryInput = {
  activeModal: AppModal | null;
  expectedModal: AppModal;
  canNavigateBack: boolean;
  currentIndex: number;
  historyState: AppModalHistoryEntry | null;
};

export function shouldReplaceAppModalHistory(
  currentModal: AppModal | null,
  nextModal: AppModal,
) {
  return currentModal !== null && currentModal !== nextModal;
}

export function shouldReplaceActiveAppModalHistory({
  activeModal,
  currentIndex,
  historyState,
}: {
  activeModal: AppModal | null;
  currentIndex: number;
  historyState: AppModalHistoryEntry | null;
}) {
  return activeModal !== null
    && historyState?.index === currentIndex
    && historyState.activeModal === activeModal;
}

export function dismissAppModalHistoryEntry<T extends AppModalHistoryEntry>(
  currentState: T,
  targetIndex: number,
): T {
  return {
    ...currentState,
    index: targetIndex,
    activeModal: null,
  };
}

export function resolveAppModalHistoryPop<T extends AppModalHistoryEntry>(
  currentState: T | null,
  targetState: T | null,
): T | null {
  if (!currentState?.activeModal) return targetState;
  return dismissAppModalHistoryEntry(
    currentState,
    targetState?.index ?? Math.max(0, currentState.index - 1),
  );
}

export function shouldPopAppModalHistory({
  activeModal,
  expectedModal,
  canNavigateBack,
  currentIndex,
  historyState,
}: ShouldPopAppModalHistoryInput) {
  return activeModal === expectedModal
    && canNavigateBack
    && historyState?.index === currentIndex
    && historyState.activeModal === expectedModal;
}
