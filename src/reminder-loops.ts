import type { Reminder } from "./types";

/** A loop is a repeating reminder whose check command runs on every tick. */
export function isLoopReminder(reminder: Pick<Reminder, "check_command">) {
  return Boolean(reminder.check_command?.trim());
}

export function formatReminderCadence(reminder: Pick<Reminder, "recurrence" | "interval_seconds">) {
  if (reminder.recurrence !== "interval") {
    return reminder.recurrence === "none" ? "" : reminder.recurrence;
  }
  const seconds = Math.max(0, reminder.interval_seconds ?? 0);
  if (seconds > 0 && seconds % 86_400 === 0) {
    const days = seconds / 86_400;
    return days === 1 ? "every day" : `every ${days} days`;
  }
  if (seconds > 0 && seconds % 3_600 === 0) {
    const hours = seconds / 3_600;
    return hours === 1 ? "every hour" : `every ${hours} h`;
  }
  const minutes = Math.max(1, Math.round(seconds / 60));
  return minutes === 1 ? "every minute" : `every ${minutes} min`;
}

export function loopActionLabel(reminder: Pick<Reminder, "check_action">, agentHandle: string) {
  if (reminder.check_action === "post") return "posts output";
  if (reminder.check_action === "wake") return `wakes @${agentHandle} on output`;
  return "";
}

export function loopCheckSummary(
  reminder: Pick<Reminder, "last_checked_at" | "last_check_status" | "last_check_detail" | "check_failures">,
  formatTime: (value: string) => string,
) {
  if (!reminder.last_checked_at) return "Not checked yet";
  const at = formatTime(reminder.last_checked_at);
  switch (reminder.last_check_status) {
    case "quiet":
      return `Last check ${at} · no output`;
    case "posted":
      return `Last check ${at} · posted output`;
    case "woke":
      return `Last check ${at} · woke the agent`;
    case "failed": {
      const count = reminder.check_failures > 1 ? ` (${reminder.check_failures} in a row)` : "";
      const detail = reminder.last_check_detail ? `: ${reminder.last_check_detail}` : "";
      return `Last check ${at} · failed${count}${detail}`;
    }
    default:
      return `Last check ${at}`;
  }
}
