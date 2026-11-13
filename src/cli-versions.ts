import { createContext, useContext, useEffect, useSyncExternalStore } from "react";
import type { CliVersionStatus } from "./types";

export type CliVersionEntry = {
  status: CliVersionStatus | null;
  loadFailed: boolean;
};

export type CliVersionApi = {
  load: (runtime: string) => Promise<CliVersionStatus>;
};

/** Opens the agent's DM and sends the owner's message there. */
export type AskAgent = (agentId: string, body: string) => Promise<void>;

type Listener = () => void;

const EMPTY_ENTRY: CliVersionEntry = { status: null, loadFailed: false };

/**
 * The installed and latest CLI version per runtime, shared by the profile and
 * the avatar hover card. One install serves every agent on a runtime, so the
 * status is keyed by runtime. Each read runs `--version` in a login shell on
 * the Mac, so hover cards reuse a recent status instead of reading again.
 * Updating is an agent's job: askAgent sends it the request.
 */
export class CliVersionStore {
  private entries = new Map<string, CliVersionEntry>();
  private loadedAt = new Map<string, number>();
  private loading = new Set<string>();
  private listeners = new Set<Listener>();
  askAgent: AskAgent | null = null;

  constructor(private api: CliVersionApi, private now: () => number = Date.now) {}

  get = (runtime: string): CliVersionEntry => this.entries.get(runtime) ?? EMPTY_ENTRY;

  subscribe = (listener: Listener) => {
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); };
  };

  /**
   * Reads the status unless one read within maxAgeMs is cached or a read is
   * already on its way.
   */
  refresh(runtime: string, maxAgeMs: number) {
    const loadedAt = this.loadedAt.get(runtime);
    const fresh = loadedAt !== undefined && this.now() - loadedAt <= maxAgeMs;
    if (fresh || this.loading.has(runtime)) return;
    this.loading.add(runtime);
    this.api.load(runtime).then(
      (status) => {
        this.loadedAt.set(runtime, this.now());
        this.set(runtime, { status, loadFailed: false });
      },
      () => {
        this.loadedAt.delete(runtime);
        this.set(runtime, { loadFailed: true });
      },
    ).finally(() => this.loading.delete(runtime));
  }

  /** Makes the next view read the status again instead of reusing it. */
  invalidate(runtime: string) {
    this.loadedAt.delete(runtime);
  }

  private set(runtime: string, patch: Partial<CliVersionEntry>) {
    this.entries.set(runtime, { ...this.get(runtime), ...patch });
    for (const listener of this.listeners) listener();
  }
}

export const CliVersionStoreContext = createContext<CliVersionStore | null>(null);

const noopSubscribe = () => () => {};
const noEntry = () => EMPTY_ENTRY;

/**
 * The CLI status for a runtime, read when the caller mounts unless a read
 * within maxAgeMs is cached. Returns null outside a CliVersionStoreContext.
 */
export function useCliVersion(runtime: string, maxAgeMs: number) {
  const store = useContext(CliVersionStoreContext);
  const entry = useSyncExternalStore(
    store?.subscribe ?? noopSubscribe,
    store ? () => store.get(runtime) : noEntry,
  );

  useEffect(() => {
    store?.refresh(runtime, maxAgeMs);
  }, [store, runtime, maxAgeMs]);

  return store ? { entry, store } : null;
}
