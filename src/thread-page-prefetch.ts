// A thread opens when the finger lifts off its reply summary. Asking for its
// newest replies when the finger lands saves the length of the tap; opening
// the thread then takes that request instead of starting its own.

/** A prefetched page older than this is asked for again. */
export const THREAD_PAGE_PREFETCH_MAX_AGE_MS = 5_000;

type Prefetch<Page, Baseline> = {
  key: string;
  startedAt: number;
  /** What the client held when the request began, for applying the page. */
  baseline: Baseline;
  page: Promise<Page>;
};

export class ThreadPagePrefetches<Page, Baseline> {
  private readonly entries = new Map<string, Prefetch<Page, Baseline>>();

  constructor(
    private readonly now: () => number = () => performance.now(),
    private readonly maxAgeMs = THREAD_PAGE_PREFETCH_MAX_AGE_MS,
  ) {}

  /** Starts a request unless the same one is already fresh. */
  start(threadId: string, args: unknown, baseline: () => Baseline, fetchPage: () => Promise<Page>) {
    const key = JSON.stringify(args);
    const existing = this.entries.get(threadId);
    if (existing && existing.key === key && this.fresh(existing)) return;
    const page = fetchPage();
    // Whoever opens the thread sees a failure; an unused prefetch fails quietly.
    page.catch(() => {});
    this.entries.set(threadId, { key, startedAt: this.now(), baseline: baseline(), page });
  }

  /** Takes a fresh request for exactly these arguments, once. */
  take(threadId: string, args: unknown): { page: Promise<Page>; baseline: Baseline } | undefined {
    const entry = this.entries.get(threadId);
    this.entries.delete(threadId);
    if (!entry || entry.key !== JSON.stringify(args) || !this.fresh(entry)) return undefined;
    return { page: entry.page, baseline: entry.baseline };
  }

  private fresh(entry: Prefetch<Page, Baseline>) {
    return this.now() - entry.startedAt <= this.maxAgeMs;
  }
}
