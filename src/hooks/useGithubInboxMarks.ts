import { useMemo, useState } from "react";

import { githubInboxItemsWithPending } from "../github-inbox";
import type { GithubInboxItem } from "../types";

function errorText(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

/**
 * Read and Done for GitHub notification threads. A mark shows at once and
 * holds until the write returns with the stored inbox (or an error).
 */
export function useGithubInboxMarks(
  items: readonly GithubInboxItem[],
  onMarkRead: (threadId: string) => Promise<void>,
  onMarkDone: (threadId: string) => Promise<void>,
) {
  const [pendingRead, setPendingRead] = useState<ReadonlySet<string>>(() => new Set());
  const [pendingDone, setPendingDone] = useState<ReadonlySet<string>>(() => new Set());
  const [actionError, setActionError] = useState<string | null>(null);

  const visibleItems = useMemo(
    () => githubInboxItemsWithPending(items, { read: pendingRead, done: pendingDone }),
    [items, pendingRead, pendingDone],
  );

  async function mark(item: GithubInboxItem, kind: "read" | "done") {
    const setPending = kind === "read" ? setPendingRead : setPendingDone;
    setPending((current) => new Set(current).add(item.thread_id));
    setActionError(null);
    try {
      await (kind === "read" ? onMarkRead(item.thread_id) : onMarkDone(item.thread_id));
    } catch (error) {
      setActionError(errorText(error));
    } finally {
      // The response carries the stored inbox, which now has the mark (or not).
      setPending((current) => {
        const next = new Set(current);
        next.delete(item.thread_id);
        return next;
      });
    }
  }

  return { items: visibleItems, mark, actionError, setActionError };
}
