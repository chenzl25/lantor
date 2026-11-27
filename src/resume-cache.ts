import type { Bootstrap, Message } from "./types";
import type { ChannelReadingPosition } from "./channel-reading-position";

// iOS discards a Home Screen app's page while it sits in the background, and
// returning to it reloads the page. Without a local copy the phone then shows
// the startup screen until the workspace comes back from the Mac. The phone
// keeps the last workspace it showed in IndexedDB: a reload draws that at once
// and the normal bootstrap replaces it. The server stays the source of truth;
// this is a cache the browser may drop at any time.

/** A copy older than this is not shown. */
export const RESUME_CACHE_MAX_AGE_MS = 7 * 24 * 60 * 60 * 1000;
/** A reload within this long of the last save also reopens the same view. */
export const RESUME_VIEW_MAX_AGE_MS = 60 * 60 * 1000;
/** How many recently opened channels, and threads, keep their messages. */
export const RESUME_RECENT_LIMIT = 10;
const CHANNEL_ROOTS_KEPT = 30;
const THREAD_REPLIES_KEPT = 50;

const DATABASE = "lantor-resume-cache";
const STORE = "snapshots";
const KEY = "phone";

export type ResumeRecent = { channelIds: string[]; threadIds: string[] };

export type ResumeView = {
  activeChannelId: string | null;
  activeThreadId: string | null;
  threadOpen: boolean;
  homeOpen: boolean;
};

export type ResumeSnapshot = {
  /** The app build that wrote it; another build's copy is dropped. */
  version: string;
  savedAt: number;
  data: Bootstrap;
  recent: ResumeRecent;
  view: ResumeView;
  positions: [string, ChannelReadingPosition][];
};

/** Most recent first, without repeats, at most `RESUME_RECENT_LIMIT`. */
export function rememberRecent(list: readonly string[], id: string | null) {
  if (!id) return [...list];
  return [id, ...list.filter((item) => item !== id)].slice(0, RESUME_RECENT_LIMIT);
}

function newestBySeq(messages: Message[], limit: number) {
  return [...messages].sort((left, right) => right.seq - left.seq).slice(0, limit);
}

/**
 * The workspace as worth keeping: every collection the first screen needs, but
 * messages only for the recent channels (their newest top-level messages) and
 * threads (the root and its newest replies). Unsent messages and their local
 * file previews never leave the page.
 */
export function workspaceForResume(data: Bootstrap, recent: ResumeRecent): Bootstrap {
  const sent = data.messages.filter((message) => message.seq > 0 && message.delivery_state !== "sending");
  const channelIds = new Set(recent.channelIds);
  const threadIds = new Set(recent.threadIds);
  const kept = new Map<string, Message>();
  for (const channelId of channelIds) {
    const roots = sent.filter((message) => message.channel_id === channelId && !message.thread_root_id);
    for (const message of newestBySeq(roots, CHANNEL_ROOTS_KEPT)) kept.set(message.id, message);
  }
  for (const threadId of threadIds) {
    const root = sent.find((message) => message.id === threadId);
    if (root) kept.set(root.id, root);
    const replies = sent.filter((message) => message.thread_root_id === threadId);
    for (const message of newestBySeq(replies, THREAD_REPLIES_KEPT)) kept.set(message.id, message);
  }
  const messages = sent
    .filter((message) => kept.has(message.id))
    .map((message) => (message.attachments.some((attachment) => attachment.local_url)
      ? { ...message, attachments: message.attachments.map(({ local_url: _local, ...attachment }) => attachment) }
      : message));
  const { __perf: _perf, ...rest } = data;
  return { ...rest, messages };
}

/** Whether a stored copy may be shown, and whether its view may be reopened. */
export function resumeSnapshotUse(snapshot: Pick<ResumeSnapshot, "version" | "savedAt">, version: string, now: number) {
  const age = now - snapshot.savedAt;
  const usable = snapshot.version === version && age >= 0 && age <= RESUME_CACHE_MAX_AGE_MS;
  return { usable, reopenView: usable && age <= RESUME_VIEW_MAX_AGE_MS };
}

/** Only the app added to the Home Screen keeps a copy, not a browser tab. */
export function isHomeScreenApp() {
  if (typeof window === "undefined") return false;
  return (navigator as Navigator & { standalone?: boolean }).standalone === true
    || window.matchMedia?.("(display-mode: standalone)").matches === true;
}

/** The build version the app shell stamps into production pages. */
export function appShellVersion() {
  if (typeof document === "undefined") return null;
  return document.querySelector<HTMLMetaElement>('meta[name="lantor-shell-version"]')?.content || null;
}

function openDatabase(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DATABASE, 1);
    request.onupgradeneeded = () => request.result.createObjectStore(STORE);
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

async function withStore<T>(mode: IDBTransactionMode, use: (store: IDBObjectStore) => IDBRequest<T>): Promise<T> {
  const database = await openDatabase();
  try {
    return await new Promise<T>((resolve, reject) => {
      const transaction = database.transaction(STORE, mode);
      const request = use(transaction.objectStore(STORE));
      transaction.oncomplete = () => resolve(request.result);
      transaction.onerror = () => reject(transaction.error);
      transaction.onabort = () => reject(transaction.error);
    });
  } finally {
    database.close();
  }
}

export function saveResumeSnapshot(snapshot: ResumeSnapshot) {
  return withStore("readwrite", (store) => store.put(snapshot, KEY)).then(() => undefined);
}

export function readResumeSnapshot(): Promise<ResumeSnapshot | undefined> {
  return withStore("readonly", (store) => store.get(KEY) as IDBRequest<ResumeSnapshot | undefined>);
}

export function clearResumeSnapshot() {
  return withStore("readwrite", (store) => store.delete(KEY)).then(() => undefined);
}

/** The stored copy if this build may show it; anything else is deleted. */
export async function loadResumeSnapshot(version: string, now = Date.now()) {
  const snapshot = await readResumeSnapshot();
  if (!snapshot) return null;
  const use = resumeSnapshotUse(snapshot, version, now);
  if (!use.usable) {
    await clearResumeSnapshot();
    return null;
  }
  return { snapshot, reopenView: use.reopenView };
}
