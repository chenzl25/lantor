import type { Message } from "./types";

export type MessageReferenceKind = "message" | "thread";

export type ParsedMessageReference = {
  key: string;
  kind: MessageReferenceKind;
  id: string;
  token: string;
};

export type MessageReference = {
  kind: MessageReferenceKind;
  id: string;
  token: string;
};

export type ResolvedMessageReference = MessageReference & {
  message: Pick<Message, "id" | "channel_id" | "thread_root_id" | "sender_name" | "body" | "created_at"> | null;
  channel: Pick<import("./types").Channel, "id" | "name"> | null;
  replyCount: number | null;
};

export type MessageReferenceLocation = {
  channelId: string;
  threadId: string | null;
  focusedMessageId: string;
  showThread: boolean;
};

export const MESSAGE_REFERENCE_PATTERN = /\[\[(message|thread):([0-9a-fA-F-]{8,36})\]\]/g;

export function messageReferenceToken(kind: MessageReferenceKind, id: string) {
  return `[[${kind}:${id}]]`;
}

export function appendMessageReferenceToken(text: string, kind: MessageReferenceKind, id: string) {
  const token = messageReferenceToken(kind, id);
  if (text.includes(token)) return text;
  const trimmedEnd = text.trimEnd();
  return `${trimmedEnd}${trimmedEnd ? "\n" : ""}${token}\n`;
}

export function parseMessageReferences(text: string): ParsedMessageReference[] {
  return Array.from(text.matchAll(MESSAGE_REFERENCE_PATTERN), (match, index) => ({
    key: `${match[0]}:${index}`,
    kind: match[1].toLowerCase() as MessageReferenceKind,
    id: match[2],
    token: match[0],
  }));
}

export function removeMessageReferenceToken(text: string, token: string) {
  return text.replace(token, "").replace(/\n{3,}/g, "\n\n").trimStart();
}

export function withoutMessageReferenceTokens(text: string) {
  return text.replace(MESSAGE_REFERENCE_PATTERN, "").replace(/\n{3,}/g, "\n\n").trim();
}

export function messageReferenceLocation(
  kind: MessageReferenceKind,
  message: Message,
): MessageReferenceLocation {
  const threadId = kind === "thread"
    ? message.thread_root_id ?? message.id
    : message.thread_root_id;
  return {
    channelId: message.channel_id,
    threadId,
    focusedMessageId: message.id,
    showThread: threadId !== null,
  };
}

export function resolveMessageReference(
  reference: MessageReference,
  messages: import("./types").Message[],
  channels: import("./types").Channel[],
): ResolvedMessageReference {
  const message = reference.kind === "thread"
    ? messages.find((item) => item.id === reference.id) ?? null
    : messages.find((item) => item.id === reference.id) ?? null;
  const channel = message ? channels.find((item) => item.id === message.channel_id) ?? null : null;
  const replyCount = reference.kind === "thread"
    ? messages.filter((item) => item.thread_root_id === reference.id).length
    : null;
  return { ...reference, message, channel, replyCount };
}
