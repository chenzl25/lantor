# Navigation

`App()` in `src/main.tsx` owns which view is showing: the channel, its thread,
the tab (Chat, Tasks, GitHub, Wiki), the agent panel, the full-screen panel
(Search, Activity, Saved, Needs you, GitHub notifications) and, on phones, Home
(the sidebar).

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
Home Screen app has no browser Back. Phones start on Home, except
that a Home Screen app reloaded within an hour of leaving reopens the channel
and thread it showed ([resume copy](web-app-shell.md#resume-copy-on-phones)).
Navigation is in-app:

- Home lists channels; tapping one opens it, and its Back returns Home.
- Tapping a message opens its thread; the thread's Back returns to the channel.
- The agent panel's Back returns to where it was opened: Home, or the channel
  it was opened from.
- The bottom bar (Home, Activity, Needs you, GitHub, Search) swaps panels in
  place; Home closes the open panel.

Pages move like a navigation stack, all with one timing
(`--mobile-nav-duration`, 300ms, and `--mobile-nav-ease`): a channel slides
in over Home, which waits a third of the way back under it, and a thread or
agent panel slides in over the channel, which waits a third of the way back
in turn. Home and the channel stay mounted, so their CSS transitions also
cover the way back. A thread or agent panel unmounts when it closes, so its
Back button first slides it out the way a finished swipe does, and closes it
afterwards. A channel's messages stay hidden until they sit at their reading
position, then rise 24px into place from the reply box over 260ms. The
scroller itself moves, clipped by its shell, so its scroll geometry, and with
it the read position, stays put while it rises. Reopening the channel that is
already open shows its messages at once. On any width, "Back to where you left
off…" shows only once a channel has been hidden for 400ms, so a loaded
channel, placed within a frame or two, opens without it. With reduced motion
every move is instant.

Neither the channel nor a thread pans sideways on a phone: their scrollers
hide horizontal overflow, as no phone app lets a whole conversation move
under the finger. Wide code blocks and tables scroll in their own box. A
GitHub link keeps its state icons on one line only when its text is the
reference itself (`#123`, `rw#123`, `owner/repo#123`, `RW #123`); longer link
text wraps like the sentence around it instead of running past the screen.

A swipe from the left edge (the first 24px) goes back: it drags the open
thread or agent panel off to the right over the channel, which slides in from
a third of the way back, or, with neither open, drags the channel aside to
Home. The surface follows the finger through CSS variables on the app element
(`--mobile-swipe-x`, `--mobile-swipe-progress`, `--mobile-swipe-duration`,
with `data-mobile-swipe` naming the surface), written once per frame without
rendering React. On release the swipe completes past 28% of the width or with
a flick of 0.45 px/ms over its last 100 ms, and a flick back towards the edge
cancels it; otherwise it snaps back. A completed swipe first slides the surface
the rest of the way and only then navigates, so the panel leaves the screen
instead of vanishing. Dialogs, a second finger, and a mostly vertical drag
cancel it. The rules are in `src/mobile-back-swipe.ts`.

`npm run test:navigation` checks these rules in Chromium and WebKit;
`npm run test:phone-controls` checks the edge swipe, the rise and that
nothing pans sideways.
