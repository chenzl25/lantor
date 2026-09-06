import { useCallback, useMemo, useSyncExternalStore } from "react";
import type { Message } from "../types";
import { streamedMessage, streamingMessages } from "../streaming-message-store";

export function useStreamingMessage(message: Message, finalOnly = false) {
  const subscribe = useCallback((listener: () => void) => finalOnly ? () => {} : streamingMessages.subscribe(message.id, listener), [message.id, finalOnly]);
  const getSnapshot = useCallback(() => finalOnly ? undefined : streamingMessages.get(message.id), [message.id, finalOnly]);
  const snapshot = useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
  return useMemo(() => finalOnly && message.delivery_state === "streaming"
    ? { ...message, body: "" } : streamedMessage(message, snapshot), [message, snapshot, finalOnly]);
}
