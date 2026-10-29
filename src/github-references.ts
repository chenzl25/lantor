// Resolves `#123`, `rw#123`, `rw #123` and `owner/repo#123` in message text to GitHub
// links. PRs/issues in the synced GitHub queues get their state from one local
// read (`load_github_references`); other referenced PRs/issues (usually merged
// or closed ones) are looked up in batches through `lookup_github_references`,
// which queries GitHub and caches the answers on the backend.
import { apiInvoke } from "./apiClient";
import type { GithubReferenceItem, GithubReferenceLookup, GithubReferences } from "./types";

export type GithubReferenceIndex = {
  /** Lower-cased `owner/repo` by channel id. */
  byChannel: ReadonlyMap<string, string>;
  /** Lower-cased alias or repo name → lower-cased `owner/repo`; `null` when ambiguous. */
  byName: ReadonlyMap<string, string | null>;
  /**
   * Explicit aliases and repository names, but not derived aliases: only these
   * qualify a spaced `rw #123`, so prose like `in #123` never matches `in` for
   * a repository called `iceberg-nessie`.
   */
  spacedNames: ReadonlySet<string>;
  /** Lower-cased `owner/repo` → canonical `owner/repo` and its short display prefix. */
  repositories: ReadonlyMap<string, { nameWithOwner: string; shortName: string }>;
  /** `owner/repo#number` (lower-cased repo) → cached item. */
  items: ReadonlyMap<string, GithubReferenceItem>;
  /** Explicit short name set on each channel binding. */
  aliasByChannel: ReadonlyMap<string, string>;
};

export type GithubIssueLink = {
  nameWithOwner: string;
  number: number;
};

const EMPTY_INDEX: GithubReferenceIndex = {
  byChannel: new Map(),
  byName: new Map(),
  spacedNames: new Set(),
  repositories: new Map(),
  items: new Map(),
  aliasByChannel: new Map(),
};

const REPOSITORY_PART = /^[A-Za-z0-9][A-Za-z0-9_.-]*$/;
const GITHUB_ISSUE_URL = /^https:\/\/github\.com\/([A-Za-z0-9][A-Za-z0-9_.-]*)\/([A-Za-z0-9_.-]+)\/(?:pull|issues)\/([0-9]+)(?:[/?#].*)?$/;

let index = EMPTY_INDEX;
/** Lookups wait for the first queue read so queued items are not looked up. */
let referencesLoaded = false;
let loading: Promise<void> | null = null;
const listeners = new Set<() => void>();

/** `iceberg-rust` → `ir`; single-word names get no derived alias. */
export function derivedReferenceAlias(repositoryName: string) {
  const parts = repositoryName.toLowerCase().split(/[-_.]+/).filter(Boolean);
  if (parts.length < 2) return "";
  return parts.map((part) => part[0]).join("");
}

export function buildGithubReferenceIndex(data: GithubReferences): GithubReferenceIndex {
  const byChannel = new Map<string, string>();
  const aliasByChannel = new Map<string, string>();
  const repositories = new Map<string, { nameWithOwner: string; shortName: string }>();
  const derived = new Map<string, Set<string>>();
  const named = new Map<string, Set<string>>();
  const explicit = new Map<string, Set<string>>();
  const add = (target: Map<string, Set<string>>, name: string, repository: string) => {
    if (!name) return;
    const key = name.toLowerCase();
    target.set(key, (target.get(key) ?? new Set()).add(repository));
  };

  for (const binding of data.repositories ?? []) {
    const key = binding.name_with_owner.toLowerCase();
    byChannel.set(binding.channel_id, key);
    const repositoryName = binding.name_with_owner.split("/")[1] ?? binding.name_with_owner;
    const alias = binding.reference_alias.trim().toLowerCase();
    aliasByChannel.set(binding.channel_id, alias);
    const previous = repositories.get(key);
    repositories.set(key, {
      nameWithOwner: binding.name_with_owner,
      shortName: alias || previous?.shortName || repositoryName,
    });
    add(explicit, alias, key);
    add(named, repositoryName, key);
    add(derived, derivedReferenceAlias(repositoryName), key);
  }

  const byName = new Map<string, string | null>();
  // Explicit aliases win over repository names, which win over derived
  // aliases. A name that maps to two repositories at the same level stays
  // unresolved instead of guessing. The backend lists the same short names to
  // agents (`agent_github_reference_lines`); keep the two in step.
  for (const source of [derived, named, explicit]) {
    for (const [name, targets] of source) {
      byName.set(name, targets.size === 1 ? [...targets][0] : null);
    }
  }
  byName.delete("task");
  const spacedNames = new Set([...named.keys(), ...explicit.keys()]);

  const items = new Map<string, GithubReferenceItem>();
  for (const item of data.items ?? []) {
    items.set(`${item.name_with_owner.toLowerCase()}#${item.number}`, item);
  }
  return { byChannel, byName, spacedNames, repositories, items, aliasByChannel };
}

export function getGithubReferenceIndex() {
  return index;
}

export function subscribeGithubReferences(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function setGithubReferences(data: GithubReferences) {
  index = buildGithubReferenceIndex(data);
  listeners.forEach((listener) => listener());
  markGithubReferencesLoaded();
}

function markGithubReferencesLoaded() {
  referencesLoaded = true;
  if (queuedLookups.size > 0) lookupTimer ??= setTimeout(flushGithubReferenceLookups, LOOKUP_DELAY_MS);
}

export function loadGithubReferences() {
  loading ??= apiInvoke("load_github_references")
    .then(setGithubReferences)
    .catch((error) => {
      console.warn("Failed to load GitHub references", error);
      markGithubReferencesLoaded();
    })
    .finally(() => {
      loading = null;
    });
  return loading;
}

/**
 * Resolves a reference prefix (`null` for a bare `#123`) to `owner/repo`. A bare
 * `#123` means the repository bound to the message's channel. With `spaced`
 * (`rw #123`) the prefix must be an explicit alias, a repository name or a bound
 * `owner/repo`, since prose like `src/app #12` or `in #12` is not a reference.
 */
export function resolveGithubRepository(
  references: GithubReferenceIndex,
  prefix: string | null,
  channelId: string | undefined,
  spaced = false,
) {
  if (prefix === null) return channelId ? references.byChannel.get(channelId) ?? null : null;
  if (prefix.includes("/")) {
    const [owner, repository, extra] = prefix.split("/");
    if (extra !== undefined || !REPOSITORY_PART.test(owner) || !REPOSITORY_PART.test(repository ?? "")) return null;
    const key = prefix.toLowerCase();
    return spaced && !references.repositories.has(key) ? null : key;
  }
  const name = prefix.toLowerCase();
  if (spaced && !references.spacedNames.has(name)) return null;
  return references.byName.get(name) ?? null;
}

export function githubReferenceUrl(references: GithubReferenceIndex, repository: string, number: number) {
  const item = references.items.get(`${repository}#${number}`);
  if (item) return item.url;
  const nameWithOwner = references.repositories.get(repository)?.nameWithOwner ?? repository;
  // GitHub redirects /issues/N to /pull/N when N is a pull request.
  return `https://github.com/${nameWithOwner}/issues/${number}`;
}

export function parseGithubIssueUrl(href: string | undefined): GithubIssueLink | null {
  const match = href?.match(GITHUB_ISSUE_URL);
  if (!match) return null;
  return { nameWithOwner: `${match[1]}/${match[2]}`, number: Number(match[3]) };
}

export function githubReferenceKey(link: GithubIssueLink) {
  return `${link.nameWithOwner.toLowerCase()}#${link.number}`;
}

export function githubReferenceItem(references: GithubReferenceIndex, link: GithubIssueLink) {
  return references.items.get(githubReferenceKey(link)) ?? null;
}

// On-demand state of references that are not in the synced queues.
type LookupEntry = { item: GithubReferenceItem | null; checkedAt: number; pending: boolean };

/** Batch window: links rendered together are looked up in one request. */
const LOOKUP_DELAY_MS = 50;
const LOOKUP_BATCH_SIZE = 100;
/** Re-ask the backend (which has its own cache) at most this often per reference. */
const LOOKUP_RECHECK_MS = 10 * 60_000;

const lookups = new Map<string, LookupEntry>();
const lookupListeners = new Set<() => void>();
let queuedLookups = new Map<string, GithubIssueLink>();
let lookupTimer: ReturnType<typeof setTimeout> | null = null;
let lookupErrorShown = false;

export function subscribeGithubReferenceLookups(listener: () => void) {
  lookupListeners.add(listener);
  return () => {
    lookupListeners.delete(listener);
  };
}

/** The looked-up item for a reference key; a stable object until it changes. */
export function lookedUpGithubReference(key: string) {
  return lookups.get(key)?.item ?? null;
}

/** Queues a state lookup for a reference that is not in the synced queues. */
export function requestGithubReferenceLookup(link: GithubIssueLink, now = Date.now()) {
  const key = githubReferenceKey(link);
  const entry = lookups.get(key);
  if (entry && (entry.pending || entry.item?.state === "merged" || now - entry.checkedAt < LOOKUP_RECHECK_MS)) return;
  if (queuedLookups.has(key)) return;
  queuedLookups.set(key, link);
  lookupTimer ??= setTimeout(flushGithubReferenceLookups, LOOKUP_DELAY_MS);
}

function flushGithubReferenceLookups() {
  lookupTimer = null;
  if (!referencesLoaded) return;
  const queued = [...queuedLookups].filter(([key]) => !index.items.has(key));
  queuedLookups = new Map();
  for (let start = 0; start < queued.length; start += LOOKUP_BATCH_SIZE) {
    const batch = queued.slice(start, start + LOOKUP_BATCH_SIZE);
    for (const [key] of batch) {
      const previous = lookups.get(key);
      lookups.set(key, { item: previous?.item ?? null, checkedAt: previous?.checkedAt ?? 0, pending: true });
    }
    void apiInvoke("lookup_github_references", {
      references: batch.map(([, link]) => ({ nameWithOwner: link.nameWithOwner, number: link.number })),
    })
      .then((result) => applyGithubReferenceLookup(batch.map(([key]) => key), result))
      .catch((error) => {
        applyGithubReferenceLookup(batch.map(([key]) => key), { items: [], error: String(error) });
      });
  }
}

function applyGithubReferenceLookup(keys: string[], result: GithubReferenceLookup) {
  if (result.error && !lookupErrorShown) {
    lookupErrorShown = true;
    console.warn("GitHub reference lookup incomplete:", result.error);
  }
  const found = new Map(result.items.map((item) => [`${item.name_with_owner.toLowerCase()}#${item.number}`, item]));
  const checkedAt = Date.now();
  for (const key of keys) {
    const previous = lookups.get(key)?.item ?? null;
    const item = found.get(key) ?? null;
    // Keep the previous object when nothing changed so links do not re-render.
    const unchanged = previous && item && previous.state === item.state && previous.is_draft === item.is_draft
      && previous.checks_status === item.checks_status && previous.title === item.title;
    lookups.set(key, { item: unchanged ? previous : item ?? (result.error ? previous : null), checkedAt, pending: false });
  }
  lookupListeners.forEach((listener) => listener());
}

/** `rw#27495` for bound repositories, `owner/repo#27495` otherwise. */
export function githubReferenceLabel(references: GithubReferenceIndex, link: GithubIssueLink) {
  const shortName = references.repositories.get(link.nameWithOwner.toLowerCase())?.shortName;
  return `${shortName ?? link.nameWithOwner}#${link.number}`;
}

export type GithubReferenceStatus = "open" | "draft" | "merged" | "closed" | "completed" | "not_planned";

export function githubReferenceStatus(item: GithubReferenceItem): GithubReferenceStatus {
  if (item.state === "merged") return "merged";
  if (item.state === "closed") {
    if (item.kind !== "issue") return "closed";
    // GitHub shows issues closed as not planned (or duplicate) in grey, others as completed.
    return item.state_reason === "not_planned" || item.state_reason === "duplicate" ? "not_planned" : "completed";
  }
  return item.kind === "pull" && item.is_draft ? "draft" : "open";
}
