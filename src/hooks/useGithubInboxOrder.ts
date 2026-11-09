import { useMemo, useRef } from "react";

import { orderGithubInboxItems } from "../github-inbox";
import type { GithubInboxItem } from "../types";

/**
 * Unread threads first. A thread read while the list is shown keeps its place
 * until `session` changes (the list is opened again), so reading a row does
 * not reshuffle the rows around it.
 */
export function useGithubInboxOrder(items: readonly GithubInboxItem[], session: unknown) {
  const seen = useRef<{ session: unknown; unread: Set<string> }>({ session, unread: new Set() });
  if (seen.current.session !== session) seen.current = { session, unread: new Set() };
  const unread = seen.current.unread;
  return useMemo(() => {
    for (const item of items) if (item.unread) unread.add(item.thread_id);
    return orderGithubInboxItems(items, unread);
  }, [items, unread]);
}
