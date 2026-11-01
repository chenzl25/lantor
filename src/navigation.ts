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
};

export type NavigationAction =
  /**
   * Opens an agent's panel. `fromHome` marks a panel opened from phone Home.
   * Opening the agent that is already open keeps its origin.
   */
  | { type: "openAgent"; agentId: string; fromHome?: boolean }
  /** Closes the agent panel, or only `agentId`'s panel when given. */
  | { type: "closeAgent"; agentId?: string }
  /** Shows the view a browser history entry recorded. */
  | { type: "restore"; selectedAgentId: string | null };

export const initialNavigationState: NavigationState = {
  selectedAgentId: null,
  agentOrigin: null,
};

/** Returns `state` itself when an action changes nothing. */
export function navigationReducer(state: NavigationState, action: NavigationAction): NavigationState {
  switch (action.type) {
    case "openAgent":
      return showAgent(state, action.agentId, action.fromHome ? "home" : undefined);
    case "closeAgent":
      if (action.agentId !== undefined && action.agentId !== state.selectedAgentId) return state;
      return showAgent(state, null, undefined);
    case "restore":
      return showAgent(state, action.selectedAgentId, undefined);
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
  if (sameAgent && agentOrigin === state.agentOrigin) return state;
  return { ...state, selectedAgentId, agentOrigin };
}
