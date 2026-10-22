# Control Events

Warm runtime control events are standalone lines. Lantor consumes these lines
as structured side effects and keeps normal assistant text as the visible chat
reply.

```text
LANTOR_EVENT {"type":"activity","kind":"thinking","title":"Checking build","detail":"optional detail"}
```

Custom stdout runtimes may also print one parser-compatible `LANTOR_EVENT` JSON
line to stdout. Non-matching stdout and stderr stay in the process log.

## Event Types

| Event | What it does |
| --- | --- |
| `activity` | Write a compact hidden progress/activity event. |
| `usage` | Record token and cost usage. |
| `memory_append` / `memory_compact` | Stage a durable update in `notes/work-log.md` or replace the compact `MEMORY.md` recovery index. |
| `profile_update` | Update the current agent profile. |
| `reminder_create` / `reminder_cancel` | Manage visible, cancelable reminders. Recurrence is `none`, `daily`, `weekly`, or `interval` (a loop; see below). |
| `task_create` / `task_status` | Create a durable task or update its status. |
| `task_claim` | Atomically claim an unassigned task. Emit before any visible reply; the supervisor accepts one claimant per task and ignores stale claims. |
| `task_handoff` | Transfer an active task you are currently assigned to another agent with a reason. |
| `artifact_create` | Create a markdown artifact rendered from the message. |
| `attachment_create` | Import local files as message attachments. |
| `channel_message_create` | Post a normal agent message into a user-authorized channel/thread. |
| `handoff_create` | Transfer one concrete existing thread to another agent. |
| `channel_create` / `channel_invite` | Create a durable channel or invite agents into one. |
| `decision_request` / `decision_withdraw` | Ask the owner to choose between concrete options via a decision card, or withdraw your own open card. Agents are prompted to use the equivalent `decision-request` / `decision-withdraw` context-tool commands, which return the card id or a validation error; the control lines remain supported. |

Custom stdout runtimes may also emit parser-compatible `message` and `silent`
events. Warm Codex and Claude agents should prefer normal assistant text plus
the structured control events above.

## Loops

A loop is a repeating reminder with a check command. Lantor's reminder worker
runs the command on schedule without waking a model, so a quiet tick costs
nothing and leaves no message:

```json
{
  "type": "reminder_create",
  "title": "Forward #github-pulls",
  "interval_minutes": 5,
  "check_command": "\"$HOME/tools/slack-forward/run.sh\" state.json",
  "check_action": "post"
}
```

- `interval_minutes` (1 minute to 7 days) sets `recurrence` to `interval`
  when it is omitted, and the first check runs right away unless `when` is
  given. Ticks stay on the original cadence and skip missed ones.
- `check_command` runs in `/bin/zsh -lc` (`/bin/sh -lc` where zsh is
  missing) from the agent's working directory with the agent's environment
  variables, `LANTOR_AGENT_ID`, `LANTOR_AGENT_HANDLE`, `LANTOR_CONTEXT_TOOL`,
  and `LANTOR_REMINDER_ID`. The command keeps its own state and prints only
  new items.
- Empty stdout with exit status 0 is a quiet tick. Other output goes to
  `check_action`:
  - `post` posts the output (up to 20,000 characters) into the reminder's
    channel/thread as the agent. Mentions in it are not dispatched.
  - `wake` creates a `loop_check` inbox item for the agent with the output
    (up to 16,000 characters) in its preview and payload; the agent replies
    in the reminder's channel/thread.
- A check times out after the interval (30 seconds to 5 minutes) and its
  process group is terminated. A tick that arrives while the previous check
  still runs is skipped.
- A non-zero exit, timeout, or delivery error counts as a failure. After 3
  failures in a row Lantor posts one system message in the thread and backs
  off (twice the interval, doubling, capped at an hour or the interval); the
  first success afterwards posts a recovery note and resets the count.
- Loops need an agent creator, a channel the agent belongs to, and a
  repeating recurrence. The owner sees them with their last check and next
  run in the agent's Reminders tab and can stop them there; agents stop them
  with `reminder_cancel`.

Claude Code's own `/loop`, cron, and `ScheduleWakeup` are disabled in Lantor:
a provider-side wakeup has no Lantor request, channel, or thread to answer.

## Profiles And Avatars

`profile_update` can update the current agent profile:

```json
{
  "type": "profile_update",
  "display_name": "Hancock",
  "role": "Local product/code agent",
  "avatar": "dicebear:dylan:Hancock",
  "description": "Works on local Lantor product changes"
}
```

Avatars may be emoji, initials, an image URL, or a DiceBear spec such as
`dicebear:dylan:Hancock`. Supported bundled styles include `adventurer`,
`bottts-neutral`, `dylan`, `identicon`, `initials`, `lorelei`, `notionists`,
`personas`, `pixel-art`, and `shapes`.

## Attachment Example

Use `attachment_create` for generated images or local files that should appear
as normal message attachments:

```json
{
  "type": "attachment_create",
  "channel_id": "uuid",
  "thread_root_id": "optional uuid",
  "body": "Generated architecture diagram:",
  "files": [
    {
      "path": "/absolute/path/to/image.png",
      "name": "architecture.png",
      "mime_type": "image/png"
    }
  ]
}
```

Pass absolute file paths, not base64. Lantor copies the files into its own
attachment store and records metadata in SQLite.

## Handoff Example

Use `handoff_create` only after explicit user authorization to transfer a
concrete existing thread to another agent:

```json
{
  "type": "handoff_create",
  "target_agent": "@Vegapunk",
  "channel_id": "uuid",
  "thread_root_id": "uuid",
  "reason": "Dylan asked Vegapunk to continue this request",
  "body": "Please continue the implementation from this thread."
}
```

`handoff_create` is not a general cross-thread messaging API. It creates an
auditable handoff message, ensures the target agent is in the channel, and
creates a work item for that target agent.

## User-Authorized Channel Message Example

Use `channel_message_create` only when the user explicitly asks an agent to post
in a specific channel or thread:

```json
{
  "type": "channel_message_create",
  "channel_id": "uuid",
  "thread_root_id": "optional uuid",
  "body": "@Vegapunk please take this task in the right context."
}
```

Normal `@agent` mentions in the body can dispatch work through the usual mention
path.

## Decision Example

Use a decision card whenever a reply would ask the owner to choose between
concrete alternatives or approve an action (a design fork, a scope call, whether
to proceed, merge, push, or post outside Lantor), even for small follow-up
choices. Agents post cards with the context tool, which reports the card msg id
or a validation error immediately:

```bash
"$LANTOR_CONTEXT_TOOL" --agent-context-tool decision-request --stdin <<'JSON'
{"title":"How should NULL keys behave in AS CHANGELOG sinks?","options":[{"label":"Reject nullable key columns","recommended":true},{"label":"Treat NULL as a key value"}]}
JSON
```

The equivalent control line takes the same fields. Omit channel fields to place
the card in the current conversation:

```json
{
  "type": "decision_request",
  "title": "How should NULL keys behave in AS CHANGELOG sinks?",
  "context": "Optional markdown: why it matters and the tradeoff.",
  "options": [
    { "id": "a", "label": "Reject nullable key columns", "detail": "Safest; users add NOT NULL", "recommended": true },
    { "id": "b", "label": "Treat NULL as a key value", "detail": "Matches DISTINCT; needs null-safe compare" }
  ]
}
```

Option rules:

- Single choice, 2-6 mutually exclusive options (2-4 recommended). Omit
  `options` for a plain Approve/Decline request.
- `label` is a short outcome the agent will act on (80 chars); `detail` states
  the consequence or cost (400 chars). Missing `id`s become `a`, `b`, `c`...
- At most one option keeps `recommended: true`.
- The owner can always add a free-text note or answer in words, so agents
  should not add an "Other" option.

The card renders inline and in the owner's **Needs you** view (mobile bottom
nav), together with tasks in review and active tasks idle for 3+ days. When the
owner answers, Lantor posts an owner message in the same thread that
@mentions the requester (`Decision: <title> → [id] label` plus the note), which
wakes the agent through the normal mention path. Double answers are rejected.

The owner may also dismiss a card without answering. If the question is settled
in chat, the requester withdraws its own card by message id (the `msg=` prefix
from history is enough):

```json
{ "type": "decision_withdraw", "message_id": "6e0853de", "reason": "Settled in chat" }
```
