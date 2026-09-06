import type { Channel, Message } from "./types";
import type { MessageReference, ResolvedMessageReference } from "./message-references";
import { retainEqual } from "./render-identity";

// Newly arriving stream text can introduce references without a parent render.
// Chips subscribe to their target; passing full message arrays to streaming rows
// would make concurrent streams invalidate each other's memo boundaries.
export class MessageReferenceStore {
  private messages = new Map<string, Message>();
  private channels = new Map<string, Channel>();
  private replyCounts = new Map<string, number>();
  private snapshots = new Map<string, ResolvedMessageReference>();
  private listeners = new Map<string, Set<() => void>>();

  constructor(messages: Message[], channels: Channel[]) { this.update(messages, channels); }

  private resolve(reference: MessageReference): ResolvedMessageReference {
    const message = this.messages.get(reference.id);
    const channel = message ? this.channels.get(message.channel_id) : null;
    return {
      ...reference,
      message: message ? { id: message.id, channel_id: message.channel_id, thread_root_id: message.thread_root_id,
        sender_name: message.sender_name, body: message.body, created_at: message.created_at } : null,
      channel: channel ? { id: channel.id, name: channel.name } : null,
      replyCount: reference.kind === "thread" ? this.replyCounts.get(reference.id) ?? 0 : null,
    };
  }

  get(reference: MessageReference) {
    const key = `${reference.kind}:${reference.id}`;
    let snapshot = this.snapshots.get(key);
    if (!snapshot) { snapshot = this.resolve(reference); this.snapshots.set(key, snapshot); }
    return snapshot;
  }

  subscribe(reference: MessageReference, listener: () => void) {
    const key = `${reference.kind}:${reference.id}`;
    const listeners = this.listeners.get(key) ?? new Set();
    listeners.add(listener);
    this.listeners.set(key, listeners);
    return () => {
      listeners.delete(listener);
      if (!listeners.size) { this.listeners.delete(key); this.snapshots.delete(key); }
    };
  }

  update(messages: Message[], channels: Channel[]) {
    this.messages = new Map(messages.map((message) => [message.id, message]));
    this.channels = new Map(channels.map((channel) => [channel.id, channel]));
    this.replyCounts = new Map();
    for (const message of messages) {
      if (message.thread_root_id) this.replyCounts.set(message.thread_root_id, (this.replyCounts.get(message.thread_root_id) ?? 0) + 1);
    }
    for (const [key, before] of this.snapshots) {
      const after = retainEqual(before, this.resolve({ kind: before.kind, id: before.id, token: before.token }));
      if (after === before) continue;
      this.snapshots.set(key, after);
      this.listeners.get(key)?.forEach((listener) => listener());
    }
  }
}
