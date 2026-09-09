# Owner Activity

The owner's Activity modal uses `load_activity_feed`, independently of chat
message hydration. `load_activity_counts` supplies total and unread badges.
These endpoints are available over both Web and Tauri transports.

- Filter and dismissal/read eligibility are evaluated in SQLite before pagination.
  All kinds sort newest first, with item ID as the stable tie breaker. Owner
  replies update thread recency without adding unread counts.
- Each page returns at most 30 items. Cursors contain timestamp and item ID;
  `after` loads older items and `before` loads newer items. Only one cursor may
  be supplied. A cursor does not require its source row to still exist.
- SQLite selects at most 31 metadata rows, then reads text excerpts capped at
  2048 characters. Titles, actor names, and channel labels are capped too.
  Message attachments and artifact bodies are not loaded by this API.
- The modal holds one page and two cursors. Navigation replaces the page;
  rapid filter changes serialize requests and retain only the last queued
  selection. Closing releases the page; reopening starts with latest activity.
- Updates show a refresh prompt without reshuffling the page being read.
  Page read/dismiss actions use each item's displayed timestamp, so later
  activity survives. They do not mark whole channels read.
- Badge queries are coalesced, limited to one in flight and one start per five
  seconds, and paused in hidden tabs. Counts are independent of the selected
  page. A failed refresh retains the last successful badge.

This bounds Activity payloads, frontend cache, and rendered rows. Database
query cost still grows with history: SQLite aggregates message metadata and
checks mention text. It is not a constant-time materialized feed. The pressure
regression reports query latency and payload for 5000 threads / 10000 messages.

Verify with `npm test`, `npm run build`, `npm run test:activity-feed`, and
`cargo test --manifest-path src-tauri/Cargo.toml owner_inbox -- --nocapture`.
The browser test uses an isolated fixture server and does not mutate live inbox
state. The Rust tests use disposable SQLite databases.
