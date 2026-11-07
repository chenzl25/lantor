import type { ApiArgs } from "./api-contract";
import type { Message, ThreadMessagePage } from "./types";

// A thread opens with its newest replies and loads earlier ones on request,
// so a thread with thousands of replies does not download and render them all.
// See docs/bootstrap-payload.md.
export const THREAD_PAGE_REPLIES = 50;

/**
 * The seq of the oldest reply in the run loaded up to the newest reply, or
 * null once no earlier reply exists. Undefined means the thread has not been
 * loaded yet.
 */
export type ThreadReplyStart = number | null;

export function mergeThreadReplyStart(
  current: ThreadReplyStart | undefined,
  page: Pick<ThreadMessagePage, "has_more" | "next_before_seq">,
): ThreadReplyStart {
  if (!page.has_more || page.next_before_seq === null) return null;
  if (current === undefined) return page.next_before_seq;
  if (current === null) return null;
  return Math.min(current, page.next_before_seq);
}

/**
 * Opening a thread loads the newest page. Reopening it, or reloading it after
 * a snapshot change, reloads everything from the run's start, so replies
 * missed meanwhile cannot leave a gap.
 */
export function threadPageRequest(threadRootId: string, start: ThreadReplyStart | undefined): ApiArgs<"load_thread_page"> {
  if (start === undefined) return { threadRootId, limit: THREAD_PAGE_REPLIES };
  return { threadRootId, sinceSeq: start ?? 0, limit: THREAD_PAGE_REPLIES };
}

/**
 * Replies to show: the loaded run only. Earlier replies already in memory
 * from search, Activity or a reference are hidden until their page loads, so
 * the thread never shows a gap. A reply being jumped to stays visible while
 * the replies up to it load. Unsent replies have no seq yet and always show.
 */
export function repliesInLoadedRun(replies: Message[], start: ThreadReplyStart | undefined, keepMessageId: string | null = null) {
  if (start === undefined || start === null) return replies;
  return replies.filter((reply) => !(reply.seq > 0) || reply.seq >= start || reply.id === keepMessageId);
}
