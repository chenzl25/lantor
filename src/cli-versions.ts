import { createContext, useContext, useEffect, useSyncExternalStore } from "react";
import type { CliVersionStatus } from "./types";

/** How often a shown status is re-read while its update runs. */
export const CLI_UPDATE_POLL_MS = 2000;

export type CliVersionEntry = {
  status: CliVersionStatus | null;
  loadFailed: boolean;
  /** update_cli was sent and has not answered yet. */
  starting: boolean;
};

export type CliVersionApi = {
  load: (runtime: string) => Promise<CliVersionStatus>;
  update: (runtime: string) => Promise<CliVersionStatus>;
};

/** Opens the agent's DM and sends the owner's request to fix a failed update. */
export type AskAgentToFixCli = (agentId: string, body: string) => Promise<void>;

type Listener = () => void;

const EMPTY_ENTRY: CliVersionEntry = { status: null, loadFailed: false, starting: false };

/**
 * The installed and latest CLI version per runtime, shared by the profile and
 * the avatar hover card. One install serves every agent on a runtime, so the
 * status is keyed by runtime. Each read runs `--version` in a login shell on
 * the Mac, so hover cards reuse a recent status instead of reading again.
 */
export class CliVersionStore {
  private entries = new Map<string, CliVersionEntry>();
  private loadedAt = new Map<string, number>();
  private loading = new Map<string, number>();
  private requests = new Map<string, number>();
  private listeners = new Set<Listener>();
  askAgentToFix: AskAgentToFixCli | null = null;

  constructor(private api: CliVersionApi, private now: () => number = Date.now) {}

  get = (runtime: string): CliVersionEntry => this.entries.get(runtime) ?? EMPTY_ENTRY;

  subscribe = (listener: Listener) => {
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); };
  };

  /**
   * Reads the status unless one read within maxAgeMs is cached or a read is
   * already on its way. A running update is always read again.
   */
  refresh(runtime: string, maxAgeMs: number) {
    const entry = this.get(runtime);
    const loadedAt = this.loadedAt.get(runtime);
    const fresh = loadedAt !== undefined && this.now() - loadedAt <= maxAgeMs && !entry.status?.updating;
    if (fresh || this.loading.has(runtime)) return;
    const request = this.nextRequest(runtime);
    this.loading.set(runtime, request);
    this.api.load(runtime).then(
      (status) => this.settle(runtime, request, { status, loadFailed: false }),
      () => this.settle(runtime, request, { loadFailed: true }),
    ).finally(() => {
      if (this.loading.get(runtime) === request) this.loading.delete(runtime);
    });
  }

  async update(runtime: string) {
    const request = this.nextRequest(runtime);
    this.loading.delete(runtime);
    this.set(runtime, { starting: true });
    try {
      const status = await this.api.update(runtime);
      this.settle(runtime, request, { status, loadFailed: false, starting: false });
    } catch {
      this.settle(runtime, request, { loadFailed: true, starting: false });
    }
  }

  private nextRequest(runtime: string) {
    const request = (this.requests.get(runtime) ?? 0) + 1;
    this.requests.set(runtime, request);
    return request;
  }

  /** Applies an answer unless a later request for this runtime was sent. */
  private settle(runtime: string, request: number, patch: Partial<CliVersionEntry>) {
    if (this.requests.get(runtime) !== request) return;
    if (patch.status) this.loadedAt.set(runtime, this.now());
    else this.loadedAt.delete(runtime);
    this.set(runtime, patch);
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
 * within maxAgeMs is cached, and re-read while an update runs. Returns null
 * outside a CliVersionStoreContext.
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

  useEffect(() => {
    if (!store || !entry.status?.updating) return;
    const timer = window.setTimeout(() => store.refresh(runtime, 0), CLI_UPDATE_POLL_MS);
    return () => window.clearTimeout(timer);
  }, [store, runtime, entry]);

  return store ? { entry, store } : null;
}
