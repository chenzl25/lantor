import type { Message } from "./types";

export type MessageScrollAnchor = { messageId: string; seq: number; offset: number };
export type ChannelReadingPosition = {
  anchor: MessageScrollAnchor;
  atBottom: boolean;
  latestRootId: string;
  /** Highest top-level message seq this page has shown on screen. */
  seenSeq: number;
};

// Positions last for this page session only. After a reload the server's read
// marker decides where a channel opens, so another device's reading counts.
const LEGACY_STORAGE_KEY = "lantor.channelReadingPositions.v1";
const MAX_CHANNELS = 128;
const positions = new Map<string, ChannelReadingPosition>();
if (typeof window !== "undefined") {
  try { window.localStorage.removeItem(LEGACY_STORAGE_KEY); } catch { /* storage may be unavailable */ }
}

export function readChannelPosition(channelId: string) {
  return positions.get(channelId) ?? null;
}

export function rememberChannelPosition(channelId: string, position: ChannelReadingPosition) {
  // Optimistic sends have seq=0 and can disappear on failure. Never turn one
  // into an anchor that would make restoration page back to sequence 0.
  if (position.anchor.seq <= 0) return;
  positions.delete(channelId);
  positions.set(channelId, position);
  while (positions.size > MAX_CHANNELS) positions.delete(positions.keys().next().value!);
}

/** The first top-level message to open at, given the server's first unread
 * root and what this page already showed. Own messages never count as new. */
export function firstUnreadRoot(roots: readonly Message[], firstUnreadSeq: number | null | undefined, seenSeq = 0) {
  if (firstUnreadSeq == null) return null;
  const from = Math.max(firstUnreadSeq, seenSeq + 1);
  return roots.find((message) => message.seq >= from && message.sender_role !== "owner") ?? null;
}

/** Messages below what has been shown, for the back-to-bottom button. */
export function countUnseenRoots(roots: readonly Message[], seenSeq: number) {
  let count = 0;
  for (let index = roots.length - 1; index >= 0 && roots[index].seq > seenSeq; index -= 1) {
    if (roots[index].sender_role !== "owner") count += 1;
  }
  return count;
}

function messageRows(viewport: HTMLElement) {
  return viewport.querySelectorAll<HTMLElement>("article[data-message-id]");
}

// Inspect only outer row boxes, using binary search even for long histories.
// Do not force layout of every content-visibility-skipped Markdown descendant.
export function captureMessageAnchor(viewport: HTMLElement): MessageScrollAnchor | null {
  const top = viewport.getBoundingClientRect().top;
  const rows = messageRows(viewport);
  let low = 0, high = rows.length;
  while (low < high) {
    const mid = (low + high) >>> 1;
    if (rows[mid].getBoundingClientRect().bottom <= top) low = mid + 1;
    else high = mid;
  }
  let row = rows[low];
  if (!row) return null;
  const next = rows[low + 1];
  if (next && next.getBoundingClientRect().top < top + viewport.clientHeight
    && Math.abs(next.getBoundingClientRect().top - top) < Math.abs(row.getBoundingClientRect().top - top)) row = next;
  return { messageId: row.dataset.messageId!, seq: Number(row.dataset.messageSeq ?? 0),
    offset: row.getBoundingClientRect().top - top };
}

/** Seq of the last row shown on screen (0 when none is). A row counts once
 * its top is clear of the list's bottom padding, not when an edge peeks in. */
export function lastVisibleMessageSeq(viewport: HTMLElement, margin = 32) {
  const bottom = viewport.getBoundingClientRect().bottom - margin;
  const rows = messageRows(viewport);
  let low = 0, high = rows.length;
  while (low < high) {
    const mid = (low + high) >>> 1;
    if (rows[mid].getBoundingClientRect().top < bottom) low = mid + 1;
    else high = mid;
  }
  return low > 0 ? Number(rows[low - 1].dataset.messageSeq ?? 0) : 0;
}
