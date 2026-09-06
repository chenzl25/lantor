import { useCallback, useMemo, useSyncExternalStore } from "react";
import type { Message } from "../types";
import { streamedMessage, streamingMessages } from "../streaming-message-store";

export function useStreamingMessage(message: Message) {
  const subscribe = useCallback((listener: () => void) => streamingMessages.subscribe(message.id, listener), [message.id]);
  const getSnapshot = useCallback(() => streamingMessages.get(message.id), [message.id]);
  const snapshot = useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
  return useMemo(() => streamedMessage(message, snapshot), [message, snapshot]);
}
