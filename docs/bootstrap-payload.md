# Compact bootstrap and channel unread counts

Web bootstrap and `currentChannelOnly` desktop bootstrap return summaries for the first screen. The unparameterized web route applies the same limits per channel. The full desktop bootstrap remains available.

| Collection | Compact response | Detail access |
| --- | --- | --- |
| Messages | Latest 20 roots per channel, latest 2 replies per root; artifact bodies omitted | Existing channel pagination; `load_thread_page` on thread expansion ([thread pages](#thread-pages)); existing message/artifact lookup |
| Agents | Profile summary with `details_loaded: false`; no launch command, environment or workspace scan | `load_agent_detail` on drawer open or before editing |
| Work items | Latest 80 without context | Agent detail and existing scoped collection reads |
| Activities | Latest 3 per owner; detail capped at 240 characters; small scalar metadata retained | Agent detail returns up to 80 activities for that agent; search loads its collection context |
| Thread activity | Loaded roots and unread threads; includes total visible `reply_count` | Activity/search and thread expansion reload thread metadata |
| Inbox markers | Keys for returned entities, channels and the saved-message marker | Activity/search load complete marker collections |

Web responses (bootstrap, `load_ui_state`, `load_agent_detail`) replace inline `data:image/...;base64` avatars with immutable `/api/avatars/owner?v=<digest>` / `/api/avatars/agents/<id>?v=<digest>` URLs served with a content-digest ETag; base64 does not compress, so one uploaded avatar otherwise costs 100KB+ per payload. A web form that echoes such a URL back leaves the stored avatar unchanged. Desktop payloads keep the data URLs.

Run logs and artifact bodies retain their existing on-demand APIs. Message bodies displayed in the first screen are not truncated, so 300 KB is a representative workload target, not a hard response limit for arbitrary message sizes. Reply counts come from the server even when only two replies are loaded. Opening a thread loads its newest replies; see [thread pages](#thread-pages).

Lazy hydration preserves rows edited or removed after the request began. Agent editing waits for configuration hydration. Thread requests are cancelled logically on close/navigation and repeated after snapshot cursor changes, so replay-gap recovery can restore replies outside the compact window. Normal mutations, SSE replay, and the versioned `load_ui_state` invalidation contract remain unchanged; collection reads still return full rows.

Channel read markers store the maximum committed `messages.seq` in that channel. Unread counts use the `(channel_id, seq)` index and retain the existing owner, streaming, empty-placeholder, attachment and artifact visibility rules. Counts are an indexed count of eligible rows, not subtraction of global sequence numbers. Duplicate marks do not publish another refresh event.

Migration converts a legacy timestamp marker only when its read set can be represented exactly by one sequence watermark. Nonmonotonic imported histories retain timestamp comparison until the next mark-read operation. `last_read_at` remains for thread read compatibility. Migration equality is checked against a frozen copy of the old SQL, including timezone-equivalent timestamps and visibility exceptions.

## Thread pages

Opening a thread requests `load_thread_page` with `limit: 50`: the root plus its
50 newest replies, with `has_more` and `next_before_seq` (the oldest returned
reply's sequence). The thread shows the total reply count from its thread
activity, and **Show earlier replies** above the oldest loaded reply. Scrolling
up loads the previous page (`beforeSeq`) once that button is within one screen
of the top, keeping the reply that was first where it was on screen. The limit
is clamped to 1–200.

The client keeps one run per thread, from its oldest loaded reply to the newest.
Earlier replies that are already in memory, for example from Activity, search or
a quoted message, stay hidden until their page loads, so the thread never shows
a gap. Reopening the thread, or a snapshot cursor change while it is open,
requests `sinceSeq` set to the run's start (0 once the whole thread is loaded),
which returns every reply from there on; replies that arrived while the panel
was closed or the event stream had a gap are filled in. A jump to a reply older
than the run requests `sinceSeq` set to that reply, keeping it visible while the
replies after it load.

`load_thread_messages`, which returns the whole thread as one list, stays for
pages loaded before thread paging: a Home Screen app keeps its loaded page until
the owner taps **Refresh**.

### Finding in a thread

Cmd+F (or **Find in thread** in the thread header or its phone menu) opens a
find bar over the top of the open thread. Without an open thread, Cmd+F is left
to the browser. The bar counts every match in the thread, root included, and
moves between them in thread order: Enter or Cmd+G goes to the next match,
Shift+Enter or Shift+Cmd+G to the previous one, and Escape closes the bar.

- Matching ignores case and treats any whitespace as a space; matches do not
  overlap. `foldFindText` in `src/thread-find.ts` and `fold_for_find` in
  `message_store/search.rs` apply the same rule.
- Loaded messages are searched in their rendered text (the elements marked
  `data-find-text`), which includes collapsed long messages and code blocks but
  not buttons. Matches are painted with the CSS Custom Highlight API, so the
  message DOM is never rewritten.
- Replies older than the loaded run are counted by `find_in_thread`
  (`threadRootId`, `query`, `beforeSeq` set to the run's start), which returns
  each matching reply's id, seq and number of matches, not its body. A thread
  that is fully loaded never calls it.
- Moving to a match in an unloaded reply requests `load_thread_page` with
  `sinceSeq` set to that reply, as a jump does, then counts it again from the
  page. A reply that renders without the match (for example a hidden progress
  line) is skipped.
- Before scrolling to a match, its collapsed long message or code block
  expands. The scroll that places a match does not load the previous page, so
  the match stays in view; scrolling on from there loads it as usual.

## Mobile channel hydration

Mobile web skips `load_channel_previews`: Home consumes channel metadata, including indexed `latest_message_at` for recent sorting, not full messages from every channel. Selecting a channel requests `load_channel_messages` with `rootsOnly: true, limit: 30`; older pages use the same opt-in. Each page contains only timeline roots plus `thread_activities` for those roots, so the reply count/time and thread entry work without downloading replies. Opening the thread loads its newest page of replies. Desktop/legacy callers keep their existing defaults. Root pagination is clamped to 1–100, uses a strictly decreasing sequence cursor, and preserves the channel reading anchor through hydration.

Scoped event invalidations are coalesced at a maximum passive cadence of 2 requests/second, with a trailing refresh and per-scope version fences. Explicit user mutations can refresh immediately. Message bodies continue to arrive immediately through SSE. Failed invalidations are retained for foreground/periodic recovery, not retried in a tight loop.

Activity owner enumeration seeks distinct owner keys through a covering expression index instead of scanning every historical fact; deleted/anonymous owners retain their own summaries. Thread aggregation and indexed latest-reply lookup retain timestamp-instant ordering, empty-stream filtering and monotonic read-watermark semantics. Automatic channel read receipts carry the visible snapshot's `throughSeq`; explicit mark-all still reads the current maximum.

Verification:

- `npm test` and `npm run build`.
- `npm run test:thread-pages`: Chromium and WebKit (desktop and iPhone) checks for the newest page, scroll-up and button loading with a stable viewport, reopening, short threads and jumps to an old reply.
- `npm run test:thread-find`: Chromium and WebKit (desktop and iPhone) checks that Cmd+F counts loaded and unloaded matches, walks them in order, loads earlier replies, expands collapsed messages and code blocks, paints and clears highlights, and leaves a fully loaded thread to the page.
- `npm run test:mobile-performance` and `npm run test:channel-reading`: isolated Chromium/WebKit fixtures for lazy mobile navigation, roots/counts, passive gestures, refresh bursts, and reading-position races.
- `npm run test:web-sync`: lazy workspace/edit configuration, complete thread expansion, normal mutations, stale responses, foreground recovery and real SSE disconnect/reconnect.
- Existing Markdown, message-row and avatar E2Es.
- `cargo test --manifest-path src-tauri/Cargo.toml` includes migration and real Axum bootstrap/detail contracts.
- Opt-in `benchmark_bootstrap_payload` uses `LANTOR_BENCH_SOURCE_DATABASE`, creates and deletes a disposable SQLite backup, and prints only aggregate bytes, timings, counts and unread equality. Six requests per route (discard first for timing), then 20 channel reads. It never prints profile fields or message content and must not benchmark by migrating the live database.
