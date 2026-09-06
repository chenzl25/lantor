import type { Bootstrap } from "./types";
import { removeChannelsFromBootstrap, savedMessagesWithState, type SnapshotOptimisticState } from "./state-sync";

export type UiStateScope = "owner_profile" | "channels" | "channel_members" | "thread_activities"
  | "agents" | "saved_messages" | "dismissed_inbox_items" | "read_inbox_items" | "artifacts"
  | "tasks" | "reminders" | "agent_schedules" | "agent_runs" | "agent_work_items"
  | "agent_activities" | "supervisor" | "launch_agent";
export type UiStatePatch = Partial<Pick<Bootstrap, UiStateScope>>;

// A refresh event invalidates these collections, not the entire workspace.
// Unknown protocol events and expired replay cursors deliberately recover with a snapshot.
export function scopesForRefresh(reason?: string): UiStateScope[] | null {
  if (!reason || reason === "event_replay_gap") return null;
  if (reason === "message") return ["channels", "thread_activities", "tasks"];
  if (reason === "channel_read") return ["channels"];
  if (reason === "saved_message_updated") return ["saved_messages"];
  if (reason === "owner_profile_updated") return ["owner_profile"];
  if (reason.startsWith("owner_inbox_")) return ["dismissed_inbox_items", "read_inbox_items", "channels", "thread_activities"];
  if (reason === "inbox_read" || reason === "inbox_archived") return ["agents"];
  if (reason.startsWith("task_") || /^github_(issue|review)_task_/.test(reason)) return ["tasks", "channels", "thread_activities"];
  if (reason.startsWith("github_")) return ["channels"];
  if (reason.startsWith("agent_schedule_")) return ["agent_schedules"];
  if (reason.startsWith("reminder_")) return ["reminders"];
  if (reason === "profile_update" || reason === "agent_updated" || reason === "agent_created") return ["agents", "channel_members"];
  if (reason === "agent_deleted") return ["agents", "channel_members", "channels", "tasks", "agent_work_items", "agent_runs", "agent_schedules", "reminders"];
  if (reason === "channel_deleted") return ["channels"];
  if (reason.startsWith("channel_") || reason === "dm_opened") return ["channels", "channel_members"];
  if (reason === "agent_queued" || reason === "supervisor_command") return ["agents", "supervisor"];
  if (reason.startsWith("supervisor_service_")) return ["launch_agent", "supervisor"];
  if (reason === "held_outputs_expired") return ["agent_work_items"];
  return null;
}

export function applyUiStatePatch(current: Bootstrap, patch: UiStatePatch, optimistic: SnapshotOptimisticState): Bootstrap {
  let next = { ...current, ...patch };
  if (patch.channels) {
    const persisted = new Set(patch.channels.map((channel) => channel.id));
    const removed = new Set(current.channels.filter((channel) => !persisted.has(channel.id)
      && !optimistic.channels.has(channel.id)).map((channel) => channel.id));
    if (removed.size > 0) next = removeChannelsFromBootstrap(next, removed);
    next.channels = next.channels.filter((channel) => !optimistic.removedChannelIds.has(channel.id));
    for (const channel of optimistic.channels.values()) {
      if (!persisted.has(channel.id) && !optimistic.removedChannelIds.has(channel.id)) next.channels.push(channel);
    }
  }
  if (patch.tasks) {
    const tasks = new Map(patch.tasks.map((task) => [task.message_id, task]));
    next.messages = next.messages.map((message) => {
      const task = tasks.get(message.id);
      return task && (message.task_status !== task.status || message.task_number !== task.number)
        ? { ...message, task_status: task.status, task_number: task.number } : message;
    });
  }
  if (patch.saved_messages) {
    for (const [id, override] of optimistic.savedToggles) {
      next.saved_messages = savedMessagesWithState(next.saved_messages, id, override.saved, override.entry);
    }
  }
  return next;
}
