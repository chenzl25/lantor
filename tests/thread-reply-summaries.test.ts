import assert from "node:assert/strict";
import test from "node:test";

import { buildThreadReplySummaries } from "../src/thread-reply-summaries";
import type { Message, ThreadActivity } from "../src/types";

function reply(overrides: Partial<Message> = {}): Message {
  return {
    id: "reply-id",
    seq: 1,
    channel_id: "channel-id",
    thread_root_id: "root-id",
    sender_agent_id: null,
    sender_name: "Dylan",
    sender_role: "owner",
    body: "",
    is_task: false,
    thread_followed: false,
    delivery_state: "complete",
    stream_key: "",
    task_number: null,
    task_status: null,
    attachments: [],
    artifacts: [],
    created_at: "2026-11-28T00:00:00.000Z",
    updated_at: "2026-11-28T00:00:00.000Z",
    ...overrides,
  };
}

function activity(overrides: Partial<ThreadActivity> = {}): ThreadActivity {
  return {
    reply_count: 5,
    thread_root_id: "root-id",
    channel_id: "channel-id",
    unread_count: 0,
    latest_message_id: "latest-id",
    latest_activity_at: "2026-11-28T06:49:00.000Z",
    ...overrides,
  };
}

const speed = { sender_role: "agent", sender_name: "Speed", sender_agent_id: "speed-id" };
const owner = { sender_role: "owner", sender_name: "Dylan", sender_agent_id: null };

test("a root whose replies are not loaded takes its reply senders from the server", () => {
  const summaries = buildThreadReplySummaries([], [activity({ participants: [owner, speed] })]);
  assert.equal(summaries["root-id"].count, 5);
  assert.deepEqual(summaries["root-id"].participants, [owner, speed]);
});

test("loaded replies add only the senders the server has not reported yet", () => {
  const summaries = buildThreadReplySummaries(
    [
      reply({ id: "a", ...speed }),
      reply({ id: "b", sender_role: "system", sender_name: "Lantor" }),
      reply({ id: "c", sender_role: "agent", sender_name: "Arch", sender_agent_id: "arch-id" }),
    ],
    [activity({ participants: [owner, speed] })],
  );
  assert.deepEqual(
    summaries["root-id"].participants.map((participant) => participant.sender_name),
    ["Dylan", "Speed", "Arch"],
  );
});

test("without server senders the loaded replies name the senders, as before", () => {
  const summaries = buildThreadReplySummaries(
    [reply({ id: "a", ...speed }), reply({ id: "b", ...owner }), reply({ id: "c", ...speed })],
    [],
  );
  assert.equal(summaries["root-id"].count, 3);
  assert.deepEqual(
    summaries["root-id"].participants.map((participant) => participant.sender_name),
    ["Speed", "Dylan"],
  );
});
