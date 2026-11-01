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
- Retry creates a new queued work item instead of mutating historical state.

Warm Codex and Claude runtimes reply with normal assistant text for the current
channel or thread. Lantor routes that text into the correct chat surface. They
may also emit standalone `LANTOR_EVENT` control lines for structured side
effects.

Both warm runtimes keep one long-lived process per agent, and they share its
handling in `src-tauri/src/runtime/warm.rs`: the per-agent registry, reusing
or retiring a process before a start, the stderr log, what happens when the
process exits, and the idle stop. `claude.rs` and `codex.rs` implement
`WarmProvider` and keep what depends on their protocol: spawning and the
handshake, stdout parsing, turns, follow-ups and interrupts.

Restart in the agent panel asks for a fresh process. The request is stored on
the agent (`runtime_restart_requested_at`), so it survives the order in which
the supervisor picks up queued starts. The next start retires the agent's idle
process and spawns a new one, which resumes the same Claude session or Codex
thread. A process that is in a turn is replaced at the first start after that
turn. Use it when something the provider reads only at startup has changed,
such as an MCP login.

Stdout-command runtimes are still supported for custom scripts. They can print
one line to stdout with the `LANTOR_EVENT ` prefix followed by JSON. Non-matching
stdout and stderr are preserved only in the run log.

## Run Lifecycle

A run, the work item it carries, and its agent each have a status, and they
change together. Only `src-tauri/src/runtime/lifecycle.rs` writes those
statuses (and a run's `stopped_at`); a test in
`src-tauri/src/tests/runtime_lifecycle.rs` fails when another file does. It
has one function per transition:

- Queueing and starting: an agent is queued for a start
  (`mark_agent_queued_in_tx`), a run is created with its work item running
  (`create_run_in_tx`), and its process comes up (`mark_run_running_in_tx`).
  A Claude follow-up that becomes its own turn gets a run that starts running
  (`start_follow_up_run`). A start command that ends without leaving a run
  (its request already finished, the budget is spent, the runtime would not
  start) returns a still-queued agent to idle
  (`idle_agent_after_start_command_in_tx`).
- Follow-ups joining a running turn: a Codex steer joins the run
  (`join_work_item_to_run`), goes back to the queue if it cannot be sent
  (`return_steer_to_queue`), and settles when the turn ends
  (`settle_codex_steer`); Claude follow-ups move between their states
  (`move_follow_up_status`) and finish with the turn that consumed them
  (`finish_merged_follow_up`).
- Outcomes during a turn: no visible reply (`mark_work_item_silent`) or a
  reply held back because newer messages arrived (`mark_work_item_held_in_tx`).
- Ending: a warm turn ends (`end_turn`) or a cold process exits
  (`end_cold_run`).
- Failed launches: a cold process cannot spawn (`fail_cold_launch`), a warm
  turn fails before it starts (`fail_warm_start`), a work item fails before
  any run (`fail_work_item_before_run`), or a warm runtime cannot spawn
  (`mark_agent_runtime_failed`).
- Orphans: the supervisor closes runs no process owns, at startup
  (`close_orphaned_runs_at_startup`) and while running (`close_orphaned_run`).
- Stop and cancel: the run is marked stopping and its work item cancelling,
  and whatever can no longer end on its own is finished. An owner's cancel
  writes only while the work item still has the status it read; a request
  whose turn started or finished in between is read again.

Each function updates the rows and enqueues their UI events in one
transaction, or in the caller's transaction for the `_in_tx` variants. The
caller picks the outcome and does what follows the commit: task status,
reconciliation, and the activity log. A new transition is a new function in
that module.

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
"$LANTOR_CONTEXT_TOOL" --agent-context-tool decision-request --stdin <<'JSON'
{"title":"<question>","options":[{"label":"<outcome>","detail":"<cost>","recommended":true},{"label":"<outcome>"}]}
JSON
"$LANTOR_CONTEXT_TOOL" --agent-context-tool decision-withdraw --message-id "<card msg id>" --reason "<why>"
"$LANTOR_CONTEXT_TOOL" --agent-context-tool decision-list --state open
```

Inbox, workspace, and memory commands default to the current agent. Use
`--target "@handle"` only when inspecting another visible agent.
`github sync` reads the bound repository's current review requests, updates the
channel's cached review queue and badge, and returns a compact JSON result. It
does not write to GitHub.
`decision-request` posts a decision card (see `docs/control-events.md`) and
prints its msg id or a validation error. The payload is the `decision_request`
JSON on stdin, `--json`, or `--file`; `--title` alone makes an approve/decline
card. Cards go to the agent's in-flight turn conversation unless `--target` is
given. `decision-withdraw` and `decision-list` act only on the agent's own cards.

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

## Agent Skills

Claude and Codex both start in the agent's working directory. Besides the
user's global skill folders, they load skills from that directory, so a skill
meant for one agent goes there:

- Claude agents: `<working directory>/.claude/skills/<skill>/SKILL.md`
- Codex agents: `<working directory>/.agents/skills/<skill>/SKILL.md`

Skills in `~/.claude/skills` and `~/.codex/skills` stay visible to every agent
of that provider, so keep only shared skills there. Logins, MCP servers and
other provider settings stay shared either way. A running process may not see a
skill added after it started: use Restart in the agent panel, and the next turn
runs in a fresh process.
