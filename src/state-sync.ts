import type {
  AgentActivity,
  AgentRun,
  AgentSubscriptionStatus,
  AgentWorkItem,
  Artifact,
  Bootstrap,
  Channel,
  ChannelMember,
  Message,
  SavedMessage,
} from "./types";

const ACTIVITY_HISTORY_LIMIT_PER_AGENT = 80;

export type UiBackendEvent =
  | { type: "refresh"; reason?: string }
  | { type: "batch"; events: string[] }
  | { type: "message_upsert"; reason?: string; message: Message }
  | {
      type: "message_delta";
      reason?: string;
      message_id: string;
      append: string;
      body_length?: number;
      delivery_state: Message["delivery_state"];
    }
  | { type: "message_delete"; reason?: string; message_id: string }
  | { type: "activity_upsert"; reason?: string; activity: AgentActivity }
  | {
      type: "agent_run_upsert";
      reason?: string;
      run: Omit<AgentRun, "log"> & { log?: string };
    }
  | {
      type: "agent_subscription_status_upsert";
      reason?: string;
      agent_id: string;
      subscription_status: AgentSubscriptionStatus;
    }
  | {
      type: "work_item_upsert";
      reason?: string;
      work_item: Omit<AgentWorkItem, "context"> & { context?: string };
    }
  | { type: "artifact_upsert"; reason?: string; artifact: Artifact }
  | { type: "channel_member_upsert"; reason?: string; member: ChannelMember }
  | {
      type: "channel_member_remove";
      reason?: string;
      channel_id: string;
      agent_id: string;
    };

export type MessageDelta = {
  append: string;
  bodyLength?: number;
  deliveryState: Message["delivery_state"];
};

export type HydratedMessageDeltaReconciliation =
  | "append"
  | "covered"
  | "retry";

export function reconcileHydratedMessageDelta(
  baselineBody: string,
  delta: MessageDelta,
): HydratedMessageDeltaReconciliation {
  const targetLength = delta.bodyLength;
  if (
    targetLength === undefined
    || !Number.isSafeInteger(targetLength)
    || targetLength < 0
  ) {
    // Retained events written before body_length was added cannot be
    // positioned safely. The freshly loaded row is authoritative.
    return "covered";
  }

  const baselineLength = Array.from(baselineBody).length;
  if (baselineLength >= targetLength) return "covered";

  const appendLength = Array.from(delta.append).length;
  return baselineLength === Math.max(0, targetLength - appendLength)
    ? "append"
    : "retry";
}

export type SavedToggleOverride = {
  saved: boolean;
  entry?: SavedMessage;
};

export type SnapshotOptimisticState = {
  messages: ReadonlyMap<string, Message>;
  channels: ReadonlyMap<string, Channel>;
  removedChannelIds: ReadonlySet<string>;
  savedToggles: ReadonlyMap<string, SavedToggleOverride>;
};

export type SnapshotHydrationState = {
  snapshotInvalidated: boolean;
  loadedHistoricalMessageIds: ReadonlySet<string>;
  paginatedChannelIds: ReadonlySet<string>;
  initializedChannelIds: ReadonlySet<string>;
};

export type SnapshotApplyOptions = {
  includeOptimistic: boolean;
  optimistic: SnapshotOptimisticState;
  hydration: SnapshotHydrationState;
};

export type SnapshotApplyResult = {
  data: Bootstrap;
  acknowledgedOptimisticChannelIds: string[];
  acknowledgedRemovedChannelIds: string[];
  retainedHistoricalMessageIds: string[];
};

export type BackendEventResult = {
  data: Bootstrap | null;
  needsRefresh: boolean;
  deletedMessageIds: string[];
};

export type OptimisticMutation =
  | { type: "channel_add"; channel: Channel }
  | { type: "channel_remove"; channelIds: ReadonlySet<string> }
  | { type: "message_add"; message: Message }
  | { type: "message_remove"; messageId: string }
  | {
      type: "message_replace";
      optimisticMessageId: string;
      persistedMessage: Message;
    }
  | {
      type: "saved_message_toggle";
      messageId: string;
      saved: boolean;
      entry?: SavedMessage;
    }
  | {
      type: "channel_member_set";
      channelId: string;
      agentId: string;
      member?: ChannelMember;
    };

function timestampSortValue(value: string) {
  const parsed = new Date(value).getTime();
  return Number.isFinite(parsed) ? parsed : 0;
}

function activityOwnerKey(activity: AgentActivity) {
  return activity.agent_id ?? `handle:${activity.agent_handle || "unknown"}`;
}

export function limitActivitiesPerAgent(activities: AgentActivity[]) {
  const counts = new Map<string, number>();
  return [...activities]
    .sort(
      (left, right) =>
        timestampSortValue(right.created_at) -
        timestampSortValue(left.created_at),
    )
    .filter((activity) => {
      const key = activityOwnerKey(activity);
      const count = counts.get(key) ?? 0;
      if (count >= ACTIVITY_HISTORY_LIMIT_PER_AGENT) return false;
      counts.set(key, count + 1);
      return true;
    });
}

// Optimistic sends sort after every persisted message; an empty streaming
// placeholder sorts after those too. The backend reserves the placeholder when
// a run starts and only assigns its real seq with the first visible text, so
// until then it must not hold a slot above messages posted in the meantime.
const OPTIMISTIC_SORT_SEQ = Number.MAX_SAFE_INTEGER - 1;
const PENDING_REPLY_SORT_SEQ = Number.MAX_SAFE_INTEGER;

function messageSortSeq(message: Message) {
  if (message.delivery_state === "streaming" && !message.body.trim()) {
    return PENDING_REPLY_SORT_SEQ;
  }
  return Number.isSafeInteger(message.seq) && message.seq > 0
    ? message.seq
    : OPTIMISTIC_SORT_SEQ;
}

export function sortMessages(messages: Message[]) {
  return [...messages].sort((left, right) => {
    const leftSeq = messageSortSeq(left);
    const rightSeq = messageSortSeq(right);
    if (leftSeq !== rightSeq) return leftSeq - rightSeq;
    return (
      new Date(left.created_at).getTime() -
      new Date(right.created_at).getTime()
    );
  });
}

export function mergeMessages(current: Message[], incoming: Message[]) {
  if (incoming.length === 0) return current;
  const messagesById = new Map(
    current.map((message) => [message.id, message]),
  );
  for (const message of incoming) {
    messagesById.set(message.id, message);
  }
  return sortMessages(Array.from(messagesById.values()));
}

export function normalizeSnapshot(next: Bootstrap): Bootstrap {
  return {
    ...next,
    messages: sortMessages(next.messages),
    channel_message_history: next.channel_message_history ?? [],
    agent_activities: limitActivitiesPerAgent(next.agent_activities),
  };
}

export function removeChannelsFromBootstrap(
  next: Bootstrap,
  channelIds: ReadonlySet<string>,
): Bootstrap {
  return {
    ...next,
    channels: next.channels.filter((item) => !channelIds.has(item.id)),
    thread_activities: next.thread_activities.filter(
      (item) => !channelIds.has(item.channel_id),
    ),
    channel_members: next.channel_members.filter(
      (item) => !channelIds.has(item.channel_id),
    ),
    messages: next.messages.filter(
      (item) => !channelIds.has(item.channel_id),
    ),
    channel_message_history: next.channel_message_history.filter(
      (item) => !channelIds.has(item.channel_id),
    ),
    saved_messages: next.saved_messages.filter(
      (item) => !channelIds.has(item.channel_id),
    ),
    artifacts: next.artifacts.filter(
      (item) => !channelIds.has(item.channel_id),
    ),
    tasks: next.tasks.filter((item) => !channelIds.has(item.channel_id)),
    reminders: next.reminders.filter(
      (item) => !item.channel_id || !channelIds.has(item.channel_id),
    ),
    agent_schedules: next.agent_schedules.filter(
      (item) => !channelIds.has(item.channel_id),
    ),
    agent_work_items: next.agent_work_items.filter(
      (item) => !item.channel_id || !channelIds.has(item.channel_id),
    ),
  };
}

export function savedMessagesWithState(
  savedMessages: SavedMessage[],
  messageId: string,
  saved: boolean,
  savedEntry?: SavedMessage,
) {
  const withoutMessage = savedMessages.filter(
    (item) => item.message_id !== messageId,
  );
  if (!saved) {
    return withoutMessage.length === savedMessages.length
      ? savedMessages
      : withoutMessage;
  }
  const existing =
    savedMessages.find((item) => item.message_id === messageId) ?? null;
  if (!savedEntry && existing) return savedMessages;
  if (!savedEntry) return savedMessages;
  return [savedEntry, ...withoutMessage];
}

function applySnapshotOptimism(
  snapshot: Bootstrap,
  optimistic: SnapshotOptimisticState,
  includeOptimistic: boolean,
) {
  let data = snapshot;
  const acknowledgedOptimisticChannelIds: string[] = [];
  const acknowledgedRemovedChannelIds: string[] = [];

  if (includeOptimistic && optimistic.removedChannelIds.size > 0) {
    const snapshotChannelIds = new Set(
      data.channels.map((channel) => channel.id),
    );
    const pendingRemovedChannelIds = new Set<string>();
    for (const channelId of optimistic.removedChannelIds) {
      if (snapshotChannelIds.has(channelId)) {
        pendingRemovedChannelIds.add(channelId);
      } else {
        acknowledgedRemovedChannelIds.push(channelId);
      }
    }
    if (pendingRemovedChannelIds.size > 0) {
      data = removeChannelsFromBootstrap(data, pendingRemovedChannelIds);
    }
  }

  if (includeOptimistic && optimistic.channels.size > 0) {
    const existingIds = new Set(data.channels.map((channel) => channel.id));
    const pending: Channel[] = [];
    for (const [channelId, channel] of optimistic.channels) {
      if (existingIds.has(channelId)) {
        acknowledgedOptimisticChannelIds.push(channelId);
      } else {
        pending.push(channel);
      }
    }
    if (pending.length > 0) {
      data = { ...data, channels: [...data.channels, ...pending] };
    }
  }

  if (includeOptimistic && optimistic.messages.size > 0) {
    const existingIds = new Set(data.messages.map((message) => message.id));
    const pending = Array.from(optimistic.messages.values()).filter(
      (message) => !existingIds.has(message.id),
    );
    if (pending.length > 0) {
      data = {
        ...data,
        messages: sortMessages([...data.messages, ...pending]),
      };
    }
  }

  if (optimistic.savedToggles.size > 0) {
    let savedMessages = data.saved_messages;
    for (const [messageId, override] of optimistic.savedToggles) {
      savedMessages = savedMessagesWithState(
        savedMessages,
        messageId,
        override.saved,
        override.entry,
      );
    }
    if (savedMessages !== data.saved_messages) {
      data = { ...data, saved_messages: savedMessages };
    }
  }

  return {
    data,
    acknowledgedOptimisticChannelIds,
    acknowledgedRemovedChannelIds,
  };
}

export function reconcileHydration(
  current: Bootstrap | null,
  snapshot: Bootstrap,
  hydration: SnapshotHydrationState,
) {
  if (!current) {
    return { data: snapshot, retainedHistoricalMessageIds: [] };
  }

  const snapshotMessageIds = new Set(
    snapshot.messages.map((message) => message.id),
  );
  const snapshotChannelIds = new Set(
    snapshot.channels.map((channel) => channel.id),
  );
  let currentOnlyMessages = current.messages.filter(
    (message) => !snapshotMessageIds.has(message.id),
  );
  const retainedHistoricalMessageIds: string[] = [];

  if (!hydration.snapshotInvalidated) {
    currentOnlyMessages = currentOnlyMessages.filter(
      (message) =>
        snapshotChannelIds.has(message.channel_id) &&
        (hydration.loadedHistoricalMessageIds.has(message.id) ||
          hydration.paginatedChannelIds.has(message.channel_id) ||
          hydration.initializedChannelIds.has(message.channel_id)),
    );
    retainedHistoricalMessageIds.push(
      ...currentOnlyMessages.map((message) => message.id),
    );
  } else {
    currentOnlyMessages = currentOnlyMessages.filter((message) =>
      snapshotChannelIds.has(message.channel_id),
    );
  }

  const currentMessagesById = new Map(
    current.messages.map((message) => [message.id, message]),
  );
  let preservedStreaming = false;
  const snapshotMessages = snapshot.messages.map((message) => {
    if (message.delivery_state !== "streaming") return message;
    const local = currentMessagesById.get(message.id);
    if (!local) return message;
    if (
      local.delivery_state !== "streaming" ||
      local.body.length > message.body.length
    ) {
      preservedStreaming = true;
      return local;
    }
    return message;
  });

  if (currentOnlyMessages.length === 0 && !preservedStreaming) {
    return { data: snapshot, retainedHistoricalMessageIds };
  }

  return {
    data: {
      ...snapshot,
      messages: sortMessages([
        ...snapshotMessages,
        ...currentOnlyMessages,
      ]),
    },
    retainedHistoricalMessageIds,
  };
}

export function applySnapshot(
  current: Bootstrap | null,
  snapshot: Bootstrap,
  options: SnapshotApplyOptions,
): SnapshotApplyResult {
  const normalized = normalizeSnapshot(snapshot);
  const optimistic = applySnapshotOptimism(
    normalized,
    options.optimistic,
    options.includeOptimistic,
  );
  const hydrated = reconcileHydration(
    current,
    optimistic.data,
    options.hydration,
  );
  return {
    data: hydrated.data,
    acknowledgedOptimisticChannelIds:
      optimistic.acknowledgedOptimisticChannelIds,
    acknowledgedRemovedChannelIds:
      optimistic.acknowledgedRemovedChannelIds,
    retainedHistoricalMessageIds:
      hydrated.retainedHistoricalMessageIds,
  };
}

export function resolveActiveChannelId(
  channels: Channel[],
  currentChannelId: string,
  preferredChannelId?: string,
) {
  if (
    preferredChannelId &&
    channels.some((channel) => channel.id === preferredChannelId)
  ) {
    return preferredChannelId;
  }
  if (channels.some((channel) => channel.id === currentChannelId)) {
    return currentChannelId;
  }
  return channels[0]?.id ?? "";
}

export type ThreadHydrationResult =
  | { status: "pending" }
  | { status: "ready"; threadId: string | null };

export function reconcileThreadHydration(options: {
  messages: Message[];
  channelId: string;
  hydrated: boolean;
  hasRememberedThread: boolean;
  rememberedThreadId: string | null;
}): ThreadHydrationResult {
  if (!options.hydrated) return { status: "pending" };

  const repliedRootIds = new Set(
    options.messages
      .filter(
        (message) =>
          message.channel_id === options.channelId &&
          Boolean(message.thread_root_id),
      )
      .map((message) => message.thread_root_id),
  );
  const defaultThreadId =
    options.messages.find(
      (message) =>
        message.channel_id === options.channelId &&
        !message.thread_root_id &&
        repliedRootIds.has(message.id),
    )?.id ?? null;

  if (!options.hasRememberedThread) {
    return { status: "ready", threadId: defaultThreadId };
  }
  if (!options.rememberedThreadId) {
    return { status: "ready", threadId: null };
  }
  const rememberedThreadExists = options.messages.some(
    (message) =>
      message.id === options.rememberedThreadId &&
      message.channel_id === options.channelId &&
      !message.thread_root_id,
  );
  return {
    status: "ready",
    threadId: rememberedThreadExists
      ? options.rememberedThreadId
      : defaultThreadId,
  };
}

function transition(
  data: Bootstrap | null,
  needsRefresh = false,
  deletedMessageIds: string[] = [],
): BackendEventResult {
  return { data, needsRefresh, deletedMessageIds };
}

export function applyMessageDeltas(
  current: Bootstrap | null,
  deltas: ReadonlyMap<string, MessageDelta>,
): BackendEventResult {
  if (!current) return transition(current, true);
  let missing = false;
  let changed = false;
  const seen = new Set<string>();
  const messages = current.messages.map((message) => {
    const delta = deltas.get(message.id);
    if (!delta) return message;
    seen.add(message.id);
    if (delta.bodyLength !== undefined) {
      const reconciliation = reconcileHydratedMessageDelta(message.body, delta);
      if (reconciliation === "covered") return message;
      if (reconciliation === "retry") {
        missing = true;
        return message;
      }
    }
    changed = true;
    return {
      ...message,
      body: `${message.body}${delta.append}`,
      delivery_state: delta.deliveryState,
    };
  });
  for (const messageId of deltas.keys()) {
    if (!seen.has(messageId)) {
      missing = true;
      break;
    }
  }
  return transition(
    changed ? { ...current, messages } : current,
    missing,
  );
}

export function applyBackendEvent(
  current: Bootstrap | null,
  event: UiBackendEvent,
): BackendEventResult {
  if (event.type === "refresh" || event.type === "batch") {
    return transition(current, true);
  }
  if (!current) return transition(current, true);

  if (event.type === "message_upsert") {
    const existingIndex = current.messages.findIndex(
      (item) => item.id === event.message.id,
    );
    const messages =
      existingIndex >= 0
        ? current.messages.map((item) =>
            item.id === event.message.id ? event.message : item,
          )
        : [...current.messages, event.message];
    return transition({
      ...current,
      messages: sortMessages(messages),
    });
  }

  if (event.type === "message_delta") {
    return applyMessageDeltas(
      current,
      new Map([
        [
          event.message_id,
          {
            append: event.append,
            bodyLength: event.body_length,
            deliveryState: event.delivery_state,
          },
        ],
      ]),
    );
  }

  if (event.type === "message_delete") {
    const deletedMessageIds = current.messages
      .filter(
        (message) =>
          message.id === event.message_id ||
          message.thread_root_id === event.message_id,
      )
      .map((message) => message.id);
    return transition(
      {
        ...current,
        messages: current.messages.filter(
          (message) =>
            message.id !== event.message_id &&
            message.thread_root_id !== event.message_id,
        ),
      },
      false,
      deletedMessageIds,
    );
  }

  if (event.type === "activity_upsert") {
    const existingIndex = current.agent_activities.findIndex(
      (item) => item.id === event.activity.id,
    );
    const activities =
      existingIndex >= 0
        ? current.agent_activities.map((item) =>
            item.id === event.activity.id ? event.activity : item,
          )
        : [event.activity, ...current.agent_activities];
    return transition({
      ...current,
      agent_activities: limitActivitiesPerAgent(activities),
    });
  }

  if (event.type === "agent_run_upsert") {
    const existing = current.agent_runs.find(
      (item) => item.id === event.run.id,
    );
    const run: AgentRun = {
      ...event.run,
      log: event.run.log ?? existing?.log ?? "",
    };
    const agentRuns = existing
      ? current.agent_runs.map((item) =>
          item.id === event.run.id ? { ...item, ...run } : item,
        )
      : [run, ...current.agent_runs];
    agentRuns.sort(
      (left, right) =>
        new Date(right.started_at).getTime() -
        new Date(left.started_at).getTime(),
    );
    return transition({
      ...current,
      agent_runs: agentRuns.slice(0, 30),
    });
  }

  if (event.type === "agent_subscription_status_upsert") {
    const existingIndex = current.agents.findIndex(
      (agent) => agent.id === event.agent_id,
    );
    if (existingIndex < 0) return transition(current, true);
    return transition({
      ...current,
      agents: current.agents.map((agent, index) =>
        index === existingIndex
          ? { ...agent, subscription_status: event.subscription_status }
          : agent,
      ),
    });
  }

  if (event.type === "work_item_upsert") {
    const existing = current.agent_work_items.find(
      (item) => item.id === event.work_item.id,
    );
    const workItem: AgentWorkItem = {
      ...event.work_item,
      context: event.work_item.context ?? existing?.context ?? "",
      source_kind:
        event.work_item.source_kind ?? existing?.source_kind ?? "manual",
    };
    const workItems = existing
      ? current.agent_work_items.map((item) =>
          item.id === event.work_item.id
            ? { ...item, ...workItem }
            : item,
        )
      : [workItem, ...current.agent_work_items];
    workItems.sort(
      (left, right) =>
        new Date(right.created_at).getTime() -
        new Date(left.created_at).getTime(),
    );
    return transition({
      ...current,
      agent_work_items: workItems.slice(0, 80),
    });
  }

  if (event.type === "artifact_upsert") {
    const artifact = event.artifact;
    if (
      !artifact ||
      typeof artifact.id !== "string" ||
      typeof artifact.message_id !== "string"
    ) {
      return transition(current, true);
    }
    const currentArtifacts = Array.isArray(current.artifacts)
      ? current.artifacts
      : [];
    const existingIndex = currentArtifacts.findIndex(
      (item) => item.id === artifact.id,
    );
    const artifacts =
      existingIndex >= 0
        ? currentArtifacts.map((item) =>
            item.id === artifact.id ? artifact : item,
          )
        : [...currentArtifacts, artifact];
    const messages = current.messages.map((message) => {
      if (message.id !== artifact.message_id) return message;
      const currentMessageArtifacts = Array.isArray(message.artifacts)
        ? message.artifacts
        : [];
      const existingArtifactIndex = currentMessageArtifacts.findIndex(
        (item) => item.id === artifact.id,
      );
      const messageArtifacts =
        existingArtifactIndex >= 0
          ? currentMessageArtifacts.map((item) =>
              item.id === artifact.id ? artifact : item,
            )
          : [...currentMessageArtifacts, artifact];
      return { ...message, artifacts: messageArtifacts };
    });
    return transition({ ...current, artifacts, messages });
  }

  if (event.type === "channel_member_upsert") {
    const member = event.member;
    if (
      !member ||
      typeof member.channel_id !== "string" ||
      typeof member.agent_id !== "string"
    ) {
      return transition(current, true);
    }
    const filtered = current.channel_members.filter(
      (entry) =>
        !(
          entry.channel_id === member.channel_id &&
          entry.agent_id === member.agent_id
        ),
    );
    return transition({
      ...current,
      channel_members: [...filtered, member],
    });
  }

  if (event.type === "channel_member_remove") {
    if (
      typeof event.channel_id !== "string" ||
      typeof event.agent_id !== "string"
    ) {
      return transition(current, true);
    }
    const filtered = current.channel_members.filter(
      (entry) =>
        !(
          entry.channel_id === event.channel_id &&
          entry.agent_id === event.agent_id
        ),
    );
    return transition(
      filtered.length === current.channel_members.length
        ? current
        : { ...current, channel_members: filtered },
    );
  }

  return transition(current, true);
}

export function applyBackendEvents(
  current: Bootstrap | null,
  events: UiBackendEvent[],
): BackendEventResult {
  let result = transition(current);
  for (const event of events) {
    const next = applyBackendEvent(result.data, event);
    result = {
      data: next.data,
      needsRefresh: result.needsRefresh || next.needsRefresh,
      deletedMessageIds: [
        ...result.deletedMessageIds,
        ...next.deletedMessageIds,
      ],
    };
  }
  return result;
}

export function parseBackendEventPayload(payload: unknown): UiBackendEvent[] {
  if (typeof payload !== "string") {
    throw new TypeError("Backend event payload must be a JSON string");
  }
  const parsed = JSON.parse(payload) as unknown;
  if (!parsed || typeof parsed !== "object") {
    throw new TypeError("Backend event payload must be an object");
  }
  if ((parsed as { type?: unknown }).type === "batch") {
    const events = (parsed as { events?: unknown }).events;
    if (
      !Array.isArray(events) ||
      events.some((event) => typeof event !== "string")
    ) {
      throw new TypeError("Backend event batch must contain JSON strings");
    }
    return events.flatMap((event) => parseBackendEventPayload(event));
  }
  return [parsed as UiBackendEvent];
}

export function applyOptimisticMutation(
  current: Bootstrap | null,
  mutation: OptimisticMutation,
): Bootstrap | null {
  if (!current) return current;

  if (mutation.type === "channel_add") {
    if (
      current.channels.some(
        (channel) => channel.id === mutation.channel.id,
      )
    ) {
      return current;
    }
    return {
      ...current,
      channels: [...current.channels, mutation.channel],
    };
  }

  if (mutation.type === "channel_remove") {
    return removeChannelsFromBootstrap(current, mutation.channelIds);
  }

  if (mutation.type === "message_add") {
    return {
      ...current,
      messages: [...current.messages, mutation.message],
    };
  }

  if (mutation.type === "message_remove") {
    return {
      ...current,
      messages: current.messages.filter(
        (message) => message.id !== mutation.messageId,
      ),
    };
  }

  if (mutation.type === "message_replace") {
    const messages = current.messages
      .filter(
        (message) =>
          message.id !== mutation.optimisticMessageId &&
          message.id !== mutation.persistedMessage.id,
      )
      .concat(mutation.persistedMessage);
    return { ...current, messages: sortMessages(messages) };
  }

  if (mutation.type === "saved_message_toggle") {
    const savedMessages = savedMessagesWithState(
      current.saved_messages,
      mutation.messageId,
      mutation.saved,
      mutation.entry,
    );
    return savedMessages === current.saved_messages
      ? current
      : { ...current, saved_messages: savedMessages };
  }

  const without = current.channel_members.filter(
    (entry) =>
      !(
        entry.channel_id === mutation.channelId &&
        entry.agent_id === mutation.agentId
      ),
  );
  if (!mutation.member) {
    return without.length === current.channel_members.length
      ? current
      : { ...current, channel_members: without };
  }
  return {
    ...current,
    channel_members: [...without, mutation.member],
  };
}
