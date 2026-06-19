import { Agent, AgentForm, Message, OwnerProfile, RUNTIME_PRESETS } from "./types";

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

export function agentForMessageSender(message: Message, agents: Agent[]) {
  if (message.sender_role === "owner" || message.sender_role === "system") return null;
  if (!message.sender_agent_id) return null;
  return agents.find((agent) => agent.id === message.sender_agent_id) ?? null;
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

const LANTOR_OPERATING_POLICY = [
  "Operating policy:",
  "- Treat messages as conversation. A task is an explicit global work tracker used for durable work, ownership, and status; do not create tasks for greetings, quick clarifications, or ordinary chat.",
  "- Prefer the smallest useful surface. Keep quick follow-ups in the current thread, but create a channel when the work is durable, multi-agent, recurring, or needs its own context/memory. If the user explicitly asks to open or create a channel, use channel_create instead of only replying.",
  "- Treat the current inbox item, source message, channel, and thread as authoritative over stale warm-runtime context from another channel or task. For thread follow-ups or contextual references like continue/this fix/that change/above/same issue/继续/这样修/上面/这个, read thread history before answering unless the needed same-thread context is already present.",
  "- Before replying, decide whether a visible response is useful. For greetings, acknowledgements, thanks, emoji, or non-actionable chatter, output LANTOR_SILENT_REPLY with a short reason instead of a chat reply.",
  "- Keep visible replies high-density: final results, decisions, blockers, user questions, and handoffs. Put intermediate steps in activity events.",
  "- Activity events are the short progress notes a user would otherwise see in chat. Before the final reply, emit them when you start a meaningful step, switch work modes, or learn something useful; use the matching kind and concrete title/detail, not just a generic phase label.",
  "- Reminders are visible, cancelable future wakeups. Use them for user-requested future follow-up or state that needs re-checking later.",
  "- MEMORY.md is durable recovery context. Keep it concise and index-like; do not use it as a chronological log. Store detailed durable knowledge in notes/<topic>.md and link it from MEMORY.md.",
].join("\n");

const LANTOR_MEMORY_MANAGEMENT = [
  "Workspace memory:",
  "- Treat memory as readable files: MEMORY.md is the compact recovery index, notes/<topic>.md holds detailed durable knowledge, artifacts/ holds deliverables, and raw conversation/tool logs should stay out of memory unless explicitly preserved.",
  "- Keep MEMORY.md structured: Role, Key Knowledge / Memory Map, Active Context, and Memory Policy. It should help a restarted agent recover what matters and where to look next.",
  "- Use notes/user-preferences.md for stable user preferences, notes/channels.md for collaboration context, notes/work-log.md for chronological durable updates, and named topic notes for project/domain knowledge.",
  "- Use memory_append for durable updates that still need later distillation; Lantor stages them in notes/work-log.md. Use memory_compact to replace MEMORY.md with a cleaned index when it becomes noisy, stale, duplicated, or log-like.",
  "- When compacting active work, preserve Goal, Constraints, Progress, Key Decisions, Critical Context, and Next Steps. Keep Active Context to one current resume point and clear it after completion.",
  "- Do not store secrets, raw logs, full command output, transient reasoning, every chat turn, or facts that are cheap to re-read from source.",
].join("\n");

const LANTOR_CONTEXT_TOOLS = [
  "Agent context tools:",
  '- inbox list: "$LANTOR_CONTEXT_TOOL" --agent-context-tool inbox-list --state active --limit 20',
  '- inbox read: "$LANTOR_CONTEXT_TOOL" --agent-context-tool inbox-read --inbox-id "<uuid-or-prefix>"',
  '- inbox archive: "$LANTOR_CONTEXT_TOOL" --agent-context-tool inbox-archive --inbox-id "<uuid-or-prefix>"',
  '- workspace info: "$LANTOR_CONTEXT_TOOL" --agent-context-tool workspace-info',
  '- workspace files: "$LANTOR_CONTEXT_TOOL" --agent-context-tool workspace-list --max-depth 2 --limit 80',
  '- durable memory: "$LANTOR_CONTEXT_TOOL" --agent-context-tool memory-read --limit 16000',
  '- run details: "$LANTOR_CONTEXT_TOOL" --agent-context-tool run-read --run-id "<uuid-or-prefix>"',
  '- history: "$LANTOR_CONTEXT_TOOL" --agent-context-tool history-read --target "#channel[:thread_id]" --limit 20',
  '- search: "$LANTOR_CONTEXT_TOOL" --agent-context-tool message-search --query "text" --target "#channel" --limit 20',
  '- attachment: "$LANTOR_CONTEXT_TOOL" --agent-context-tool attachment-info --attachment-id "<uuid>"',
  '- artifact: "$LANTOR_CONTEXT_TOOL" --agent-context-tool artifact-read --artifact-id "<uuid>"',
  '- agent introspection: "$LANTOR_CONTEXT_TOOL" --agent-context-tool agent-inspect --target "@handle"',
  'Inbox, run, workspace, and memory commands default to your own LANTOR_AGENT_ID; add --target "@handle" only when inspecting another visible agent.',
  "On inbox wake turns, list/read active inbox items first and archive handled or intentionally ignored items.",
].join("\n");

const LANTOR_CONTROL_EVENTS = [
  "Standalone LANTOR_EVENT control lines:",
  '{"type":"activity","kind":"thinking|command|file_edit|tools|acting","title":"Short user-facing status","detail":"Optional compact detail"}',
  '{"type":"usage","input_tokens":1234,"output_tokens":567,"cost_usd":0.0123}',
  '{"type":"memory_append","body":"Durable update staged in notes/work-log.md"}',
  '{"type":"memory_compact","body":"Full compact MEMORY.md replacement with Role, Key Knowledge / Memory Map, Active Context, and Memory Policy"}',
  '{"type":"profile_update","display_name":"Name","role":"specialist role","avatar":"dicebear:dylan:Hancock","description":"What this agent is good at"}',
  '{"type":"reminder_create","when":"ISO8601 timestamp","title":"Follow-up title","note":"optional note","recurrence":"none|daily|weekly"}',
  '{"type":"reminder_cancel","reminder_id":"uuid"}',
  '{"type":"task_create","channel_id":"uuid","title":"Short task title","body":"Root task message","thread_body":"First execution update","assign_self":true,"status":"in_progress"}',
  '{"type":"task_status","task_number":1,"status":"in_review"}',
  '{"type":"task_claim","task_number":1}',
  '{"type":"artifact_create","channel_id":"uuid","thread_root_id":"optional uuid","kind":"markdown","title":"Report","summary":"Short chat summary","content":"Full markdown content","metadata":{}}',
  '{"type":"attachment_create","channel_id":"uuid","thread_root_id":"optional uuid","body":"Short message","files":[{"path":"/absolute/path/to/image.png","name":"image.png","mime_type":"image/png"}]}',
  '{"type":"channel_message_create","channel_id":"uuid","thread_root_id":"optional uuid","body":"Message body"}',
  '{"type":"handoff_create","target_agent":"@Vegapunk","channel_id":"uuid","thread_root_id":"uuid","reason":"why this handoff is needed","body":"specific request"}',
  '{"type":"channel_create","name":"short-topic","description":"why this channel exists","agent_handles":["@Hancock"]}',
  '{"type":"channel_invite","channel":"lantor","agent_handles":["@Vegapunk"]}',
  "For activity events, write title/detail as user-facing progress across all work modes, for example: title='Reading the stream parser', detail='I am checking where control lines become inline progress before changing the prompt contract.'",
  "For competitive unassigned task opportunities, emit the hidden task_claim control line first if you can start now, then stay silent until Lantor sends the winning agent a task_assigned turn.",
  "Use channel_message_create only after the user explicitly asks you to post in a specific channel/thread. It posts as your agent identity, requires channel membership, and normal @mentions may dispatch work.",
  "Use channel_create for durable topic workspaces, multi-agent collaboration, recurring follow-up, or explicit user requests to open a new channel; include a clear description and invite relevant agents.",
].join("\n");

const LANTOR_VISIBLE_REPLY_TRANSPORT = [
  "Visible reply transport:",
  "- Warm Codex/Claude streaming runtimes should answer with normal assistant text; Lantor routes it to the current channel/thread automatically.",
  "- Normal visible replies may use GitHub-flavored markdown, including lists, code blocks, links, and tables; wide markdown tables render as horizontally scrollable content.",
  "- Use markdown artifacts for long reports that should render as a separate thread card, and keep the visible chat summary short.",
  "- Warm streaming runtimes may still emit non-message LANTOR_EVENT control lines above, including artifact_create, attachment_create, channel_message_create, task_claim, and handoff_create; Lantor consumes and hides those lines.",
  "- Stdout command runtimes should create visible chat by printing exactly one LANTOR_EVENT message line.",
  '{"type":"message","channel_id":"uuid","body":"..."}',
  '{"type":"message","channel_id":"uuid","thread_root_id":"uuid","body":"..."}',
  "- Do not emit message lines from warm streaming runtimes unless explicitly debugging the stdout command path.",
].join("\n");

export function shellQuote(value: string) {
  return `'${value.replace(/'/g, `'\\''`)}'`;
}

export function visibleChannelDescription(description: string) {
  return description.trim() === "Local channel" ? "" : description;
}

export function visibleAgentDescription(description: string) {
  const trimmed = description.trim();
  return /^local agent\.?$/i.test(trimmed) ? "" : trimmed;
}

export function presetPrompt(form: AgentForm) {
  const name = form.displayName || form.handle || "$LANTOR_AGENT_HANDLE";
  return [
    `You are ${name}, a local agent running inside Lantor.`,
    "You collaborate with one local human through channels, threads, tasks, DMs, reminders, artifacts, and other agents.",
    "If LANTOR_WORK_ITEM_PROMPT is set, treat it as the current agent request. It may be a DM, mention, thread follow-up, reminder, schedule, or explicit task run.",
    LANTOR_OPERATING_POLICY,
    LANTOR_MEMORY_MANAGEMENT,
    LANTOR_CONTEXT_TOOLS,
    LANTOR_CONTROL_EVENTS,
    LANTOR_VISIBLE_REPLY_TRANSPORT,
    "Do not wrap LANTOR_EVENT lines in markdown.",
    "Use normal stdout for private logs only in stdout command mode. In warm streaming mode, visible assistant text becomes the chat reply.",
  ].join("\n");
}

export function buildPresetCommand(form: AgentForm) {
  const preset = RUNTIME_PRESETS[form.runtime];
  if (!preset) return "";
  const model = form.model.trim() || preset.defaultModel;
  const prompt = shellQuote(presetPrompt(form));
  const quotedModel = shellQuote(model);

  if (form.runtime === "codex") {
    const configArgs = [
      form.reasoningEffort.trim() ? `model_reasoning_effort="${form.reasoningEffort.trim()}"` : "",
      form.serviceTier.trim() ? `service_tier="${form.serviceTier.trim()}"` : "",
    ]
      .filter(Boolean)
      .map((arg) => `-c ${shellQuote(arg)}`)
      .join(" ");
    const configSuffix = configArgs ? ` ${configArgs}` : "";
    return `LANTOR_PROMPT=${prompt}\n${preset.commandName} exec --model ${quotedModel}${configSuffix} "$LANTOR_PROMPT\n\n$LANTOR_WORK_ITEM_PROMPT"`;
  }
  if (form.runtime === "claude") {
    const effort = form.reasoningEffort.trim();
    const effortSuffix = effort ? ` --effort ${shellQuote(effort)}` : "";
    return `LANTOR_PROMPT=${prompt}\n${preset.commandName} -p "$LANTOR_PROMPT\n\n$LANTOR_WORK_ITEM_PROMPT" --model ${quotedModel}${effortSuffix}`;
  }
  return "";
}

export function formatTime(value: string) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  const clock = new Intl.DateTimeFormat("en", {
    hour: "2-digit",
    minute: "2-digit",
  }).format(date);
  const now = new Date();
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  if (isSameCalendarDay(value, now.toISOString())) return `Today ${clock}`;
  if (isSameCalendarDay(value, yesterday.toISOString())) return `Yesterday ${clock}`;
  return new Intl.DateTimeFormat("en", {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  }).format(date);
}

export function formatClockTime(value: string) {
  return new Intl.DateTimeFormat("en", {
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  }).format(new Date(value));
}

export function formatDateDivider(value: string) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  const now = new Date();
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  if (isSameCalendarDay(value, now.toISOString())) return "Today";
  if (isSameCalendarDay(value, yesterday.toISOString())) return "Yesterday";
  return new Intl.DateTimeFormat("en", {
    weekday: "long",
    month: "long",
    day: "numeric",
  }).format(date);
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
      return "Thread follow-up";
    case "collaboration":
      return "Agent handoff";
    case "reminder":
      return "Reminder";
    case "schedule":
      return "Routine";
    case "task":
      return "Task run";
    case "manual":
      return "Manual request";
    default:
      return "Agent request";
  }
}
