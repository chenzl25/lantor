import { useEffect } from "react";
import { apiInvoke } from "../apiClient";

// The channel read watermark also covers thread replies, but it only advances
// while the conversation column shows its latest message. An expanded thread,
// or a layout that shows the thread in place of the conversation, hides that
// column, so the visible thread receipts its own replies.
export function useVisibleThreadRead({ threadId, unreadCount, latestActivityAt }: {
  threadId: string | null; unreadCount: number; latestActivityAt: string | null;
}) {
  useEffect(() => {
    if (!threadId || !latestActivityAt || unreadCount <= 0) return;
    const items = [{ itemId: `thread:${threadId}`, dismissedUntil: latestActivityAt }];
    let timer: ReturnType<typeof setTimeout> | undefined;
    function schedule() {
      clearTimeout(timer);
      if (document.visibilityState !== "visible") return;
      timer = setTimeout(() => {
        if (document.visibilityState !== "visible") return;
        // Fence the receipt to the activity this snapshot shows, so a reply
        // arriving while the request is in flight stays unread.
        void apiInvoke("mark_inbox_items_read", { items }).catch(console.error);
      }, 300);
    }
    schedule();
    document.addEventListener("visibilitychange", schedule);
    return () => { clearTimeout(timer); document.removeEventListener("visibilitychange", schedule); };
  }, [threadId, unreadCount, latestActivityAt]);
}
