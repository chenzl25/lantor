import assert from "node:assert/strict";
import test from "node:test";

import { formatReminderCadence, isLoopReminder, loopActionLabel, loopCheckSummary } from "../src/reminder-loops";

const at = (value: string) => `<${value}>`;

test("loop reminders are the ones with a check command", () => {
  assert.equal(isLoopReminder({ check_command: "./poll.sh" }), true);
  assert.equal(isLoopReminder({ check_command: "  " }), false);
  assert.equal(isLoopReminder({ check_command: "" }), false);
});

test("cadence reads as an interval for loops and as the recurrence otherwise", () => {
  assert.equal(formatReminderCadence({ recurrence: "interval", interval_seconds: 300 }), "every 5 min");
  assert.equal(formatReminderCadence({ recurrence: "interval", interval_seconds: 60 }), "every minute");
  assert.equal(formatReminderCadence({ recurrence: "interval", interval_seconds: 5_400 }), "every 90 min");
  assert.equal(formatReminderCadence({ recurrence: "interval", interval_seconds: 7_200 }), "every 2 h");
  assert.equal(formatReminderCadence({ recurrence: "interval", interval_seconds: 86_400 }), "every day");
  assert.equal(formatReminderCadence({ recurrence: "daily", interval_seconds: null }), "daily");
  assert.equal(formatReminderCadence({ recurrence: "none", interval_seconds: null }), "");
});

test("action and last check summaries name what happened", () => {
  assert.equal(loopActionLabel({ check_action: "post" }, "Speed"), "posts output");
  assert.equal(loopActionLabel({ check_action: "wake" }, "Speed"), "wakes @Speed on output");
  const base = { last_checked_at: "T", last_check_detail: "", check_failures: 0 };
  assert.equal(loopCheckSummary({ ...base, last_checked_at: null, last_check_status: "" }, at), "Not checked yet");
  assert.equal(loopCheckSummary({ ...base, last_check_status: "quiet" }, at), "Last check <T> · no output");
  assert.equal(loopCheckSummary({ ...base, last_check_status: "posted" }, at), "Last check <T> · posted output");
  assert.equal(
    loopCheckSummary({ ...base, last_check_status: "failed", last_check_detail: "exit status 3", check_failures: 3 }, at),
    "Last check <T> · failed (3 in a row): exit status 3",
  );
});
