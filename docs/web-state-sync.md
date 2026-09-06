# Web state synchronization

`bootstrap` initializes the UI and captures the durable `ui_events.id` cursor before loading state. Normal writes no longer wait for another bootstrap. Message responses replace optimistic rows by id; committed entity events update the other clients. Refresh events invalidate only the collections listed in `src/ui-state-sync.ts`, read through `load_ui_state`. That endpoint does not read message history, run logs, or artifact content.

SSE reconnects pass the last delivered cursor in the `cursor` query parameter (the server also accepts native `Last-Event-ID`). The browser's fixed retry is disabled by closing failed EventSources. Retries use exponential delay with jitter, capped at 30 seconds; a connection must remain open for 10 seconds before resetting the backoff. Closing a subscription cancels its timers and ignores late deliveries.

Visibility, focus, pageshow and online events coalesce into one foreground reconciliation. The 60-second safety net also checks the durable event cursor, and skips hidden tabs and text editing. An up-to-date check returns no events. HTTP replay and SSE share a delivery cursor, so overlapping replay does not apply deltas twice. The event cursor is used instead of `messages.seq` because edits, deletes, read markers and task changes do not create a new message sequence. A fresh page gets its cursor from its fresh bootstrap; a restored page retains its existing cursor.

Expired/pruned cursors and unsupported event payloads retain snapshot recovery. Streaming messages with a missing baseline first use targeted `load_message` hydration. Collection reads are single-flight, coalesced by scope, and invalidated if newer events arrive while a read is in flight. Failed scopes remain pending for a later foreground or periodic retry. Automatic channel read marking waits 300ms, requires unread state, and is disabled while hidden.

When adding a mutation, publish its affected entity or a scoped invalidation in the same database transaction. If it writes a message directly, include a message event; a task collection update cannot recreate a missing message body. Extend `scopesForRefresh` when adding a refresh reason. This path reuses existing store readers. Compact bootstrap and its lazy detail readers are documented in [bootstrap-payload.md](bootstrap-payload.md).

Validation:

- `npm test`: state patches, optimistic preservation, cursor deduplication, backoff and disposal.
- `npm run build` followed by `npm run test:web-sync`: production React UI and real EventSource against an isolated synthetic HTTP service; send/read/task operations, stale collection response, simulated hidden lifecycle for 65 seconds, real SSE outage for 10 seconds, and unchanged foreground checks.
- `cargo test --manifest-path src-tauri/Cargo.toml`: real SQLite and Axum mutation, scoped read and replay contracts, alongside existing backend coverage.
