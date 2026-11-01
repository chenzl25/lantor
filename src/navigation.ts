import { withoutAppModal, type AppModal } from "./app-modal-history";

/**
 * Which view `App()` shows, kept in one reducer so each navigation step is a
 * named action that can be tested without rendering. Fields move here from
 * `App()` one group at a time; docs/navigation.md lists the rules they follow.
 */
export type NavigationState = {
  /** The agent whose panel is open. */
  selectedAgentId: string | null;
  /**
   * Where the open agent panel came from. Phones send its Back to Home when it
   * was opened from Home; otherwise Back shows the channel under it.
   */
  agentOrigin: "home" | null;
  /** The thread panel is shown when this is set and a thread is active. */
  threadRequested: boolean;
  /** Home on phones; the sidebar overlay on narrow desktops. */
  showMobileSidebar: boolean;
  /** The full-screen panel (Search, Activity, Saved, Needs you). */
  activeModal: AppModal | null;
};

export type NavigationAction =
  /**
   * Opens an agent's panel. `fromHome` marks a panel opened from phone Home.
   * Opening the agent that is already open keeps its origin.
   */
  | { type: "openAgent"; agentId: string; fromHome?: boolean }
  /** Closes the agent panel, or only `agentId`'s panel when given. */
  | { type: "closeAgent"; agentId?: string }
  /** Opens the thread panel and closes the agent panel. */
  | { type: "revealThread" }
  /** Closes the thread panel; the thread stays active. */
  | { type: "hideThread" }
  /**
   * Shows or hides the thread panel to match a channel's remembered thread,
   * leaving the agent panel alone.
   */
  | { type: "restoreThreadPanel"; open: boolean }
  /** Leaves Home and the agent panel to show a channel. */
  | { type: "showChannel" }
  /** Hides Home. */
  | { type: "hideHome" }
  /** Shows Home over whatever is open. */
  | { type: "showSidebar" }
  /** Closes the thread and agent panels and shows Home. */
  | { type: "showHome" }
  /** Closes everything above Home, the full-screen panel included. */
  | { type: "returnHome" }
  /** Opens a full-screen panel, replacing any open one, and leaves Home. */
  | { type: "openPanel"; modal: AppModal }
  /** Closes the full-screen panel, or only `modal` when given. */
  | { type: "closePanel"; modal?: AppModal }
  /** Shows the view a browser history entry recorded. */
  | {
    type: "restore";
    selectedAgentId: string | null;
    threadRequested: boolean;
    showMobileSidebar: boolean;
    activeModal: AppModal | null;
  };

/** Phones start on Home. */
export function initialNavigationState(showMobileSidebar: boolean): NavigationState {
  return {
    selectedAgentId: null,
    agentOrigin: null,
    threadRequested: false,
    showMobileSidebar,
    activeModal: null,
  };
}

/** Returns `state` itself when an action changes nothing. */
export function navigationReducer(state: NavigationState, action: NavigationAction): NavigationState {
  switch (action.type) {
    case "openAgent":
      return showAgent(state, action.agentId, action.fromHome ? "home" : undefined);
    case "closeAgent":
      if (action.agentId !== undefined && action.agentId !== state.selectedAgentId) return state;
      return showAgent(state, null, undefined);
    case "revealThread":
      return patch(showAgent(state, null, undefined), { threadRequested: true });
    case "hideThread":
      return patch(state, { threadRequested: false });
    case "restoreThreadPanel":
      return patch(state, { threadRequested: action.open });
    case "showChannel":
      return patch(showAgent(state, null, undefined), { showMobileSidebar: false });
    case "hideHome":
      return patch(state, { showMobileSidebar: false });
    case "showSidebar":
      return patch(state, { showMobileSidebar: true });
    case "showHome":
      return patch(showAgent(state, null, undefined), { threadRequested: false, showMobileSidebar: true });
    case "returnHome":
      return patch(showAgent(state, null, undefined), {
        threadRequested: false,
        showMobileSidebar: true,
        activeModal: null,
      });
    case "openPanel":
      return patch(state, { showMobileSidebar: false, activeModal: action.modal });
    case "closePanel":
      return patch(state, {
        activeModal: action.modal === undefined ? null : withoutAppModal(state.activeModal, action.modal),
      });
    case "restore":
      return patch(showAgent(state, action.selectedAgentId, undefined), {
        threadRequested: action.threadRequested,
        showMobileSidebar: action.showMobileSidebar,
        activeModal: action.activeModal,
      });
  }
}

/** An origin of `undefined` keeps the current one while the same agent stays open. */
function showAgent(
  state: NavigationState,
  selectedAgentId: string | null,
  origin: NavigationState["agentOrigin"] | undefined,
): NavigationState {
  const sameAgent = selectedAgentId === state.selectedAgentId;
  const agentOrigin = selectedAgentId === null
    ? null
    : origin ?? (sameAgent ? state.agentOrigin : null);
  return patch(state, { selectedAgentId, agentOrigin });
}

function patch(state: NavigationState, changes: Partial<NavigationState>): NavigationState {
  const keys = Object.keys(changes) as (keyof NavigationState)[];
  return keys.every((key) => state[key] === changes[key]) ? state : { ...state, ...changes };
}
