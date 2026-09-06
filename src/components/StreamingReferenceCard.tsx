import { useCallback, useSyncExternalStore } from "react";
import type { MessageReference, ResolvedMessageReference } from "../message-references";
import type { MessageReferenceStore } from "../message-reference-store";
import { MessageReferenceCard } from "./MessageReferenceCard";

export function StreamingReferenceCard({ reference, store, onOpen }: {
  reference: MessageReference;
  store: MessageReferenceStore;
  onOpen?: (reference: ResolvedMessageReference) => void;
}) {
  const { kind, id, token } = reference;
  const subscribe = useCallback((listener: () => void) => store.subscribe({ kind, id, token }, listener), [store, kind, id, token]);
  const getSnapshot = useCallback(() => store.get({ kind, id, token }), [store, kind, id, token]);
  const resolved = useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
  return <MessageReferenceCard reference={resolved} compact onOpen={onOpen} />;
}
