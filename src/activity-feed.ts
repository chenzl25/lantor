import { isProgressOnlyMessage } from "./message-grouping";
import type { ActivityFeedItem, Agent, Channel, Message, ThreadActivity } from "./types";
import { markdownPreviewText } from "./markdown-preview";
import { firstLines } from "./ui-utils";

type ActivityFeedSource = {
  channels: Channel[];
  agents: Agent[];
  messages: Message[];
  threadActivities: ThreadActivity[];
  unfollowedThreadIds: ReadonlySet<string>;
  ownerMentionHandles: readonly string[];
};

// Activity is a view of agent messages. System/owner roots still supply thread
// context, but neither they nor task/reminder state can produce a feed entry.
export function buildActivityFeedItems(source: ActivityFeedSource): ActivityFeedItem[] {
  const channels = new Map(source.channels.map(channel => [channel.id, channel]));
  const agents = new Map(source.agents.map(agent => [agent.id, agent]));
  const roots = new Map(source.messages.filter(message => !message.thread_root_id).map(message => [message.id, message]));
  const activities = new Map(source.threadActivities.map(activity => [activity.thread_root_id, activity]));
  // Agents may use a custom role name rather than the literal "agent".
  const messages = source.messages
    .filter(message => message.sender_role !== "owner" && message.sender_role !== "system" && !isProgressOnlyMessage(message))
    .sort((left, right) => left.seq - right.seq);
  const latestByChannel = new Map<string, Message>();
  const latestByThread = new Map<string, Message>();
  for (const message of messages) {
    latestByChannel.set(message.channel_id, message);
    if (message.thread_root_id) latestByThread.set(message.thread_root_id, message);
  }

  const threadIds = new Set([...latestByThread.keys()].filter(id => {
    const root = roots.get(id);
    return root && !source.unfollowedThreadIds.has(id)
      && (root.thread_followed || (activities.get(id)?.agent_unread_count ?? 0) > 0);
  }));
  const surface = (channel: Channel) => channel.kind === "dm" ? "Direct message" : `#${channel.name}`;
  const text = (message: Message) => (message.body && markdownPreviewText(message.body))
    || (message.attachments.length ? "Shared files" : "Shared a report");
  const fromMessage = (message: Message, kind: ActivityFeedItem["kind"], id: string, unreadCount: number): ActivityFeedItem => ({
    id,
    dismissId: id,
    kind,
    title: firstLines(text(message), 1),
    excerpt: text(message),
    surface: channels.has(message.channel_id) ? surface(channels.get(message.channel_id)!) : "Unknown",
    actor: message.sender_name,
    timestamp: message.created_at,
    unread: unreadCount > 0,
    actorAgentId: message.sender_agent_id,
    actorRole: message.sender_role,
    channelId: message.channel_id,
    threadId: message.thread_root_id,
    messageId: message.id,
    replyCount: message.thread_root_id ? activities.get(message.thread_root_id)?.reply_count ?? 0 : 0,
    newCount: unreadCount,
  });

  const items: ActivityFeedItem[] = [];
  for (const channel of source.channels) {
    const unreadCount = channel.agent_unread_count ?? 0;
    const latest = latestByChannel.get(channel.id);
    if (!latest || unreadCount === 0) continue;
    if (latest.thread_root_id && threadIds.has(latest.thread_root_id)) continue;
    const item = fromMessage(latest, channel.kind === "dm" ? "dm" : "channel", `${channel.kind}:${channel.id}`, unreadCount);
    item.title = channel.kind === "dm"
      ? `DM with @${agents.get(channel.dm_agent_id ?? "")?.handle ?? "agent"}`
      : `New in #${channel.name}`;
    items.push(item);
  }

  for (const id of threadIds) {
    const latest = latestByThread.get(id)!;
    const activity = activities.get(id);
    const item = fromMessage(latest, "thread", `thread:${id}`, activity?.agent_unread_count ?? 0);
    // Hydration can contain only the latest reply; the first unread agent reply
    // may need to be loaded when the user opens this item.
    if (item.unread && activity?.first_unread_agent_message_id) {
      item.messageId = activity.first_unread_agent_message_id;
    }
    items.push(item);
  }

  const handles = source.ownerMentionHandles.map(handle => handle.toLowerCase());
  for (const message of messages) {
    if (!handles.some(handle => message.body.toLowerCase().includes(handle))) continue;
    const unreadCount = message.thread_root_id
      ? activities.get(message.thread_root_id)?.agent_unread_count ?? 0
      : channels.get(message.channel_id)?.agent_unread_count ?? 0;
    const item = fromMessage(message, "mention", `mention:${message.id}`, unreadCount);
    item.threadId = message.thread_root_id ?? message.id;
    items.push(item);
  }
  return items;
}
