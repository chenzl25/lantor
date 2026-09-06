import assert from "node:assert/strict";
import test from "node:test";

import {
  applyBackendEvent,
  applyBackendEvents,
  applyOptimisticMutation,
  applySnapshot,
  parseBackendEventPayload,
  reconcileHydratedMessageDelta,
  reconcileThreadHydration,
  type SnapshotApplyOptions,
} from "../src/state-sync";
import type {
  Agent,
  Bootstrap,
  Channel,
  ChannelMember,
  Message,
  SavedMessage,
} from "../src/types";

const NOW = "2026-07-26T12:00:00.000Z";

function channel(id: string, overrides: Partial<Channel> = {}): Channel {
  return {
    id,
    name: id,
    description: "",
    kind: "channel",
    dm_agent_id: null,
    unread_count: 0,
    github_unread_count: 0,
    github_review_synced_at: null,
    ...overrides,
  };
}

function message(
  id: string,
  channelId: string,
  overrides: Partial<Message> = {},
): Message {
  return {
    id,
    seq: 1,
    channel_id: channelId,
    thread_root_id: null,
    sender_agent_id: null,
    sender_name: "Dylan",
    sender_role: "owner",
    body: id,
    is_task: false,
    thread_followed: false,
    delivery_state: "complete",
    stream_key: "",
    task_number: null,
    task_status: null,
    attachments: [],
    artifacts: [],
    created_at: NOW,
    updated_at: NOW,
    ...overrides,
  };
}

function agent(id: string, overrides: Partial<Agent> = {}): Agent {
  return {
    id,
    handle: id,
    display_name: id,
    role: "agent",
    status: "idle",
    runtime: "codex",
    model: "gpt-5.6-sol",
    reasoning_effort: "medium",
    service_tier: "",
    avatar: "",
    description: "",
    launch_command: "",
    environment_variables: "",
    working_directory: "",
    workspace_exists: false,
    workspace_memory_path: "",
    workspace_memory_exists: false,
    workspace_entries: [],
    daily_budget_micros: 0,
    subscription_status: null,
    ...overrides,
  };
}

function bootstrap(overrides: Partial<Bootstrap> = {}): Bootstrap {
  return {
    db_url: "sqlite://test",
    web_base_url: null,
    owner_profile: {
      display_name: "Dylan",
      avatar: "",
      description: "",
    },
    channels: [],
    thread_activities: [],
    channel_members: [],
    agents: [],
    messages: [],
    channel_message_history: [],
    saved_messages: [],
    dismissed_inbox_items: {},
    read_inbox_items: {},
    artifacts: [],
    tasks: [],
    reminders: [],
    agent_schedules: [],
    agent_runs: [],
    agent_work_items: [],
    agent_activities: [],
    supervisor: {
      pid: null,
      status: "stopped",
      updated_at: null,
    },
    launch_agent: {
      label: "com.lantor.test",
      plist_path: "",
      installed: false,
      loaded: false,
    },
    ui_event_cursor: 0,
    ...overrides,
  };
}

function snapshotOptions(
  overrides: {
    messages?: ReadonlyMap<string, Message>;
    channels?: ReadonlyMap<string, Channel>;
    removedChannelIds?: ReadonlySet<string>;
    savedToggles?: ReadonlyMap<
      string,
      { saved: boolean; entry?: SavedMessage }
    >;
    snapshotInvalidated?: boolean;
    loadedHistoricalMessageIds?: ReadonlySet<string>;
    paginatedChannelIds?: ReadonlySet<string>;
    initializedChannelIds?: ReadonlySet<string>;
  } = {},
): SnapshotApplyOptions {
  return {
    includeOptimistic: true,
    optimistic: {
      messages: overrides.messages ?? new Map(),
      channels: overrides.channels ?? new Map(),
      removedChannelIds: overrides.removedChannelIds ?? new Set(),
      savedToggles: overrides.savedToggles ?? new Map(),
    },
    hydration: {
      snapshotInvalidated: overrides.snapshotInvalidated ?? false,
      loadedHistoricalMessageIds:
        overrides.loadedHistoricalMessageIds ?? new Set(),
      paginatedChannelIds: overrides.paginatedChannelIds ?? new Set(),
      initializedChannelIds: overrides.initializedChannelIds ?? new Set(),
    },
  };
}

test("a delta applied after a request started wins over its stale streaming snapshot", () => {
  const room = channel("room");
  const initialMessage = message("stream", room.id, {
    body: "a",
    delivery_state: "streaming",
  });
  const staleSnapshot = bootstrap({
    channels: [room],
    messages: [initialMessage],
  });
  const deltaResult = applyBackendEvent(staleSnapshot, {
    type: "message_delta",
    message_id: initialMessage.id,
    append: "b",
    delivery_state: "streaming",
  });

  assert.equal(deltaResult.data?.messages[0]?.body, "ab");

  const result = applySnapshot(
    deltaResult.data,
    staleSnapshot,
    snapshotOptions({ snapshotInvalidated: true }),
  );

  assert.equal(result.data.messages[0]?.body, "ab");
  assert.equal(result.data.messages[0]?.delivery_state, "streaming");
});

test("an optimistic channel tombstone prevents stale snapshot resurrection until acknowledged", () => {
  const doomed = channel("doomed");
  const doomedMessage = message("message-1", doomed.id);
  const staleSnapshot = bootstrap({
    channels: [doomed],
    messages: [doomedMessage],
    channel_message_history: [
      { channel_id: doomed.id, before_seq: 1, has_more: false },
    ],
  });
  const options = snapshotOptions({
    removedChannelIds: new Set([doomed.id]),
    snapshotInvalidated: true,
  });

  const staleResult = applySnapshot(staleSnapshot, staleSnapshot, options);

  assert.deepEqual(staleResult.data.channels, []);
  assert.deepEqual(staleResult.data.messages, []);
  assert.deepEqual(staleResult.data.channel_message_history, []);
  assert.deepEqual(staleResult.acknowledgedRemovedChannelIds, []);

  const authoritativeResult = applySnapshot(
    staleResult.data,
    bootstrap(),
    options,
  );

  assert.deepEqual(authoritativeResult.data.channels, []);
  assert.deepEqual(authoritativeResult.acknowledgedRemovedChannelIds, [
    doomed.id,
  ]);
});

test("an optimistic channel remains selectable until a snapshot acknowledges it", () => {
  const existing = channel("existing");
  const created = channel("created");
  const pendingOptions = snapshotOptions({
    channels: new Map([[created.id, created]]),
  });

  const pending = applySnapshot(
    bootstrap({ channels: [existing] }),
    bootstrap({ channels: [existing] }),
    pendingOptions,
  );

  assert.deepEqual(
    pending.data.channels.map((item) => item.id),
    [existing.id, created.id],
  );
  assert.deepEqual(pending.acknowledgedOptimisticChannelIds, []);

  const acknowledged = applySnapshot(
    pending.data,
    bootstrap({ channels: [existing, created] }),
    pendingOptions,
  );

  assert.deepEqual(acknowledged.acknowledgedOptimisticChannelIds, [
    created.id,
  ]);
});

test("current-only history is retained only after channel hydration is known", () => {
  const room = channel("room");
  const recent = message("recent", room.id, { seq: 20 });
  const historical = message("historical", room.id, { seq: 2 });
  const current = bootstrap({
    channels: [room],
    messages: [historical, recent],
  });
  const compactSnapshot = bootstrap({
    channels: [room],
    messages: [recent],
  });

  const beforeHydration = applySnapshot(
    current,
    compactSnapshot,
    snapshotOptions(),
  );
  assert.deepEqual(
    beforeHydration.data.messages.map((item) => item.id),
    [recent.id],
  );

  const afterHydration = applySnapshot(
    current,
    compactSnapshot,
    snapshotOptions({
      initializedChannelIds: new Set([room.id]),
    }),
  );
  assert.deepEqual(
    afterHydration.data.messages.map((item) => item.id),
    [historical.id, recent.id],
  );
  assert.deepEqual(afterHydration.retainedHistoricalMessageIds, [
    historical.id,
  ]);
});

test("thread restoration stays pending until hydration can validate the remembered root", () => {
  const room = channel("room");
  const remembered = message("remembered", room.id);
  const fallback = message("fallback", room.id, { seq: 2 });
  const reply = message("reply", room.id, {
    seq: 3,
    thread_root_id: fallback.id,
  });
  const partialMessages = [fallback, reply];

  const pending = reconcileThreadHydration({
    messages: partialMessages,
    channelId: room.id,
    hydrated: false,
    hasRememberedThread: true,
    rememberedThreadId: remembered.id,
  });
  assert.deepEqual(pending, { status: "pending" });

  const rememberedAfterHydration = reconcileThreadHydration({
    messages: [remembered, ...partialMessages],
    channelId: room.id,
    hydrated: true,
    hasRememberedThread: true,
    rememberedThreadId: remembered.id,
  });
  assert.deepEqual(rememberedAfterHydration, {
    status: "ready",
    threadId: remembered.id,
  });

  const fallbackAfterHydration = reconcileThreadHydration({
    messages: partialMessages,
    channelId: room.id,
    hydrated: true,
    hasRememberedThread: true,
    rememberedThreadId: remembered.id,
  });
  assert.deepEqual(fallbackAfterHydration, {
    status: "ready",
    threadId: fallback.id,
  });
});

test("an invalidated snapshot retains targeted messages for surviving channels", () => {
  const room = channel("room");
  const snapshotMessage = message("snapshot", room.id, { seq: 1 });
  const targetedMessage = message("targeted", room.id, { seq: 2 });
  const current = bootstrap({
    channels: [room],
    messages: [snapshotMessage, targetedMessage],
  });
  const staleSnapshot = bootstrap({
    channels: [room],
    messages: [snapshotMessage],
  });

  const result = applySnapshot(
    current,
    staleSnapshot,
    snapshotOptions({ snapshotInvalidated: true }),
  );

  assert.deepEqual(
    result.data.messages.map((item) => item.id),
    [snapshotMessage.id, targetedMessage.id],
  );
});

test("message upsert, delta, and root delete form a deterministic event sequence", () => {
  const room = channel("room");
  const root = message("root", room.id, {
    body: "hel",
    delivery_state: "streaming",
  });
  const reply = message("reply", room.id, {
    seq: 2,
    thread_root_id: root.id,
  });
  const initial = bootstrap({ channels: [room] });

  const result = applyBackendEvents(initial, [
    { type: "message_upsert", message: root },
    {
      type: "message_delta",
      message_id: root.id,
      append: "lo",
      delivery_state: "complete",
    },
    { type: "message_upsert", message: reply },
    { type: "message_delete", message_id: root.id },
  ]);

  assert.deepEqual(result.data?.messages, []);
  assert.deepEqual(result.deletedMessageIds, [root.id, reply.id]);
  assert.equal(result.needsRefresh, false);
});

test("subscription status upsert updates only its agent without a bootstrap", () => {
  const codexAgent = agent("codex-agent");
  const otherAgent = agent("other-agent");
  const subscriptionStatus = {
    provider: "codex",
    plan: "pro",
    status: "available",
    windows: [
      {
        id: "codex:primary",
        label: "5-hour",
        used_percent: 24,
        resets_at: 1_788_348_600,
      },
    ],
    observed_at: NOW,
  };

  const result = applyBackendEvent(
    bootstrap({ agents: [codexAgent, otherAgent] }),
    {
      type: "agent_subscription_status_upsert",
      agent_id: codexAgent.id,
      subscription_status: subscriptionStatus,
    },
  );

  assert.equal(result.needsRefresh, false);
  assert.deepEqual(result.data?.agents[0].subscription_status, subscriptionStatus);
  assert.strictEqual(result.data?.agents[1], otherAgent);
});

test("a delta for an unknown message requests authoritative refresh", () => {
  const result = applyBackendEvent(bootstrap(), {
    type: "message_delta",
    message_id: "missing",
    append: "body",
    delivery_state: "streaming",
  });

  assert.equal(result.data?.messages.length, 0);
  assert.equal(result.needsRefresh, true);
});

test("a cursor-replayed delta already covered by the snapshot is idempotent", () => {
  const room = channel("room");
  const complete = message("complete", room.id, {
    body: "hello",
    delivery_state: "complete",
  });
  const result = applyBackendEvent(
    bootstrap({ channels: [room], messages: [complete] }),
    {
      type: "message_delta",
      message_id: complete.id,
      append: "lo",
      body_length: 5,
      delivery_state: "streaming",
    },
  );

  assert.equal(result.data?.messages[0].body, "hello");
  assert.equal(result.data?.messages[0].delivery_state, "complete");
  assert.equal(result.needsRefresh, false);
});

test("a cursor-replayed delta with a mismatched snapshot requests reconciliation", () => {
  const room = channel("room");
  const partial = message("partial", room.id, {
    body: "hell",
    delivery_state: "streaming",
  });
  const result = applyBackendEvent(
    bootstrap({ channels: [room], messages: [partial] }),
    {
      type: "message_delta",
      message_id: partial.id,
      append: "lo",
      body_length: 5,
      delivery_state: "streaming",
    },
  );

  assert.equal(result.data?.messages[0].body, "hell");
  assert.equal(result.needsRefresh, true);
});

test("hydrated message deltas append only from an exact baseline", () => {
  assert.equal(
    reconcileHydratedMessageDelta("hel", {
      append: "lo",
      bodyLength: 5,
      deliveryState: "streaming",
    }),
    "append",
  );
  assert.equal(
    reconcileHydratedMessageDelta("hello", {
      append: "lo",
      bodyLength: 5,
      deliveryState: "streaming",
    }),
    "covered",
  );
  assert.equal(
    reconcileHydratedMessageDelta("hell", {
      append: "lo",
      bodyLength: 5,
      deliveryState: "streaming",
    }),
    "retry",
  );
});

test("hydrated message delta lengths use Unicode code points and tolerate legacy events", () => {
  assert.equal(
    reconcileHydratedMessageDelta("🙂", {
      append: "好",
      bodyLength: 2,
      deliveryState: "streaming",
    }),
    "append",
  );
  assert.equal(
    reconcileHydratedMessageDelta("authoritative", {
      append: "stale",
      deliveryState: "streaming",
    }),
    "covered",
  );
});

test("backend batches are parsed and flattened without coupling parsing to React", () => {
  const first = JSON.stringify({
    type: "message_delete",
    message_id: "one",
  });
  const nested = JSON.stringify({
    type: "batch",
    events: [
      JSON.stringify({
        type: "channel_member_remove",
        channel_id: "channel",
        agent_id: "agent",
      }),
    ],
  });

  const events = parseBackendEventPayload(
    JSON.stringify({ type: "batch", events: [first, nested] }),
  );

  assert.deepEqual(
    events.map((event) => event.type),
    ["message_delete", "channel_member_remove"],
  );
});

test("optimistic saved and membership mutations update only their owned slice", () => {
  const room = channel("room");
  const savedEntry: SavedMessage = {
    id: "saved",
    message_id: "message",
    channel_id: room.id,
    channel_name: room.name,
    thread_root_id: null,
    sender_name: "Dylan",
    sender_role: "owner",
    body: "body",
    message_created_at: NOW,
    created_at: NOW,
  };
  const member: ChannelMember = {
    channel_id: room.id,
    agent_id: "agent",
    agent_handle: "agent",
    agent_display_name: "Agent",
    created_at: NOW,
  };
  const initial = bootstrap({ channels: [room] });

  const saved = applyOptimisticMutation(initial, {
    type: "saved_message_toggle",
    messageId: savedEntry.message_id,
    saved: true,
    entry: savedEntry,
  });
  const withMember = applyOptimisticMutation(saved, {
    type: "channel_member_set",
    channelId: room.id,
    agentId: member.agent_id,
    member,
  });
  const withoutMember = applyOptimisticMutation(withMember, {
    type: "channel_member_set",
    channelId: room.id,
    agentId: member.agent_id,
  });

  assert.deepEqual(saved?.saved_messages, [savedEntry]);
  assert.deepEqual(withMember?.channel_members, [member]);
  assert.deepEqual(withoutMember?.channel_members, []);
  assert.strictEqual(withoutMember?.saved_messages[0], savedEntry);
});

test("scoped state patches preserve history, project task status and remove deleted channels", async () => {
  const { applyUiStatePatch, scopesForRefresh } = await import("../src/ui-state-sync");
  const current = bootstrap({
    channels: [channel("keep"), channel("deleted")],
    messages: [message("root", "keep", { is_task: true, task_number: 1, task_status: "todo" }), message("history", "keep"), message("removed", "deleted")],
  });
  const updated = applyUiStatePatch(current, {
    channels: [channel("keep", { unread_count: 2 })],
    tasks: [{ id: "task", message_id: "root", channel_id: "keep", number: 1, status: "done" } as Bootstrap["tasks"][number]],
  }, { messages: new Map(), channels: new Map([["optimistic", channel("optimistic")]]), removedChannelIds: new Set(), savedToggles: new Map() });
  assert.deepEqual(updated.channels.map((value) => value.id), ["keep", "optimistic"]);
  assert.deepEqual(updated.messages.map((value) => value.id), ["root", "history"]);
  assert.equal(updated.messages[0].task_status, "done");
  assert.equal(updated.messages[1], current.messages[1], "loaded history keeps object identity");
  assert.equal(updated.agent_activities, current.agent_activities);
  assert.deepEqual(scopesForRefresh("channel_read"), ["channels"]);
  assert.ok(scopesForRefresh("task_status_updated")?.includes("tasks"));
  assert.equal(scopesForRefresh("event_replay_gap"), null);
  assert.equal(scopesForRefresh("future_unknown_event"), null);
});
