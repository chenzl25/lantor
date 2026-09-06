import type { Message } from "./types";

export type StreamingMessageSnapshot = Pick<Message, "body" | "delivery_state">;

// Only transient text lives here. Message identity/metadata and final responses
// remain in Bootstrap. A subscription belongs to one message, not the App.
export class StreamingMessageStore {
  private snapshots = new Map<string, StreamingMessageSnapshot>();
  private listeners = new Map<string, Set<() => void>>();

  get(id: string) { return this.snapshots.get(id); }

  subscribe(id: string, listener: () => void) {
    const listeners = this.listeners.get(id) ?? new Set();
    listeners.add(listener);
    this.listeners.set(id, listeners);
    return () => {
      listeners.delete(listener);
      if (!listeners.size) this.listeners.delete(id);
    };
  }

  publish(id: string, snapshot: StreamingMessageSnapshot) {
    const previous = this.snapshots.get(id);
    if (previous?.body === snapshot.body && previous.delivery_state === snapshot.delivery_state) return;
    this.snapshots.set(id, snapshot);
    this.listeners.get(id)?.forEach((listener) => listener());
  }

  // A late page/snapshot may carry an earlier prefix of an active stream.
  body(id: string, baseline: string) {
    const snapshot = this.get(id);
    return snapshot && (snapshot.delivery_state !== "streaming" || snapshot.body.length >= baseline.length)
      ? snapshot.body : baseline;
  }

  remove(id: string) {
    if (!this.snapshots.delete(id)) return;
    this.listeners.get(id)?.forEach((listener) => listener());
  }

  // Run after the master snapshot commits, so a final upsert never exposes the
  // old placeholder between removing an overlay and rendering its final body.
  reconcile(messages: readonly Message[]) {
    if (!this.snapshots.size) return;
    const byId = new Map(messages.map((message) => [message.id, message]));
    for (const [id, snapshot] of this.snapshots) {
      const message = byId.get(id);
      if (!message || message.delivery_state !== "streaming" ||
        (message.body === snapshot.body && message.delivery_state === snapshot.delivery_state)) {
        this.remove(id);
      } else if (snapshot.delivery_state === "streaming" && message.body.length > snapshot.body.length) {
        this.publish(id, { body: message.body, delivery_state: message.delivery_state });
      }
    }
  }

  clear() {
    for (const id of this.snapshots.keys()) this.remove(id);
  }
}

export const streamingMessages = new StreamingMessageStore();

export function streamedMessage(message: Message, snapshot: StreamingMessageSnapshot | undefined): Message {
  if (!snapshot || message.delivery_state !== "streaming") return message;
  if (snapshot.delivery_state === "streaming" && snapshot.body.length < message.body.length) return message;
  if (snapshot.body === message.body && snapshot.delivery_state === message.delivery_state) return message;
  return { ...message, ...snapshot };
}
