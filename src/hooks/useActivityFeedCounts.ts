import { useEffect, useRef, useState } from "react";
import { apiInvoke } from "../apiClient";
import type { ActivityFeedCounts } from "../types";

export function useActivityFeedCounts(revision: object, mentionHandles: string[]) {
  const [counts, setCounts] = useState<ActivityFeedCounts | null>(null);
  const invalidate = useRef(() => {});
  useEffect(() => {
    let disposed = false;
    let running = false;
    let dirty = true;
    let lastStarted = 0;
    let timer: number | undefined;
    function schedule() {
      if (disposed || running || timer !== undefined || document.hidden || !dirty) return;
      // Throttle, rather than debounce: continuous events cannot starve the badge.
      timer = window.setTimeout(refresh, Math.max(100, 5000 - (Date.now() - lastStarted)));
    }
    async function refresh() {
      timer = undefined;
      if (disposed || document.hidden) return;
      running = true;
      dirty = false;
      lastStarted = Date.now();
      try {
        const result = await apiInvoke("load_activity_counts", { mentionHandles });
        if (!disposed) setCounts(result);
      } catch {
        // Keep the last successful badge; retry on the next state change.
      } finally {
        running = false;
        schedule();
      }
    }
    invalidate.current = () => { dirty = true; schedule(); };
    document.addEventListener("visibilitychange", invalidate.current);
    schedule();
    return () => {
      disposed = true;
      window.clearTimeout(timer);
      document.removeEventListener("visibilitychange", invalidate.current);
      invalidate.current = () => {};
    };
  }, [mentionHandles]);
  useEffect(() => { invalidate.current(); }, [revision]);
  return counts;
}
