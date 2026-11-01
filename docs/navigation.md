# Navigation

`App()` in `src/main.tsx` owns which view is showing: the channel, its thread,
the tab (Chat, Tasks, GitHub, Wiki), the agent panel, the full-screen panel
(Search, Activity, Saved, Needs you) and, on phones, Home (the sidebar).

## Desktop history

Above the phone breakpoint every view change writes one browser history entry,
so browser Back/Forward, the header's Go back/Go forward buttons and Mod+[ /
Mod+] all walk the same list.

- Opening a thread, switching channel or tab, opening an agent or a panel
  pushes an entry. Switching channel can also reopen that channel's thread;
  that stays one entry.
- Closing a thread replaces the current entry.
- Closing a panel (Close or Escape) or the agent panel's Close goes back one
  entry, so Forward reopens it.
- Switching straight from one panel to another replaces the entry.
- A new view after going back drops the forward entries, as browsers do.
- Each entry stores the view in `history.state` (`AppHistoryState`); reload
  keeps the current position without adding entries.

## Phone navigation

At or below the phone breakpoint the app writes no history entries: the iPhone
Home Screen app has no browser Back. Navigation is in-app:

- Home lists channels; tapping one opens it, and its Back returns Home.
- Tapping a message opens its thread; the thread's Back returns to the channel.
- The agent panel's Back closes it and shows the last channel.
- The bottom bar swaps panels in place; Home closes the open panel.

`npm run test:navigation` checks these rules in Chromium and WebKit.
