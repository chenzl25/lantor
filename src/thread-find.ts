import type { ThreadFindHit } from "./types";

// Matching rules and match order for the thread find bar (Cmd+F).
// See docs/bootstrap-payload.md#finding-in-a-thread.

/**
 * Folds text for matching: each character is lowercased when that keeps its
 * UTF-16 length, and each whitespace character becomes a space, so offsets in
 * the folded text are offsets in the original. `fold_for_find` in
 * `src-tauri/src/message_store/search.rs` applies the same rule to replies
 * that are not loaded yet.
 */
export function foldFindText(text: string) {
  let folded = "";
  for (const char of text) {
    if (/\s/.test(char)) {
      folded += " ".repeat(char.length);
      continue;
    }
    const lower = char.toLowerCase();
    folded += lower.length === char.length ? lower : char;
  }
  return folded;
}

/** A query of only whitespace finds nothing; any other query matches as typed. */
export function findNeedle(query: string) {
  return query.trim() ? foldFindText(query) : "";
}

/** Start offsets of the non-overlapping matches of `needle` in `folded`. */
export function findOffsets(folded: string, needle: string) {
  const offsets: number[] = [];
  if (!needle) return offsets;
  let from = 0;
  for (;;) {
    const at = folded.indexOf(needle, from);
    if (at < 0) return offsets;
    offsets.push(at);
    from = at + needle.length;
  }
}

/** One message's matches, in the order the thread shows them. */
export type FindEntry = {
  messageId: string;
  seq: number;
  count: number;
  /** False for a reply whose page is not loaded; its count comes from the server. */
  loaded: boolean;
};

/** The current match: the `index`th match in a message. */
export type FindCursor = { messageId: string; index: number };

/**
 * Merges the rendered messages (in page order, root first) with server hits
 * for replies that are not loaded. A reply that is rendered is counted from
 * the page, even when it is older than the loaded run (a reply being jumped
 * to). Unsent replies have no seq yet and stay where the page shows them.
 */
export function mergeFindEntries(rendered: FindEntry[], hits: ThreadFindHit[]) {
  const shown = new Set(rendered.map((entry) => entry.messageId));
  const pending = hits
    .filter((hit) => hit.matches > 0 && !shown.has(hit.message_id))
    .sort((left, right) => left.seq - right.seq);
  const entries: FindEntry[] = [];
  let next = 0;
  rendered.forEach((entry, position) => {
    // The root stays first: every reply comes after it.
    if (position > 0 && entry.seq > 0) {
      while (next < pending.length && pending[next].seq < entry.seq) entries.push(unloadedEntry(pending[next++]));
    }
    if (entry.count > 0) entries.push(entry);
  });
  while (next < pending.length) entries.push(unloadedEntry(pending[next++]));
  return entries;
}

function unloadedEntry(hit: ThreadFindHit): FindEntry {
  return { messageId: hit.message_id, seq: hit.seq, count: hit.matches, loaded: false };
}

export function findTotal(entries: FindEntry[]) {
  return entries.reduce((total, entry) => total + entry.count, 0);
}

/** The 1-based position of the cursor among all matches, or 0 when it is not on one. */
export function findPosition(entries: FindEntry[], cursor: FindCursor | null) {
  if (!cursor) return 0;
  let before = 0;
  for (const entry of entries) {
    if (entry.messageId === cursor.messageId) {
      return cursor.index < entry.count ? before + cursor.index + 1 : 0;
    }
    before += entry.count;
  }
  return 0;
}

/** The match `step` places after (or before, when negative) the cursor, wrapping around. */
export function stepFind(entries: FindEntry[], cursor: FindCursor | null, step: 1 | -1): FindCursor | null {
  const total = findTotal(entries);
  if (total === 0) return null;
  const position = findPosition(entries, cursor);
  const next = position === 0
    ? (step > 0 ? 0 : total - 1)
    : (position - 1 + step + total) % total;
  return cursorAt(entries, next);
}

/** The cursor for the 0-based `position` among all matches. */
export function cursorAt(entries: FindEntry[], position: number): FindCursor | null {
  let remaining = position;
  for (const entry of entries) {
    if (remaining < entry.count) return { messageId: entry.messageId, index: remaining };
    remaining -= entry.count;
  }
  return null;
}

/**
 * Keeps the cursor on its message when the matches are counted again; a
 * message that lost its match moves the cursor to the next match after it.
 */
export function reconcileCursor(entries: FindEntry[], cursor: FindCursor | null, previous: FindEntry[]): FindCursor | null {
  if (!cursor) return null;
  const entry = entries.find((candidate) => candidate.messageId === cursor.messageId);
  if (entry) return { messageId: entry.messageId, index: Math.min(cursor.index, entry.count - 1) };
  // The message no longer matches: continue from the first message after it.
  const order = previous.map((candidate) => candidate.messageId);
  const at = order.indexOf(cursor.messageId);
  for (const messageId of order.slice(at + 1)) {
    if (entries.some((candidate) => candidate.messageId === messageId)) return { messageId, index: 0 };
  }
  return entries.length ? { messageId: entries[0].messageId, index: 0 } : null;
}
