export type AgentWorkspaceEntry = {
  name: string;
  path: string;
  relative_path: string;
  kind: "dir" | "file" | "other" | string;
  size_bytes: number | null;
};

export type AgentWorkspaceListing = {
  path: string;
  entries: AgentWorkspaceEntry[];
};

export type AgentWorkspaceFile = {
  name: string;
  path: string;
  relative_path: string;
  size_bytes: number;
  language: string;
  content: string;
  truncated: boolean;
};

export type Agent = {
  id: string;
  handle: string;
  display_name: string;
  role: string;
  status: string;
  runtime: string;
  model: string;
  reasoning_effort: string;
  service_tier: string;
  avatar: string;
  description: string;
  launch_command: string;
  working_directory: string;
  workspace_exists: boolean;
  workspace_memory_path: string;
  workspace_memory_exists: boolean;
  workspace_entries: AgentWorkspaceEntry[];
  daily_budget_micros: number;
};

export type OwnerProfile = {
  display_name: string;
  avatar: string;
  description: string;
};

export type Channel = {
  id: string;
  name: string;
  description: string;
  kind: "channel" | "dm";
  dm_agent_id: string | null;
  unread_count: number;
};

export type ThreadActivity = {
  thread_root_id: string;
  channel_id: string;
  unread_count: number;
  latest_message_id: string;
  latest_activity_at: string;
};

export type ChannelMember = {
  channel_id: string;
  agent_id: string;
  agent_handle: string;
  agent_display_name: string;
  created_at: string;
};

export type Message = {
  id: string;
  channel_id: string;
  thread_root_id: string | null;
  sender_agent_id: string | null;
  sender_name: string;
  sender_role: string;
  body: string;
  is_task: boolean;
  thread_followed: boolean;
  delivery_state: "complete" | "streaming" | "error" | string;
  stream_key: string;
  task_number: number | null;
  task_status: string | null;
  attachments: MessageAttachment[];
  artifacts: Artifact[];
  created_at: string;
  updated_at: string;
};

export type ThreadReplySummary = {
  count: number;
  latest: Message | null;
  participants: Message[];
};

export type SavedMessage = {
  id: string;
  message_id: string;
  channel_id: string;
  channel_name: string;
  thread_root_id: string | null;
  sender_name: string;
  sender_role: string;
  body: string;
  message_created_at: string;
  created_at: string;
};

export type MessageAttachment = {
  id: string;
  message_id: string;
  original_name: string;
  mime_type: string;
  size_bytes: number;
  storage_path: string;
  local_url?: string;
  created_at: string;
};

export type DraftAttachment = {
  id: string;
  file: File;
  original_name: string;
  mime_type: string;
  size_bytes: number;
};

export type Artifact = {
  id: string;
  message_id: string;
  channel_id: string;
  thread_root_id: string | null;
  creator_agent_id: string | null;
  creator_agent_handle: string | null;
  kind: string;
  title: string;
  summary: string;
  content: string;
  metadata: Record<string, unknown>;
  created_at: string;
  updated_at: string;
};

export type Task = {
  id: string;
  number: number;
  message_id: string;
  channel_id: string;
  title: string;
  status: string;
  version: number;
  channel_name: string;
  assignee_id: string | null;
  assignee_name: string | null;
  created_at: string;
  updated_at: string;
};

export type Reminder = {
  id: string;
  channel_id: string | null;
  channel_name: string | null;
  creator_agent_id: string | null;
  creator_agent_handle: string | null;
  thread_root_id: string | null;
  message_id: string | null;
  title: string;
  note: string;
  status: string;
  recurrence: "none" | "daily" | "weekly" | string;
  due_at: string;
  fired_at: string | null;
  completed_at: string | null;
  created_at: string;
  updated_at: string;
};

export type AgentSchedule = {
  id: string;
  agent_id: string;
  agent_handle: string;
  channel_id: string;
  channel_name: string;
  channel_kind: "channel" | "dm" | string;
  thread_root_id: string | null;
  title: string;
  prompt: string;
  cadence: "hourly" | "daily" | "weekly" | string;
  status: string;
  next_run_at: string;
  last_run_at: string | null;
  last_work_item_id: string | null;
  created_at: string;
  updated_at: string;
};

export type AgentRun = {
  id: string;
  agent_id: string;
  agent_handle: string;
  work_item_id: string | null;
  command: string;
  working_directory: string;
  status: string;
  pid: number | null;
  exit_code: number | null;
  log: string;
  input_tokens: number;
  output_tokens: number;
  cost_micros: number;
  started_at: string;
  stopped_at: string | null;
};

export type AgentWorkItem = {
  id: string;
  agent_id: string;
  agent_handle: string;
  channel_id: string | null;
  channel_name: string | null;
  thread_root_id: string | null;
  source_message_id: string | null;
  inbox_item_id?: string | null;
  task_id: string | null;
  task_number: number | null;
  source_kind: string;
  title: string;
  context: string;
  status: string;
  run_id: string | null;
  created_at: string;
  updated_at: string;
  completed_at: string | null;
};

export type AgentActivity = {
  id: string;
  agent_id: string | null;
  agent_handle: string;
  run_id: string | null;
  kind: string;
  phase: string;
  status: string;
  title: string;
  summary: string;
  detail: string;
  metadata: Record<string, unknown>;
  created_at: string;
};

export type SupervisorStatus = {
  pid: number | null;
  status: string;
  updated_at: string | null;
};

export type LaunchAgentStatus = {
  label: string;
  plist_path: string;
  installed: boolean;
  loaded: boolean;
};

export type RuntimeCheck = {
  runtime: string;
  command: string;
  available: boolean;
  detail: string;
};

export type Bootstrap = {
  db_url: string;
  web_base_url: string | null;
  owner_profile: OwnerProfile;
  channels: Channel[];
  thread_activities: ThreadActivity[];
  channel_members: ChannelMember[];
  agents: Agent[];
  messages: Message[];
  saved_messages: SavedMessage[];
  dismissed_inbox_items: Record<string, string>;
  read_inbox_items: Record<string, string>;
  artifacts: Artifact[];
  tasks: Task[];
  reminders: Reminder[];
  agent_schedules: AgentSchedule[];
  agent_runs: AgentRun[];
  agent_work_items: AgentWorkItem[];
  agent_activities: AgentActivity[];
  supervisor: SupervisorStatus;
  launch_agent: LaunchAgentStatus;
};

export type SearchScope = "all" | "messages" | "channels" | "tasks" | "agents" | "activity" | "artifacts";

export type SearchTimeRange = "any" | "today" | "7d" | "30d";

export type SearchResult = {
  id: string;
  kind: string;
  title: string;
  detail: string;
  excerpt: string;
  createdAt: string | null;
  channelId: string | null;
  threadId: string | null;
  agentId: string | null;
  senderRole?: string | null;
};

export type ActivityFeedKind = "mention" | "dm" | "thread" | "task" | "reminder" | "channel";

export type ActivityFeedItem = {
  id: string;
  dismissId: string;
  kind: ActivityFeedKind;
  title: string;
  excerpt: string;
  surface: string;
  actor: string;
  timestamp: string;
  unread: boolean;
  actorAgentId?: string | null;
  actorRole?: string | null;
  channelId: string | null;
  threadId: string | null;
  messageId: string | null;
  taskId: string | null;
  reminderId: string | null;
  replyCount: number;
  newCount: number;
};

export type AgentForm = {
  handle: string;
  displayName: string;
  role: string;
  avatar: string;
  runtime: string;
  model: string;
  reasoningEffort: string;
  serviceTier: string;
  description: string;
  launchCommand: string;
  workingDirectory: string;
  dailyBudgetUsd: string;
};

export const EMPTY_AGENT_FORM: AgentForm = {
  handle: "",
  displayName: "",
  role: "agent",
  avatar: "",
  runtime: "codex",
  model: "gpt-5.5",
  reasoningEffort: "medium",
  serviceTier: "",
  description: "",
  launchCommand: "",
  workingDirectory: "",
  dailyBudgetUsd: "",
};

export const TASK_STATUSES = ["todo", "in_progress", "in_review", "done"] as const;

export const ACTIVE_RUN_STATUSES = new Set(["starting", "running", "stopping"]);

export const RUNTIME_PRESETS: Record<string, { label: string; defaultModel: string; commandName: string; models: string[] }> = {
  codex: {
    label: "Codex",
    defaultModel: "gpt-5.5",
    commandName: "codex",
    models: ["gpt-5.5", "gpt-5.4", "gpt-5.4-mini", "gpt-5.3-codex", "gpt-5.3-codex-spark"],
  },
  claude: {
    label: "Claude",
    defaultModel: "sonnet",
    commandName: "claude",
    models: ["sonnet", "opus", "haiku"],
  },
};

const MODEL_LABELS: Record<string, string> = {
  "gpt-5.5": "GPT-5.5",
  "gpt-5.4": "GPT-5.4",
  "gpt-5.4-mini": "GPT-5.4 Mini",
  "gpt-5.3-codex": "GPT-5.3 Codex",
  "gpt-5.3-codex-spark": "GPT-5.3 Codex Spark",
};

export const CODEX_REASONING_EFFORTS = [
  { value: "low", label: "Low" },
  { value: "medium", label: "Medium" },
  { value: "high", label: "High" },
  { value: "xhigh", label: "Extra High" },
] as const;

export const CODEX_SERVICE_TIERS = [
  { value: "", label: "Standard" },
  { value: "fast", label: "Fast" },
] as const;

export function modelOptionsForRuntime(runtime: string, currentModel = "") {
  const models = RUNTIME_PRESETS[runtime]?.models ?? [];
  if (!currentModel || models.includes(currentModel)) return models;
  return [currentModel, ...models];
}

export function modelLabel(model: string) {
  return MODEL_LABELS[model] ?? model;
}
