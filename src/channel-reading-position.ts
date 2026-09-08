export type MessageScrollAnchor = { messageId: string; seq: number; offset: number };
export type ChannelReadingPosition = {
  anchor: MessageScrollAnchor;
  atBottom: boolean;
  latestRootId: string;
};

export const CHANNEL_READING_POSITION_KEY = "lantor.channelReadingPositions.v1";
const MAX_CHANNELS = 128;
const MAX_STORAGE_CHARS = 64 * 1024;

export function parseReadingPositions(raw: string | null): Map<string, ChannelReadingPosition> {
  const result = new Map<string, ChannelReadingPosition>();
  if (!raw || raw.length > MAX_STORAGE_CHARS) return result;
  try {
    const entries: unknown = JSON.parse(raw);
    if (!Array.isArray(entries)) return result;
    for (const entry of entries.slice(-MAX_CHANNELS)) {
      if (!Array.isArray(entry) || entry.length !== 2) continue;
      const [channelId, value] = entry;
      const anchor = value?.anchor;
      if (typeof channelId !== "string" || channelId.length > 128
        || typeof value?.atBottom !== "boolean" || typeof value?.latestRootId !== "string"
        || value.latestRootId.length > 128 || typeof anchor?.messageId !== "string"
        || anchor.messageId.length > 128 || !Number.isSafeInteger(anchor.seq) || anchor.seq <= 0
        || !Number.isFinite(anchor.offset) || Math.abs(anchor.offset) > 1e7) continue;
      result.set(channelId, { anchor: { messageId: anchor.messageId, seq: anchor.seq, offset: anchor.offset },
        atBottom: value.atBottom, latestRootId: value.latestRootId });
    }
  } catch { /* Corrupt or unavailable local state must not block navigation. */ }
  return result;
}

let positions: Map<string, ChannelReadingPosition> | undefined;
let writeTimer: ReturnType<typeof setTimeout> | undefined;

export function readChannelPosition(channelId: string) {
  if (!positions) {
    try { positions = parseReadingPositions(localStorage.getItem(CHANNEL_READING_POSITION_KEY)); }
    catch { positions = new Map(); }
  }
  return positions.get(channelId) ?? null;
}

export function flushChannelPositions() {
  if (writeTimer) clearTimeout(writeTimer);
  writeTimer = undefined;
  if (!positions) return;
  try { localStorage.setItem(CHANNEL_READING_POSITION_KEY, JSON.stringify([...positions])); }
  catch { /* Keep the in-memory position when storage is blocked/full. */ }
}

export function rememberChannelPosition(channelId: string, position: ChannelReadingPosition) {
  // Optimistic sends have seq=0 and can disappear on failure. Never turn one
  // into a durable anchor that would make restoration page back to sequence 0.
  if (position.anchor.seq <= 0) return;
  readChannelPosition(channelId);
  positions!.delete(channelId);
  positions!.set(channelId, position);
  while (positions!.size > MAX_CHANNELS) positions!.delete(positions!.keys().next().value!);
  if (!writeTimer) writeTimer = setTimeout(flushChannelPositions, 150);
}

// Inspect only outer row boxes, using binary search even for long histories.
// Do not force layout of every content-visibility-skipped Markdown descendant.
export function captureMessageAnchor(viewport: HTMLElement): MessageScrollAnchor | null {
  const top = viewport.getBoundingClientRect().top;
  const rows = viewport.querySelectorAll<HTMLElement>("article[data-message-id]");
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
