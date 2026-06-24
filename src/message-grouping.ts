import type { Message } from "./types";

const COMPACT_FOLLOWUP_WINDOW_MS = 5 * 60 * 1000;

export function messageRunId(message: Message) {
  const [runId] = message.stream_key.split(":");
  if (!runId) return null;
  return /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(runId)
    ? runId
    : null;
}

export function messageHasVisibleContent(message: Message) {
  return Boolean(message.body.trim() || message.attachments.length > 0 || message.artifacts.length > 0);
}

export function isProgressOnlyMessage(message: Message) {
  if (!messageRunId(message)) return false;
  if (message.sender_role === "owner" || message.sender_role === "system") return false;
  if (message.delivery_state === "streaming") return !messageHasVisibleContent(message);
  return message.delivery_state === "complete" && !messageHasVisibleContent(message);
}

export function wasEdited(message: Message) {
  if (message.stream_key) return false;
  const created = new Date(message.created_at).getTime();
  const updated = new Date(message.updated_at).getTime();
  return Number.isFinite(created) && Number.isFinite(updated) && updated - created > 1000;
}

export function isCompactFollowupMessage(message: Message, previous: Message | null | undefined) {
  if (!previous) return false;
  if (message.sender_role === "system" || previous.sender_role === "system") return false;
  if (message.is_task) return false;
  if (message.delivery_state !== "complete") return false;
  if (message.sender_name !== previous.sender_name || message.sender_role !== previous.sender_role) return false;

  const createdAt = new Date(message.created_at).getTime();
  const previousCreatedAt = new Date(previous.created_at).getTime();
  if (!Number.isFinite(createdAt) || !Number.isFinite(previousCreatedAt)) return false;
  const updatedAt = new Date(message.updated_at).getTime();
  if (Number.isFinite(updatedAt) && updatedAt - createdAt > 1000) return false;

  const delta = createdAt - previousCreatedAt;
  return delta >= 0 && delta <= COMPACT_FOLLOWUP_WINDOW_MS;
}
