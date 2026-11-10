# Agent Runtime Model

Lantor runs agents as local processes. Each agent profile stores runtime
configuration, a model, Codex reasoning and speed preferences, an optional
custom launch command, an optional working directory, and profile metadata. The
desktop app starts the same binary in
`--supervisor` mode; the supervisor owns process launch, stop commands, run
logs, event ingestion, and work-item scheduling.

Codex model presets cover GPT-6 and newer: GPT-6 Astra, GPT-6.1 Sol,
GPT-6.1 Terra, GPT-6 Sol and GPT-6 Luna. The default is GPT-6 Sol. The Codex
app-server catalog exposes reasoning efforts through `ultra` for Astra, Sol
and Terra, and through `max` for Luna. These are the CLI's ranges, which can
differ from the direct API's. GPT models older than GPT-6 cannot be saved in
a Codex profile. An existing older model remains visible as a disabled,
retired option until the owner selects a replacement; changing the presets
does not rewrite existing profiles or run history.

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

A work item's prompt carries the recent messages of its channel or thread,
read when the turn starts (`runtime/surface.rs`) for every runtime. Queued
work stores only what woke the agent, so each message appears once, including
anything posted while the work waited. The highest message in that block marks
what the agent has seen, which the freshness check compares against before
posting a reply.

Stdout-command runtimes are still supported for custom scripts. They can print
one line to stdout with the `LANTOR_EVENT ` prefix followed by JSON. Non-matching
stdout and stderr are preserved only in the run log.

## Tools in the agent profile

Profile has a collapsed **Tools** section. Opening Profile does not start a
provider process. Expanding Tools calls `load_agent_tools`; **Refresh** bypasses
the three-minute, per-agent cache. Overlapping requests for the same agent share
one discovery, including concurrent refreshes. Changes to its provider, model,
working directory or environment invalidate the cached result.

Discovery runs in the application's API process, outside the supervisor and
warm runtime registry. It uses the agent's workspace and environment and shares
the provider's CLI launch helpers. It never sends a user message, calls a model,
creates a provider thread or writes a Lantor run. Each probe owns a separate
process group, which is killed and reaped on completion or timeout; dropping a
request also kills its probe group. Individual requests time out after 30 seconds,
and a whole probe after 95 seconds. A failing section does not hide the others.
Only tool names and descriptions reach the UI, without JSON schemas.

- **Claude MCP:** stream-json control requests (`initialize`, then `mcp_status`)
  read `mcpServers`. Empty and pending results are retried for about 30 seconds;
  they become **Not confirmed yet**, not a connection failure. A failed CLI or
  control request displays **Could not read status**. A `needs-auth` server
  shows **Login required** and sorts first, like `failed`; a server's `error`
  text appears under it.
- **Claude plugins and skills:** no prompt means no `init` event. These lists
  instead come from the latest archived run containing a `system/init` whose
  `cwd` matches the workspace. Repository-specific runs are skipped. The UI
  labels the snapshot's age and separates installed plugins from `@builtin`
  plugins. With no workspace snapshot it says so; unreadable archives are shown
  as unavailable. The `mcp_servers` field in that event is never used.
- **Codex:** an initialized app-server lists `mcpServerStatus/list`, then
  `plugin/list` (installed entries only), then `skills/list` with the workspace
  and `forceReload`. The order matters because plugin discovery materializes
  plugin skills in CLI 0.165.0, so skills are **Not confirmed yet** when
  `plugin/list` fails. MCP pagination is followed. Without a thread,
  `runtimeStatus` can be null: the UI shows **Unknown**, tool counts,
  `authStatus` and `toolsError`, and does not infer an online state from a
  non-empty catalog. Plugin APIs are experimental; incompatible replies display
  unavailable rather than changing provider settings.

- **Apps:** what an agent can reach, by app rather than by plugin. Claude apps
  are its MCP servers: `claude.ai <App>` is a claude.ai connector, anything
  else a configured MCP server, with the MCP status mapped to the app. Codex
  apps come from `app/installed`: `callable` is **Connected**, enabled but not
  callable is **Needs setup**, and disabled is **Disabled**. Tool counts group
  the `codex_apps` tools by `_meta.connector_id`. Each installed plugin's
  `plugin/read` names the apps it provides; those reads are sent together
  because a remote plugin takes over a second. The `app/list` catalog is not
  read: it holds thousands of apps and its first page took about 30 seconds.

**Watched apps** head the section. They are the owner's list
(`owner_profile.watched_apps`, default Notion, Slack and Gmail, edited in
place through `update_watched_apps`), shared by every agent and device. A
watched app that is neither an app nor an MCP server of the same name shows
**Not connected**, with where to connect it for that runtime. Names match
ignoring case, spaces, punctuation and a `claude.ai` prefix. Other apps are
folded below, problems first. Under Plugins, the ones that provide apps are
listed with them.

MCP errors appear first. Each server's tool list opens separately and is
searchable; plugins and skills are nested disclosures. **Restart** uses the
existing agent restart action when changed logins or tools need to reach a warm
process. Lantor does not connect apps or change provider configuration, and
there is no overview or alerting here.

The synthetic browser suite is `npm run test:agent-tools`. The ignored
`application::agent_tools::tests::real_cli_inventory` test can check real CLI
discovery without starting a backend: explicitly set `LANTOR_TOOLS_TEST_DB`
and `LANTOR_TOOLS_TEST_AGENT`. It opens that database read-only and reports
counts, not transcripts or environment values.

## CLI updates

Profile shows the installed Claude Code or Codex version under Model
(`load_cli_version`) and offers **Update to x.y.z** when the npm registry has a
later release. One install of each CLI serves every agent on that runtime, so
the check and the update are per runtime, not per agent
(`application/cli_updates.rs`).

- The installed version comes from `claude --version` or `codex --version` in
  a login shell, read on every request. The latest version comes from the npm
  registry manifest of `@anthropic-ai/claude-code` or `@openai/codex`, cached
  for an hour (ten minutes after a failed fetch). Claude Code follows the
  `latest` tag unless `autoUpdatesChannel` in its `settings.json` is `stable`.
- **Update** (`update_cli`) runs the CLI's own `claude update` or
  `codex update`, which knows how the CLI was installed. It runs in the
  background in the API process, at most one per runtime, with a five-minute
  limit; the page polls `load_cli_version` until it finishes.
- An update counts as failed when the command exits non-zero or the CLI is
  still older than the latest release afterwards. The page then shows the end
  of the output, **Try again**, and **Ask @agent to fix it**, which opens the
  agent's DM and sends the command, versions and output as an owner message.
- On the desktop, the agent's avatar hover card offers the same update, so it
  is seen without opening Profile: a line with **Update** when a later release
  exists, then the running update, the result for ten minutes, or a failure
  with **Ask @agent to fix it** and **Try again**. While the CLI is current the
  card shows nothing extra. Profile and the hover cards share one status per
  runtime (`src/cli-versions.ts`). Since every read runs `--version` in a
  login shell, a hover card reuses a status read in the last minute; Profile
  reads it again each time it opens, and both read it every two seconds while
  an update runs.
- Running agents keep their current process. Each one starts the new version
  when its warm process next starts: after the ten-minute idle stop or a
  Restart, resuming the same session.
- The result of the last update lives in memory and is gone after the app
  restarts.

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

A turn that lost its connection to the provider ends like a failed turn, except
that its request goes back to the queue (`end_turn` with a `network_retry`) and
its partial reply is deleted (`runtime/network_retry.rs`):

- The next turn resumes the session, and its prompt says what happened: work
  already done stands, and the reply must be written again.
- Delays after each failure grow from 1 to 15 minutes. A new message on the same
  surface merges into the request and starts it at once.
- Only failures on a Mac that is really awake count, up to 5 retries. A retry
  waits for a full wake (`pmset -g systemstate`), and a failure costs nothing if
  the run slept through it (its wall clock outran its monotonic clock) or it
  ended in a dark wake. With the lid closed the Mac only has dark wakes, a few
  seconds about every 17 minutes.
- The failure after the last retry fails the request as usual and raises a card;
  answering it runs the request again.

Usage limits, other API errors, cancels and a process that died are not retried.

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
"$LANTOR_CONTEXT_TOOL" --agent-context-tool message-read --message-id "<msg id or prefix>"
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

Message rows in wake prompts, inbox previews, history and search keep long
bodies to a bounded length by cutting the middle. The marker left in the cut
names the message: `[... Lantor omitted N chars here; message-read
--message-id <msg> prints the full message ...]`. `message-read` accepts a full
id or the 8+ character prefix shown as `msg=` and prints the whole body (up to
`--max-chars`, 40,000 by default), with its thread, task and attachments.

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
