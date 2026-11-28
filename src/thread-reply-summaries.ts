import type { Message, ThreadActivity, ThreadParticipant, ThreadReplySummary } from "./types";

function sameSender(a: ThreadParticipant, b: ThreadParticipant) {
  return a.sender_role === b.sender_role
    && a.sender_name === b.sender_name
    && a.sender_agent_id === b.sender_agent_id;
}

/**
 * The reply row of each thread root: the count, the latest reply, and who
 * replied. A channel page loads only the roots, so the server's thread
 * activities supply the count and the first senders; replies loaded here add
 * any sender the server has not reported yet.
 */
export function buildThreadReplySummaries(
  visibleMessages: readonly Message[],
  activities: readonly ThreadActivity[],
): Record<string, ThreadReplySummary> {
  const loadedSenders: Record<string, ThreadParticipant[]> = {};
  const summaries: Record<string, ThreadReplySummary> = {};
  for (const message of visibleMessages) {
    if (!message.thread_root_id) continue;
    const summary = summaries[message.thread_root_id] ??= { count: 0, latest: null, participants: [] };
    summary.count += 1;
    if (!summary.latest || new Date(message.created_at) > new Date(summary.latest.created_at)) {
      summary.latest = message;
    }
    const senders = loadedSenders[message.thread_root_id] ??= [];
    if (message.sender_role !== "system" && !senders.some((sender) => sameSender(sender, message))) {
      senders.push(message);
    }
  }
  for (const activity of activities) {
    if (activity.reply_count === undefined) continue;
    const summary = summaries[activity.thread_root_id] ??= { count: 0, latest: null, participants: [] };
    summary.count = Math.max(summary.count, activity.reply_count);
    summary.latestAt = summary.latest && Date.parse(summary.latest.created_at) > Date.parse(activity.latest_activity_at)
      ? summary.latest.created_at : activity.latest_activity_at;
    summary.participants = [...(activity.participants ?? [])];
  }
  for (const [rootId, senders] of Object.entries(loadedSenders)) {
    const participants = summaries[rootId].participants;
    for (const sender of senders) {
      if (!participants.some((participant) => sameSender(participant, sender))) participants.push(sender);
    }
  }
  return summaries;
}
