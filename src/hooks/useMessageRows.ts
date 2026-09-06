import { useLayoutEffect, useMemo, useState } from "react";
import { MessageReferenceStore } from "../message-reference-store";
import { parseMessageReferences, type ResolvedMessageReference } from "../message-references";
import type { Agent, Channel, Message, OwnerProfile, ThreadReplySummary } from "../types";
import { agentForMessageSender, deletedAgentForMessageSender, ownerAsAvatarAgent } from "../ui-utils";
import type { ActiveAgentProgress } from "../components/ActivityProgressDock";
import { useRetainedValue } from "./useRetainedValue";

type AvatarAgent = ActiveAgentProgress["agent"];
export type ReplyParticipant = { key: string; agent: AvatarAgent | null; title?: string; fallback: string };
export type MessageRowData = {
  message: Message;
  agent: Agent | null;
  deletedAgent: AvatarAgent | null;
  ownerAvatar: AvatarAgent | null;
  references: Record<string, ResolvedMessageReference> | undefined;
  reply: {
    latestAt: string | null;
    participants: ReplyParticipant[];
    progress: ActiveAgentProgress[];
  } | null;
};
const NO_SUMMARIES: Record<string, ThreadReplySummary> = {};
const NO_PROGRESS: Record<string, ActiveAgentProgress[]> = {};

export function useMessageRows(
  visibleMessages: Message[], messages: Message[], channels: Channel[], agents: Agent[], owner: OwnerProfile,
  hideAgent = false,
  summaries: Record<string, ThreadReplySummary> = NO_SUMMARIES,
  progressByRoot: Record<string, ActiveAgentProgress[]> = NO_PROGRESS,
) {
  const [referenceStore] = useState(() => new MessageReferenceStore(messages, channels));
  useLayoutEffect(() => { referenceStore.update(messages, channels); }, [referenceStore, messages, channels]);
  const index = useMemo(() => {
    const replyCounts = new Map<string, number>();
    for (const message of messages) {
      if (message.thread_root_id) replyCounts.set(message.thread_root_id, (replyCounts.get(message.thread_root_id) ?? 0) + 1);
    }
    return {
      messages: new Map(messages.map((message) => [message.id, message])),
      channels: new Map(channels.map((channel) => [channel.id, channel])),
      replyCounts,
    };
  }, [messages, channels]);
  const rows = useMemo(() => {
    const agentsById = new Map(agents.map((agent) => [agent.id, agent]));
    const ownerAvatar = ownerAsAvatarAgent(owner);
    return Object.fromEntries(visibleMessages.map((message): [string, MessageRowData] => {
      const agent = hideAgent ? null : agentForMessageSender(message, agentsById);
      const summary = summaries[message.id];
      const progress = progressByRoot[message.id] ?? [];
      const activeAgentIds = new Set(progress.map((item) => item.agent.id).filter(Boolean));
      const participants = (summary?.participants ?? [])
        .filter((item) => !item.sender_agent_id || !activeAgentIds.has(item.sender_agent_id))
        .slice(0, Math.max(0, 3 - progress.length))
        .map((item): ReplyParticipant => {
          const live = agentForMessageSender(item, agentsById);
          const deleted = live ? null : deletedAgentForMessageSender(item);
          return {
            key: `${item.sender_role}:${item.sender_agent_id ?? item.sender_name}`,
            agent: live ?? deleted ?? (item.sender_role === "owner" ? ownerAvatar : null),
            title: live ? `@${live.handle}` : deleted ? `@${deleted.handle} has been deleted` : undefined,
            fallback: item.sender_name.slice(0, 1),
          };
        });
      let references: MessageRowData["references"];
      if (message.body.includes("[[") || message.body.includes("/lantor/reference/")) {
        const parsed = parseMessageReferences(message.body);
        for (const [, kind, id] of message.body.matchAll(/\/lantor\/reference\/(message|thread)\/([0-9a-fA-F-]{8,36})/g)) {
          parsed.push({ kind: kind as "message" | "thread", id, token: `[[${kind}:${id}]]`, key: `${kind}:${id}` });
        }
        references = Object.fromEntries(parsed.map(({ kind, id, token }) => {
          const target = index.messages.get(id) ?? null;
          const channel = target ? index.channels.get(target.channel_id) : null;
          return [`${kind}:${id}`, {
            kind, id, token,
            message: target ? { id: target.id, channel_id: target.channel_id, thread_root_id: target.thread_root_id,
              sender_name: target.sender_name, body: target.body, created_at: target.created_at } : null,
            channel: channel ? { id: channel.id, name: channel.name } : null,
            replyCount: kind === "thread" ? index.replyCounts.get(id) ?? 0 : null,
          }];
        }));
      }
      return [message.id, {
        message, agent,
        deletedAgent: hideAgent || agent ? null : deletedAgentForMessageSender(message),
        ownerAvatar: message.sender_role === "owner" ? ownerAvatar : null,
        references,
        reply: summary || progress.length ? { latestAt: summary?.latest?.created_at ?? null, participants, progress } : null,
      }];
    }));
  }, [visibleMessages, messages, channels, index, agents, owner, hideAgent, summaries, progressByRoot]);
  // Bootstrap replaces JSON objects. Share unchanged rows (and Markdown reference
  // slices) by content; only affected rows cross the memo boundary.
  return { rows: useRetainedValue(rows), referenceStore };
}
