import { useMemo, useRef } from "react";

import type { GithubInboxItem } from "../types";

/**
 * The unread threads, while `enabled`. A thread read while the view is shown
 * stays in it until `session` changes or the view is turned off, so opening a
 * row does not pull it out from under the pointer.
 */
export function useGithubInboxUnreadOnly(items: readonly GithubInboxItem[], enabled: boolean, session: unknown) {
  const key = enabled ? session : null;
  const shown = useRef<{ key: unknown; ids: Set<string> }>({ key, ids: new Set() });
  if (shown.current.key !== key) shown.current = { key, ids: new Set() };
  const ids = shown.current.ids;
  return useMemo(() => {
    if (!enabled) return items;
    for (const item of items) if (item.unread) ids.add(item.thread_id);
    return items.filter((item) => ids.has(item.thread_id));
  }, [items, enabled, ids]);
}
