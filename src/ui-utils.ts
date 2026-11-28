import { Agent, Message, OwnerProfile } from "./types";

export function ownerAsAvatarAgent(profile: OwnerProfile) {
  return {
    id: "owner-profile",
    handle: "owner",
    display_name: profile.display_name,
    status: "idle",
    avatar: profile.avatar,
    description: profile.description,
  };
}

export function agentForMessageSender(message: Message, agents: ReadonlyMap<string, Agent>) {
  if (message.sender_role === "owner" || message.sender_role === "system") return null;
  if (!message.sender_agent_id) return null;
  return agents.get(message.sender_agent_id) ?? null;
}

export function deletedAgentForMessageSender(message: Message) {
  if (message.sender_role === "owner" || message.sender_role === "system") return null;
  if (message.sender_agent_id) return null;
  const handle = message.sender_name.replace(/^@/, "").trim() || "deleted-agent";
  return {
    id: `deleted-agent:${message.id}`,
    handle,
    display_name: message.sender_name || "Deleted agent",
    role: "Deleted agent",
    status: "deleted",
    avatar: "",
    description: "This agent has been deleted.",
  };
}

export function visibleChannelDescription(description: string) {
  return description.trim() === "Local channel" ? "" : description;
}

export function visibleAgentDescription(description: string) {
  const trimmed = description.trim();
  return /^local agent\.?$/i.test(trimmed) ? "" : trimmed;
}

// Named presets are stable locale/options cache keys. Cache formatters, not
// formatted labels: Today/Yesterday and relative time must still advance.
const TIME_FORMAT_OPTIONS = {
  time: { hour: "2-digit", minute: "2-digit" },
  dateTime: { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" },
  shortDate: { month: "short", day: "numeric" },
  shortDateWithYear: { month: "short", day: "numeric", year: "numeric" },
  clock: { hour: "2-digit", minute: "2-digit", hourCycle: "h23" },
  divider: { weekday: "long", month: "long", day: "numeric" },
} satisfies Record<string, Intl.DateTimeFormatOptions>;
const timeFormatters = new Map<keyof typeof TIME_FORMAT_OPTIONS, Intl.DateTimeFormat>();
function timeFormatter(preset: keyof typeof TIME_FORMAT_OPTIONS) {
  let formatter = timeFormatters.get(preset);
  if (!formatter) {
    formatter = new Intl.DateTimeFormat("en", TIME_FORMAT_OPTIONS[preset]);
    timeFormatters.set(preset, formatter);
  }
  return formatter;
}

export function formatByteSize(value: number) {
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
  return `${(value / 1024 / 1024).toFixed(1)} MB`;
}

export function formatTime(value: string) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  const clock = timeFormatter("time").format(date);
  const now = new Date();
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  if (isSameCalendarDay(value, now.toISOString())) return `Today ${clock}`;
  if (isSameCalendarDay(value, yesterday.toISOString())) return `Yesterday ${clock}`;
  return timeFormatter("dateTime").format(date);
}

export function formatRelativeTime(value: string) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  const seconds = Math.round((Date.now() - date.getTime()) / 1000);
  if (seconds < 45) return "just now";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.round(hours / 24);
  if (days < 7) return `${days}d ago`;
  const weeks = Math.round(days / 7);
  if (weeks < 5) return `${weeks}w ago`;
  const now = new Date();
  return timeFormatter(date.getFullYear() === now.getFullYear() ? "shortDate" : "shortDateWithYear").format(date);
}

export function formatShortDate(value: string) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return timeFormatter(date.getFullYear() === new Date().getFullYear() ? "shortDate" : "shortDateWithYear").format(date);
}

export function formatClockTime(value: string) {
  return timeFormatter("clock").format(new Date(value));
}

export function formatDateDivider(value: string) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  const now = new Date();
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  if (isSameCalendarDay(value, now.toISOString())) return "Today";
  if (isSameCalendarDay(value, yesterday.toISOString())) return "Yesterday";
  return timeFormatter("divider").format(date);
}

export function isSameCalendarDay(left: string, right: string) {
  const leftDate = new Date(left);
  const rightDate = new Date(right);
  if (Number.isNaN(leftDate.getTime()) || Number.isNaN(rightDate.getTime())) return false;
  return (
    leftDate.getFullYear() === rightDate.getFullYear() &&
    leftDate.getMonth() === rightDate.getMonth() &&
    leftDate.getDate() === rightDate.getDate()
  );
}

export function firstLines(text: string, lines = 8) {
  const split = text.trim().split("\n");
  return split.slice(0, lines).join("\n") + (split.length > lines ? "\n..." : "");
}

export function agentRequestSourceLabel(sourceKind: string, taskNumber?: number | null) {
  if (taskNumber) return `Task #${taskNumber}`;
  switch (sourceKind) {
    case "mention":
      return "Mention";
    case "dm":
      return "DM";
    case "thread_followup":
      return "Thread reply";
    case "collaboration":
      return "Handed over";
    case "reminder":
      return "Reminder";
    case "task":
      return "Task";
    case "manual":
      return "Your request";
    default:
      return "Request";
  }
}
