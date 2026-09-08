import { useEffect } from "react";
import { apiInvoke } from "../apiClient";
import type { ChannelReadLocation } from "./useChannelMessageScroll";

export function useVisibleChannelRead({ channelId, latestRootId, throughSeq, unreadCount, ready, active, location }: {
  channelId: string | null; latestRootId: string | null; throughSeq: number;
  unreadCount: number; ready: boolean; active: boolean; location: ChannelReadLocation | null;
}) {
  const atLatest = location?.channelId === channelId && location?.latestRootId === latestRootId && location.atLatest;
  useEffect(() => {
    if (!channelId || !ready || !active || !atLatest || unreadCount <= 0 || throughSeq <= 0) return;
    let timer: ReturnType<typeof setTimeout> | undefined;
    function schedule() {
      clearTimeout(timer);
      if (document.visibilityState !== "visible") return;
      timer = setTimeout(() => {
        if (document.visibilityState !== "visible") return;
        // Fence the receipt to the committed snapshot. Messages arriving while
        // this request is in flight must not be swallowed by a server-side max.
        void apiInvoke("mark_channel_read", { channelId: channelId!, throughSeq }).catch(console.error);
      }, 300);
    }
    schedule();
    document.addEventListener("visibilitychange", schedule);
    return () => { clearTimeout(timer); document.removeEventListener("visibilitychange", schedule); };
  }, [channelId, latestRootId, throughSeq, unreadCount, ready, active, atLatest]);
}
