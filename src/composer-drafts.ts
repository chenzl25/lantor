import { useEffect, useRef } from "react";

// Unsent composer text survives a reload. iOS can discard a Home Screen app
// while it is in the background, and signing in again reloads the page.
// Attachments stay in memory only: a File cannot be stored here. See
// docs/web-access.md#unsent-drafts.
export const COMPOSER_DRAFTS_STORAGE_KEY = "lantor.composerDrafts.v1";
export const MAX_STORED_DRAFTS = 50;
export const STORED_DRAFT_MAX_AGE_MS = 30 * 24 * 60 * 60 * 1000;
const SAVE_DELAY_MS = 400;

export type ComposerDraftScope = "channel" | "thread";
type StoredDraft = { text: string; savedAt: number };
type StoredDrafts = Record<string, StoredDraft>;

function readStoredDrafts(storage: Storage, now: number): StoredDrafts {
  let parsed: unknown;
  try {
    parsed = JSON.parse(storage.getItem(COMPOSER_DRAFTS_STORAGE_KEY) ?? "{}");
  } catch {
    return {};
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
  const drafts: StoredDrafts = {};
  for (const [key, value] of Object.entries(parsed as Record<string, unknown>)) {
    if (!value || typeof value !== "object") continue;
    const { text, savedAt } = value as Partial<StoredDraft>;
    if (typeof text !== "string" || !text || typeof savedAt !== "number") continue;
    if (now - savedAt > STORED_DRAFT_MAX_AGE_MS) continue;
    drafts[key] = { text, savedAt };
  }
  return drafts;
}

/** Stored draft text for one composer scope, keyed by channel or thread id. */
export function loadComposerDrafts(scope: ComposerDraftScope, storage: Storage = window.localStorage, now = Date.now()) {
  const prefix = `${scope}:`;
  const texts: Record<string, string> = {};
  try {
    for (const [key, draft] of Object.entries(readStoredDrafts(storage, now))) {
      if (key.startsWith(prefix)) texts[key.slice(prefix.length)] = draft.text;
    }
  } catch {
    // Storage may be unavailable; drafts then last for this page only.
  }
  return texts;
}

/**
 * Applies changed drafts on top of what is stored now, so another tab's drafts
 * for other channels and threads survive. An empty text removes the draft.
 */
export function saveComposerDrafts(
  changes: ReadonlyMap<string, string>,
  storage: Storage = window.localStorage,
  now = Date.now(),
) {
  if (changes.size === 0) return;
  try {
    const drafts = readStoredDrafts(storage, now);
    for (const [key, text] of changes) {
      if (text) drafts[key] = { text, savedAt: now };
      else delete drafts[key];
    }
    const kept = Object.entries(drafts)
      .sort(([, left], [, right]) => right.savedAt - left.savedAt)
      .slice(0, MAX_STORED_DRAFTS);
    if (kept.length === 0) storage.removeItem(COMPOSER_DRAFTS_STORAGE_KEY);
    else storage.setItem(COMPOSER_DRAFTS_STORAGE_KEY, JSON.stringify(Object.fromEntries(kept)));
  } catch {
    // A full or unavailable storage keeps the in-memory draft.
  }
}

export function composerDraftChanges(
  previous: ReadonlyMap<string, string>,
  next: ReadonlyMap<string, string>,
) {
  const changes = new Map<string, string>();
  for (const [key, text] of next) {
    if (previous.get(key) !== text) changes.set(key, text);
  }
  for (const key of previous.keys()) {
    if (!next.has(key)) changes.set(key, "");
  }
  return changes;
}

export function composerDraftKey(scope: ComposerDraftScope, id: string | null | undefined) {
  return id ? `${scope}:${id}` : null;
}

let pendingDrafts = new Map<string, string>();
let saveTimer: number | undefined;
let listeningForHide = false;

/**
 * Queues the latest text of one composer. Composers report every edit, before
 * it reaches App state, so text still being typed is saved too. The queue is
 * written shortly after typing stops, and at once when the page is hidden,
 * since iOS may discard a hidden page without another event.
 */
export function rememberComposerDraft(key: string | null, text: string) {
  if (!key || typeof window === "undefined") return;
  pendingDrafts.set(key, text);
  if (!listeningForHide) {
    listeningForHide = true;
    window.addEventListener("pagehide", flushComposerDrafts);
    document.addEventListener("visibilitychange", () => {
      if (document.visibilityState === "hidden") flushComposerDrafts();
    });
  }
  window.clearTimeout(saveTimer);
  saveTimer = window.setTimeout(flushComposerDrafts, SAVE_DELAY_MS);
}

export function flushComposerDrafts() {
  window.clearTimeout(saveTimer);
  saveTimer = undefined;
  if (pendingDrafts.size === 0) return;
  const changes = pendingDrafts;
  pendingDrafts = new Map();
  saveComposerDrafts(changes);
}

function draftTexts(channelDrafts: Record<string, { text: string }>, threadDrafts: Record<string, { text: string }>) {
  const texts = new Map<string, string>();
  for (const [id, draft] of Object.entries(channelDrafts)) {
    if (draft.text) texts.set(`channel:${id}`, draft.text);
  }
  for (const [id, draft] of Object.entries(threadDrafts)) {
    if (draft.text) texts.set(`thread:${id}`, draft.text);
  }
  return texts;
}

/**
 * Queues draft changes made through App state: sending clears a draft, a failed
 * send restores it, and a message reference can be inserted from a menu. These
 * can happen while that composer is not mounted.
 */
export function useRememberedComposerDrafts(
  channelDrafts: Record<string, { text: string }>,
  threadDrafts: Record<string, { text: string }>,
) {
  const previous = useRef<Map<string, string> | null>(null);
  previous.current ??= draftTexts(channelDrafts, threadDrafts);
  useEffect(() => {
    const next = draftTexts(channelDrafts, threadDrafts);
    for (const [key, text] of composerDraftChanges(previous.current ?? new Map(), next)) {
      rememberComposerDraft(key, text);
    }
    previous.current = next;
  }, [channelDrafts, threadDrafts]);
}
