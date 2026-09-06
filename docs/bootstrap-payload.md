# Compact bootstrap and channel unread counts

Web bootstrap and `currentChannelOnly` desktop bootstrap return summaries for the first screen. The unparameterized web route applies the same limits per channel. The full desktop bootstrap remains available.

| Collection | Compact response | Detail access |
| --- | --- | --- |
| Messages | Latest 20 roots per channel, latest 2 replies per root; artifact bodies omitted | Existing channel pagination; `load_thread_messages` on thread expansion; existing message/artifact lookup |
| Agents | Profile summary with `details_loaded: false`; no launch command, environment or workspace scan | `load_agent_detail` on drawer open or before editing |
| Work items | Latest 80 without context | Agent detail and existing scoped collection reads |
| Activities | Latest 3 per owner; detail capped at 240 characters; small scalar metadata retained | Agent detail returns up to 80 activities for that agent; search loads its collection context |
| Thread activity | Loaded roots and unread threads; includes total visible `reply_count` | Activity/search and thread expansion reload thread metadata |
| Inbox markers | Keys for returned entities, channels and the saved-message marker | Activity/search load complete marker collections |

Run logs and artifact bodies retain their existing on-demand APIs. Message bodies displayed in the first screen are not truncated, so 300 KB is a representative workload target, not a hard response limit for arbitrary message sizes. Reply counts come from the server even when only two replies are loaded. Opening a thread loads its complete history; pagination of an unusually large individual thread is a separate concern.

Lazy hydration preserves rows edited or removed after the request began. Agent editing waits for configuration hydration. Thread requests are cancelled logically on close/navigation and repeated after snapshot cursor changes, so replay-gap recovery can restore replies outside the compact window. Normal mutations, SSE replay, and the versioned `load_ui_state` invalidation contract remain unchanged; collection reads still return full rows.

Channel read markers store the maximum committed `messages.seq` in that channel. Unread counts use the `(channel_id, seq)` index and retain the existing owner, streaming, empty-placeholder, attachment and artifact visibility rules. Counts are an indexed count of eligible rows, not subtraction of global sequence numbers. Duplicate marks do not publish another refresh event.

Migration converts a legacy timestamp marker only when its read set can be represented exactly by one sequence watermark. Nonmonotonic imported histories retain timestamp comparison until the next mark-read operation. `last_read_at` remains for thread read compatibility. Migration equality is checked against a frozen copy of the old SQL, including timezone-equivalent timestamps and visibility exceptions.

Verification:

- `npm test` and `npm run build`.
- `npm run test:web-sync`: lazy workspace/edit configuration, complete thread expansion, normal mutations, stale responses, foreground recovery and real SSE disconnect/reconnect.
- Existing Markdown, message-row and avatar E2Es.
- `cargo test --manifest-path src-tauri/Cargo.toml` includes migration and real Axum bootstrap/detail contracts.
- Opt-in `benchmark_bootstrap_payload` uses `LANTOR_BENCH_SOURCE_DATABASE`, creates and deletes a disposable SQLite backup, and prints only aggregate bytes, timings, counts and unread equality. Six requests per route (discard first for timing), then 20 channel reads. It never prints profile fields or message content and must not benchmark by migrating the live database.
