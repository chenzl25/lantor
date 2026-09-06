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

export type AgentSubscriptionWindow = {
  id: string;
  label: string;
  used_percent: number;
  resets_at: number | null;
};

export type AgentSubscriptionStatus = {
  provider: string;
  plan: string | null;
  status: string;
  windows: AgentSubscriptionWindow[];
  observed_at: string;
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
  environment_variables: string;
  working_directory: string;
  workspace_exists: boolean;
  workspace_memory_path: string;
  workspace_memory_exists: boolean;
  workspace_entries: AgentWorkspaceEntry[];
  daily_budget_micros: number;
  subscription_status: AgentSubscriptionStatus | null;
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
  github_unread_count: number;
  github_review_synced_at: string | null;
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
  seq: number;
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

export type ChannelMessageHistory = {
  channel_id: string;
  before_seq: number | null;
  has_more: boolean;
};

export type ChannelMessagePage = {
  messages: Message[];
  next_before_seq: number | null;
  has_more: boolean;
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

export type GithubAccount = {
  login: string;
  host: string;
};

export type GithubRepositoryBinding = {
  channel_id: string;
  repository_id: string;
  name_with_owner: string;
  url: string;
  local_path: string;
  account_login: string;
  review_login: string;
  review_queue_synced_at: string | null;
  issue_queue_synced_at: string | null;
  created_at: string;
  updated_at: string;
};

export type GithubPullRequest = {
  number: number;
  title: string;
  url: string;
  author_login: string;
  is_draft: boolean;
  state: string;
  updated_at: string;
  head_sha: string;
  checks: GithubCheckSummary;
  is_review_requested: boolean;
  is_authored: boolean;
  linked_thread_root_id: string | null;
  linked_task_id: string | null;
  linked_task_number: number | null;
  linked_task_status: string | null;
  linked_assignee_id: string | null;
  linked_assignee_name: string | null;
  review_anchor_sha: string | null;
  review_is_stale: boolean;
  review_commits_ahead: number | null;
};

export type GithubCheckSummary = {
  status: "none" | "success" | "pending" | "failure" | string;
  total: number;
  pending: number;
  failed: number;
  failing_checks: string[];
};

export type GithubLabel = {
  name: string;
  color: string;
};

export type GithubIssue = {
  number: number;
  title: string;
  url: string;
  author_login: string;
  assignee_logins: string[];
  labels: GithubLabel[];
  state: string;
  created_at: string;
  updated_at: string;
  comments_count: number;
  is_related: boolean;
  linked_thread_root_id: string | null;
  linked_task_id: string | null;
  linked_task_number: number | null;
  linked_task_status: string | null;
  linked_assignee_id: string | null;
  linked_assignee_name: string | null;
};

export type GithubIssueDetail = {
  number: number;
  title: string;
  url: string;
  author_login: string;
  assignee_logins: string[];
  labels: GithubLabel[];
  state: string;
  state_reason: string | null;
  body: string;
  milestone: string | null;
  created_at: string;
  updated_at: string;
};

export type GithubChannelOverview = {
  account: GithubAccount;
  binding: GithubRepositoryBinding | null;
  review_requests: GithubPullRequest[];
  issues: GithubIssue[];
};

export type ChannelWikiRevision = {
  id: string;
  short_id: string;
  parent_short_id: string | null;
  content: string;
  author: string;
  note: string;
  created_at: string;
};

export type ChannelWikiOverview = {
  head: ChannelWikiRevision | null;
  revisions: ChannelWikiRevision[];
  max_bytes: number;
};

export type PublishChannelWikiResult = {
  outcome: "published" | "conflict";
  overview: ChannelWikiOverview;
};

export type ChannelWikiSearchHit = {
  channel_id: string;
  channel_name: string;
  channel_kind: string;
  rev_short_id: string;
  author: string;
  updated_at: string;
  snippet: string;
};

export type GithubReviewTaskResult = {
  thread_root_id: string;
  task_id: string;
  task_number: number;
  head_sha: string;
  created: boolean;
};

export type GithubRereviewTaskResult = {
  thread_root_id: string;
  task_id: string;
  task_number: number;
  previous_head_sha: string;
  head_sha: string;
  commits_ahead: number | null;
};

export type GithubIssueTaskResult = {
  thread_root_id: string;
  task_id: string;
  task_number: number;
  anchor_updated_at: string;
  created: boolean;
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
  channel_message_history: ChannelMessageHistory[];
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
  ui_event_cursor: number;
  __perf?: BootstrapPerf;
};

export type BootstrapPerf = {
  options: {
    runtime: string;
    message_load: string;
    include_run_logs: boolean;
    compact_agent_activities: boolean;
    include_artifact_content: boolean;
  };
  total_ms: number;
  serialize_ms: number | null;
  payload_bytes: number | null;
  phases: Array<{
    name: string;
    duration_ms: number;
    rows: number | null;
  }>;
  counts: {
    channels: number;
    thread_activities: number;
    channel_members: number;
    agents: number;
    messages: number;
    saved_messages: number;
    dismissed_inbox_items: number;
    read_inbox_items: number;
    artifacts: number;
    tasks: number;
    reminders: number;
    agent_schedules: number;
    agent_runs: number;
    agent_work_items: number;
    agent_activities: number;
  };
};

export type SearchScope = "all" | "messages" | "channels" | "tasks" | "agents" | "activity" | "artifacts" | "wiki";

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
  environmentVariables: string;
  workingDirectory: string;
  dailyBudgetUsd: string;
};

export const EMPTY_AGENT_FORM: AgentForm = {
  handle: "",
  displayName: "",
  role: "agent",
  avatar: "",
  runtime: "codex",
  model: "gpt-5.6-sol",
  reasoningEffort: "medium",
  serviceTier: "",
  description: "",
  launchCommand: "",
  environmentVariables: "",
  workingDirectory: "",
  dailyBudgetUsd: "",
};

export const TASK_STATUSES = ["todo", "in_progress", "in_review", "done"] as const;

export const ACTIVE_RUN_STATUSES = new Set(["starting", "running", "stopping"]);

export const RUNTIME_PRESETS: Record<string, { label: string; defaultModel: string; commandName: string; models: string[] }> = {
  codex: {
    label: "Codex",
    defaultModel: "gpt-5.6-sol",
    commandName: "codex",
    models: [
      "gpt-6-astra",
      "gpt-5.6-sol",
      "gpt-5.6-terra",
      "gpt-5.6-luna",
      "gpt-5.5",
      "gpt-5.4",
      "gpt-5.4-mini",
      "gpt-5.3-codex",
      "gpt-5.3-codex-spark",
    ],
  },
  claude: {
    label: "Claude",
    defaultModel: "sonnet",
    commandName: "claude",
    models: ["fable", "sonnet", "opus", "haiku"],
  },
};

const MODEL_LABELS: Record<string, string> = {
  fable: "Claude Fable 5.1",
  "claude-fable-5-1": "Claude Fable 5.1",
  "gpt-6-astra": "GPT-6 Astra",
  "gpt-5.6-sol": "GPT-5.6 Sol",
  "gpt-5.6-terra": "GPT-5.6 Terra",
  "gpt-5.6-luna": "GPT-5.6 Luna",
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
  { value: "max", label: "Max" },
  { value: "ultra", label: "Ultra" },
] as const;

function maxCodexReasoningEffort(model: string) {
  const normalizedModel = model.trim().toLowerCase();
  if (
    normalizedModel === "gpt-5.6" ||
    normalizedModel === "gpt-5.6-sol" ||
    normalizedModel === "gpt-5.6-terra"
  ) {
    return "ultra";
  }
  if (normalizedModel === "gpt-6-astra" || normalizedModel === "gpt-5.6-luna") {
    return "max";
  }
  return "xhigh";
}

export function codexReasoningEffortsForModel(model: string) {
  const maximum = maxCodexReasoningEffort(model);
  const maximumIndex = CODEX_REASONING_EFFORTS.findIndex((effort) => effort.value === maximum);
  return CODEX_REASONING_EFFORTS.slice(0, maximumIndex + 1);
}

export function normalizeCodexReasoningEffortForModel(model: string, reasoningEffort: string) {
  const normalizedEffort = reasoningEffort.trim().toLowerCase();
  const requestedIndex = CODEX_REASONING_EFFORTS.findIndex(
    (effort) => effort.value === normalizedEffort,
  );
  if (requestedIndex < 0) return "medium";
  const supportedEfforts = codexReasoningEffortsForModel(model);
  return supportedEfforts[Math.min(requestedIndex, supportedEfforts.length - 1)].value;
}

export const CODEX_SERVICE_TIERS = [
  { value: "", label: "Standard" },
  { value: "fast", label: "Fast" },
] as const;

export const CLAUDE_REASONING_EFFORTS = [
  { value: "", label: "Default" },
  { value: "low", label: "Low" },
  { value: "medium", label: "Medium" },
  { value: "high", label: "High" },
  { value: "xhigh", label: "Extra High" },
  { value: "max", label: "Max" },
] as const;

const CLAUDE_EFFORT_VALUES = new Set(CLAUDE_REASONING_EFFORTS.map((effort) => effort.value));

export function isClaudeReasoningEffort(value: string) {
  return CLAUDE_EFFORT_VALUES.has(value as (typeof CLAUDE_REASONING_EFFORTS)[number]["value"]);
}

export function modelOptionsForRuntime(runtime: string, currentModel = "") {
  const models = RUNTIME_PRESETS[runtime]?.models ?? [];
  if (!currentModel || models.includes(currentModel)) return models;
  return [currentModel, ...models];
}

export function modelLabel(model: string) {
  return MODEL_LABELS[model] ?? model;
}
