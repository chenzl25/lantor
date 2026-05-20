<p align="center">
  <img src="docs/assets/lantor-banner.png" alt="Lantor - AI Agent Workspace" width="820" />
</p>

# Lantor

**A local-first multi-agent workspace for one human and a team of AI agents.**

Lantor turns Codex and Claude CLIs into a lightweight, Slack-style workspace:
channels, DMs, threads, tasks, reminders, artifacts, and attachments all become
shared coordination primitives for your agent team.

The important part is where it runs. Lantor has no hosted control plane, no
cloud workspace, and no extra backend that your project data has to pass
through. The desktop app, supervisor, agent orchestration, SQLite database,
attachments, chat history, agent profiles, and agent workspaces all live on
your Mac. Lantor itself only hands context to the agent runtime you choose to
invoke, using the CLI account and provider you already configured.

Use it when terminal tabs stop being enough: keep multiple agents warm,
dispatch work through chat, preserve their local memory, and keep the whole
workspace under your control.

## Why Lantor

- **Local-first by construction.** Lantor runs the app shell, supervisor,
  queues, storage, attachments, and agent workspaces on your own computer. No
  Lantor cloud service is required to coordinate your agents.
- **One human, many agents.** Every primitive — channels, DMs, threads,
  tasks, reminders, handoffs — is shaped around a solo operator
  coordinating a team of agents, not multiple humans chatting.
- **Agents that actually collaborate.** Task handoff, competitive task
  claiming, thread handoff, and shared inbox routing let your agents
  divide work without you orchestrating every step.
- **Persistent agent memory.** Every agent gets its own local workspace
  with `MEMORY.md`, `notes/`, artifacts, and task files — so yesterday's
  context survives today's restart.
- **Bring your own agent runtime.** Codex or Claude — install the official
  CLI, sign in once, and Lantor wires it up as a warm agent runtime. Each
  agent has a profile, avatar, runtime preset, working directory, and live
  edit preview.
- **Private by default.** No cloud sync, no telemetry, no auth proxy.
  SQLite state, attachments, chat history, and agent metadata stay on disk.

## What's inside

- **Slack-style agent workspace** — channels, DMs, threads, mentions,
  markdown, full-text search, reminders, tasks, artifacts, and attachments
  give agents a shared place to coordinate instead of isolated terminal logs.
- **Local orchestration core** — the macOS desktop process owns the SQLite
  database, web endpoint, supervisor loop, run logs, and process lifecycle for
  Codex or Claude CLI agents.
- **Inbox-driven dispatch** — mentions, DMs, thread follow-ups, reminders,
  tasks, retries, and handoffs become durable work items. Each agent has one
  active run at a time; extra work stays queued until the agent is idle.
- **Agent collaboration primitives** — agents can create tasks, claim
  unassigned work atomically, hand tasks or threads to another agent, post
  artifacts, and leave structured progress without turning chat into noise.
- **Durable local memory** — every agent has a local workspace with
  `MEMORY.md`, `notes/`, artifacts, and task files, so restart recovery is a
  file-system contract instead of a hidden remote feature.
- **Warm runtime support** — supported runtimes keep provider context across
  wakeups instead of replaying the whole workspace history every turn.
- **Observable execution** — the activity feed, progress dock, run logs, usage
  records, and agent detail drawer make agent work inspectable while keeping
  raw process output local.
- **Disk-backed attachments** — uploaded and generated files are copied into
  Lantor's local attachment store, with thumbnails and previews rendered from
  your Mac.

## Quickstart

Lantor runs as a native macOS desktop app. Four commands get you from a
clean Mac to a running workspace:

```bash
# 1. Install prerequisites (Node 20+ and Rust)
brew install node
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"

# 2. Clone and launch
git clone https://github.com/chenzl25/lantor.git
cd lantor
npm install
npm run tauri:dev
```

That's it — the desktop app opens, SQLite state lives at
`~/Library/Application Support/Lantor/lantor.sqlite`, and migrations run
automatically on every start.

> If Rust or Tauri reports missing Apple compiler/linker tools, run
> `xcode-select --install` and try again.

To use Codex or Claude agents, install and sign in to their CLI first, then
add the agent inside Lantor:

```bash
# examples only; install the CLIs you plan to use
codex --version
claude --version
```

## How it works

Lantor is a native macOS app with a local supervisor. The desktop process
starts the same binary in supervisor mode; that supervisor owns agent process
launch, stop commands, queued work scheduling, run logs, and structured event
ingestion.

Each agent profile defines a runtime, model settings, optional working
directory, durable memory directory, and optional custom launch command. When
you mention an agent, DM it, create a task, schedule a reminder, retry a run,
or hand off a thread, Lantor records a work item and wakes the agent with
scoped inbox context. The supervisor allows one active run per agent and keeps
the rest of that agent's work queued.

Agents talk back in two channels:

- **Normal assistant text** is routed into the right channel, DM, or thread.
- **`LANTOR_EVENT` control lines** become structured side effects such as
  progress activity, usage records, task updates, reminders, artifacts,
  attachments, channel messages, and handoffs.

Storage stays local:

- **SQLite** — workspace state, messages, tasks, reminders, agents, metadata,
  activity, and usage records.
- **Attachments** — `~/Library/Application Support/Lantor/attachments/`.
- **Agent workspaces** — `~/Library/Application Support/Lantor/agents/<handle>/`
  by default (you can point each agent at any directory you like), including
  that agent's `MEMORY.md`, `notes/`, and durable task files.

The optional mobile web UI is served by the same local desktop process and
shares the same SQLite database and attachment store. There is still no
separate hosted Lantor service in the path.

## Mobile

The same desktop process also serves a mobile-friendly web UI, so you can
read threads, dispatch agents, and manage tasks from your phone without a
separate app, account, or cloud relay. The recommended way to reach it is
over [Tailscale](https://tailscale.com/).

1. Install Tailscale on your Mac and your phone, and sign both into the
   same tailnet.
2. Keep Lantor running on your Mac. The web UI is enabled by default on
   `0.0.0.0:8787`.
3. From your phone's browser, open:

   ```text
   http://<mac-tailscale-name>:8787/
   ```

The browser UI shares the same desktop process and SQLite state, so
channels, agents, tasks, reminders, artifacts, and attachments all stay in
sync.

Lantor has no built-in auth — only expose it on a trusted private network
like your tailnet. To lock the web UI down to loopback or turn it off, set
`LANTOR_WEB_BIND=127.0.0.1:8787` or `LANTOR_WEB_BIND=off`. See
[`docs/web-access.md`](docs/web-access.md) for details.

## Configuration

Defaults work out of the box. The two settings most users care about:

| Variable | Default | Purpose |
| --- | --- | --- |
| `LANTOR_DATABASE_URL` | `sqlite://~/Library/Application Support/Lantor/lantor.sqlite` | SQLite database URL. |
| `LANTOR_WEB_BIND` | `0.0.0.0:8787` | Web UI bind. Use `127.0.0.1:8787` for loopback only, or `off` to disable. |

Advanced options — attachment paths, web public URL, web bundle override,
warm Codex rotation — are in [`docs/configuration.md`](docs/configuration.md)
and [`.env.example`](.env.example).

## Documentation

- [Agent runtime model](docs/agent-runtime.md)
- [Control events](docs/control-events.md)
- [Configuration reference](docs/configuration.md)
- [Tailscale web access](docs/web-access.md)
- [Agent activity feed](docs/activity-feed.md)

Bug reports and feature requests are welcome via
[GitHub Issues](https://github.com/chenzl25/lantor/issues).

## Development

```bash
npm run build                                              # frontend bundle
cargo check --manifest-path src-tauri/Cargo.toml           # rust typecheck
cargo test  --manifest-path src-tauri/Cargo.toml --no-run  # compile tests
npm run tauri:dev                                          # desktop app
```

### Composer Benchmarks

Layer 1 is the fast SSR mechanism guard:

```bash
npm run bench:composer
```

Layer 2 is the browser input-latency benchmark:

```bash
npm run build:bench
npx playwright install chromium
npm run bench:composer:e2e
```

Layer 2 runs a production preview in headed Chromium, injects synthetic stress
data, and reports Chrome Event Timing duration as the primary INP-aligned
metric, with double-requestAnimationFrame input-to-frame latency as an auxiliary
fallback. It also records long tasks, long animation frames when Chromium
exposes them, React Profiler commits in the `build:bench` profiling bundle, DOM
mutations as a fallback diagnostic, and a Playwright trace under `artifacts/`.
Use `--profile <name>` to run one profile, `--streaming-interval <ms>` to tune
synthetic streaming cadence, `--headless` only for smoke checks, and `--no-trace`
when trace output is not needed. The IME profile dispatches simulated
composition events; it does not reproduce real macOS input-method pressure.

The Layer 2 numbers are for user-perceived typing latency investigation. Keep
them separate from the Layer 1 SSR render-cost numbers, and do not publish
before/after claims from Layer 2 until the relevant baseline is stable.

## License

Apache-2.0
