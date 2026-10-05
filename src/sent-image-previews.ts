import { useSyncExternalStore } from "react";
import { attachmentAssetUrl } from "./apiClient";
import type { MessageAttachment } from "./types";

/**
 * While a send is in flight, the sender sees each image from a local blob URL.
 * Once the server acknowledges the send, the message switches to the server's
 * attachment URL, which may take a moment to produce (the web server generates
 * thumbnails on first request). Until that image has loaded, keep showing the
 * local copy so a just-sent image never goes blank.
 */

// Bounds how long a blob stays alive if the server image never settles.
const MAX_HOLD_MS = 30_000;

type HeldPreview = {
  objectUrl: string;
  timer: ReturnType<typeof setTimeout>;
  loader: HTMLImageElement;
};

const held = new Map<string, HeldPreview>();
const listeners = new Set<() => void>();

function notify() {
  listeners.forEach((listener) => listener());
}

/**
 * Pairs each persisted image attachment with the local copy it was uploaded
 * from. The server assigns new ids and may reorder rows, so match on name and
 * size. Local URLs without a persisted image counterpart are returned unpaired.
 */
export function pairSentImagePreviews(local: MessageAttachment[], persisted: MessageAttachment[]) {
  const unclaimed = local.filter((attachment) => attachment.local_url);
  const pairs: { attachment: MessageAttachment; objectUrl: string }[] = [];
  for (const attachment of persisted) {
    if (!attachment.mime_type.startsWith("image/")) continue;
    const index = unclaimed.findIndex(
      (candidate) =>
        candidate.original_name === attachment.original_name && candidate.size_bytes === attachment.size_bytes,
    );
    if (index < 0) continue;
    const [match] = unclaimed.splice(index, 1);
    pairs.push({ attachment, objectUrl: match.local_url! });
  }
  return { pairs, unpaired: unclaimed.map((attachment) => attachment.local_url!) };
}

function release(attachmentId: string, objectUrl: string) {
  const entry = held.get(attachmentId);
  if (entry?.objectUrl !== objectUrl) return;
  held.delete(attachmentId);
  clearTimeout(entry.timer);
  URL.revokeObjectURL(objectUrl);
  notify();
}

function holdUntilLoaded(attachment: MessageAttachment, objectUrl: string) {
  const previous = held.get(attachment.id);
  if (previous) release(attachment.id, previous.objectUrl);
  const loader = new Image();
  const settle = () => release(attachment.id, objectUrl);
  held.set(attachment.id, { objectUrl, loader, timer: setTimeout(settle, MAX_HOLD_MS) });
  loader.src = attachmentAssetUrl(attachment.storage_path, attachment.id, true);
  // Swap only after decoding, so the server image paints immediately. A failed
  // load swaps too: the message then shows the server result as before.
  loader.decode().then(settle, settle);
  notify();
}

/**
 * Hands the local copies of a settled send over to the persisted attachments.
 * Every object URL in `local` is either held until its server image loads or
 * revoked now.
 */
export function handOffSentImagePreviews(local: MessageAttachment[], persisted: MessageAttachment[]) {
  const { pairs, unpaired } = pairSentImagePreviews(local, persisted);
  unpaired.forEach((objectUrl) => URL.revokeObjectURL(objectUrl));
  pairs.forEach(({ attachment, objectUrl }) => holdUntilLoaded(attachment, objectUrl));
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** The local copy to show for a persisted attachment, while its server image loads. */
export function useSentImagePreview(attachmentId: string) {
  return useSyncExternalStore(
    subscribe,
    () => held.get(attachmentId)?.objectUrl,
    () => undefined,
  );
}
