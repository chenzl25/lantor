# Agent Runtime Model

Lantor runs agents as local processes. Each agent profile stores runtime
configuration, a model, Codex reasoning and speed preferences, an optional
custom launch command, an optional working directory, and profile metadata. The
desktop app starts the same binary in
`--supervisor` mode; the supervisor owns process launch, stop commands, run
logs, event ingestion, and work-item scheduling.

## Dispatch

Dispatch is work-item based:

- Human messages create work items by mentioning an agent handle, such as `@Hancock`.
- DMs, thread follow-ups, reminders, tasks, channel messages, and handoffs can wake agents.
- One active run is allowed per agent. Extra mentions, retries, and manual dispatches stay queued.
- The supervisor schedules the oldest queued work item for each idle agent.
- Cancellation marks queued work as cancelled or sends a stop command for a running run.
- Retry creates a queued attempt on the original surface and records its ID on the source request. Repeated or concurrent retries of that source return the same attempt, including after a lost response; retry the new attempt if it also fails. Explicit retry recovers agents left in error state.
- Progress cards and Agent detail expose Stop and Retry. A stop remains pending until the supervisor confirms termination; status and stop-command writes are atomic. The latest unretried failure per agent/channel/thread remains available after refresh, with its error reason.

Warm Codex and Claude runtimes reply with normal assistant text for the current
channel or thread. Lantor routes that text into the correct chat surface. They
may also emit standalone `LANTOR_EVENT` control lines for structured side
effects.

Stdout-command runtimes are still supported for custom scripts. They can print
one line to stdout with the `LANTOR_EVENT ` prefix followed by JSON. Non-matching
stdout and stderr are preserved only in the run log.

## Context Tools

The supervisor injects `LANTOR_CONTEXT_TOOL` for context access and narrow
agent-triggered Lantor actions. Agents use it to inspect the current workspace,
recover after restart, process inbox wakeups, and invoke explicit automation
requested by the user or a reminder.

```bash
"$LANTOR_CONTEXT_TOOL" --agent-context-tool inbox-list --state active --limit 20
"$LANTOR_CONTEXT_TOOL" --agent-context-tool inbox-read --inbox-id "<uuid-or-prefix>"
"$LANTOR_CONTEXT_TOOL" --agent-context-tool inbox-archive --inbox-id "<uuid-or-prefix>"
"$LANTOR_CONTEXT_TOOL" --agent-context-tool workspace-info
"$LANTOR_CONTEXT_TOOL" --agent-context-tool workspace-list --max-depth 2 --limit 80
"$LANTOR_CONTEXT_TOOL" --agent-context-tool memory-read --limit 16000
"$LANTOR_CONTEXT_TOOL" --agent-context-tool run-read --run-id "<uuid-or-prefix>"
"$LANTOR_CONTEXT_TOOL" --agent-context-tool history-read --target "#channel[:thread_id]" --limit 20
"$LANTOR_CONTEXT_TOOL" --agent-context-tool message-search --query "<text>" --target "#channel" --limit 20
"$LANTOR_CONTEXT_TOOL" --agent-context-tool github sync --channel "#channel"
"$LANTOR_CONTEXT_TOOL" --agent-context-tool attachment-info --attachment-id "<uuid>"
"$LANTOR_CONTEXT_TOOL" --agent-context-tool artifact-read --artifact-id "<uuid>"
"$LANTOR_CONTEXT_TOOL" --agent-context-tool agent-inspect --target "@handle"
```

Inbox, workspace, and memory commands default to the current agent. Use
`--target "@handle"` only when inspecting another visible agent.
`github sync` reads the bound repository's current review requests, updates the
channel's cached review queue and badge, and returns a compact JSON result. It
does not write to GitHub.

## Agent Memory

Each agent has a persistent working directory. By default Lantor uses
`~/Library/Application Support/Lantor/agents/<handle>/`, but the agent profile
can point at any directory you prefer. On first launch Lantor seeds the
directory with `MEMORY.md` and a `notes/` subdirectory.

`MEMORY.md` is the recovery entry point and should stay concise and index-like.
Detailed knowledge belongs in `notes/<topic>.md` files. Agents can also keep
artifacts and task-specific files in their workspace when work needs durable
context.

Memory-related control events:

- `memory_append`: stage a durable update in `notes/work-log.md` while keeping `MEMORY.md` as the compact recovery index.
- `memory_compact`: replace `MEMORY.md` with a cleaned compact version.
- `profile_update`: update display name, role, avatar, or description.

Memory prompts intentionally follow a file-based memory model: raw dialog/tool
output stays out of the compact index, chronological updates are staged in a work
log, and `MEMORY.md` is periodically distilled into a restart-friendly map. When
an agent compacts active work, it should preserve the fields that make the next
turn recoverable: Goal, Constraints, Progress, Key Decisions, Critical Context,
and Next Steps.
