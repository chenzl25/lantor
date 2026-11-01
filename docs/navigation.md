# Navigation

`App()` in `src/main.tsx` owns which view is showing: the channel, its thread,
the tab (Chat, Tasks, GitHub, Wiki), the agent panel, the full-screen panel
(Search, Activity, Saved, Needs you) and, on phones, Home (the sidebar).

## State

`navigate` in `src/navigation.ts` is the reducer for the navigation state: it
applies a step with `navigationReducer` and records how the step writes
history. The state covers the channel, its active thread and tab, the thread
each channel last showed, and which surfaces are open (Home, the thread panel,
the agent panel and where it was opened from, the full-screen panel). Each
change is a named action, unit tested in `tests/navigation.test.ts`; an action
that opens one surface closes the ones it covers, for example `returnHome`
closes everything above Home. The focused message is a short highlight, not a
view, so it stays `App()` state; the history entry records it from a ref.

## Desktop history

Above the phone breakpoint every view change writes one browser history entry,
so browser Back/Forward, the header's Go back/Go forward buttons and Mod+[ /
Mod+] all walk the same list.

- Opening a thread, switching channel or tab, opening an agent or a panel
  pushes an entry. Switching channel can also reopen that channel's thread;
  that stays one entry.
- Closing a thread replaces the current entry.
- Closing a panel or the agent panel (Close or Escape) goes back one entry,
  so Forward reopens it.
- Switching straight from one panel to another replaces the entry.
- Opening a quoted message or thread pushes one entry; Back returns to the
  quoting message and highlights it. This holds when the quote is in the same
  channel and Back changes nothing else on screen: the next view still gets
  its own entry.
- A new view after going back drops the forward entries, as browsers do.
- Each entry stores the view in `history.state` (`AppHistoryState`); reload
  keeps the current position without adding entries.
- Entries are written in a layout effect, while React commits the view, so a
  Back that arrives right after a change cannot find an entry still pending.
- Each navigation step says how it writes history (`HistoryMode`: push,
  replace or none). Steps dispatched before the effect runs share one entry,
  and the strongest mode wins: none (a restore from history) over replace over
  push. A handler that needs a different mode passes it to
  `dispatchNavigation` or wraps its steps in `withHistoryMode`.

## Phone navigation

At or below the phone breakpoint the app writes no history entries: the iPhone
Home Screen app has no browser Back. Navigation is in-app:

- Home lists channels; tapping one opens it, and its Back returns Home.
- Tapping a message opens its thread; the thread's Back returns to the channel.
- The agent panel's Back returns to where it was opened: Home, or the channel
  it was opened from.
- The bottom bar swaps panels in place; Home closes the open panel.

`npm run test:navigation` checks these rules in Chromium and WebKit.
