# Repository rules

These rules apply to every change in this repository, by people and by agents.
`CLAUDE.md` is a symlink to this file; edit `AGENTS.md` only. Setup and the
product overview are in the [README](README.md); commands and test suites are
in [docs/development.md](docs/development.md).

## Map

- `src-tauri/src/` is the Rust backend. One binary runs as the desktop app
  (Tauri), as the browser-only server (`--web-only`), and as the agent
  supervisor (`--supervisor`).
  - `application/` holds the operations behind the UI. `commands/` (Tauri) and
    `web.rs` (HTTP API and SSE) are thin adapters over it.
  - `runtime/` launches and supervises Claude and Codex processes and turns
    their output into messages; `prompts.rs` builds the agent prompts.
  - `agent_routing.rs` and `agent_work_dispatch.rs` turn messages into agent
    work items; `events/` applies `LANTOR_EVENT` control lines.
  - Module tests live in `src-tauri/src/tests/<module>.rs`, attached with
    `#[path]` from the module they test.
- `src/` is the React frontend, shared by the desktop window and browsers.
- `tests/` holds frontend unit tests (`*.test.ts`) and Playwright suites
  (`*.e2e.mjs`) that run the built `dist/` against a synthetic API.
- `docs/` explains how parts of the system work.

## Documentation

- Before changing an area, read its doc in `docs/`.
- A binding rule belongs in this file; an explanation of how something works
  belongs in `docs/`. Keep each rule in one place and link to it from the
  others.
- Update affected docs in the same pull request as the code. A doc you relied
  on that turned out to be stale is fixed in the same pull request.
- Keep this file under about 1,200 words. When it grows past that, move a
  topic into `docs/` and leave the one-line rule and a link here; do not
  compress rules into fragments.
- This repository is public. Never commit real messages, agent transcripts,
  personal hostnames, addresses or credentials; fixtures must be synthetic.

## Safety

- Every backend start (`npm run tauri:dev`, `npm run web:dev`,
  `npm run web:backend`, or the binary) also starts a supervisor that runs the
  agents configured in its database. Never start one against the real
  database to try a change. Prefer the synthetic `tests/*.e2e.mjs` harness;
  when a real backend is needed, point `LANTOR_DATABASE_URL` and
  `LANTOR_ATTACHMENT_DIR` at a fresh temporary location, never at a copy that
  contains agents or queued work, and set `LANTOR_WEB_BIND` to a free local
  address ([configuration](docs/configuration.md)).
- Run only one backend-owning process per database
  ([why](docs/development.md#frontend-hot-reload)).
- Tests never write to the live database or to agent workspaces. Read live data
  only with read-only access.

## Changing code

- Keep each pull request to one purpose. A refactor or migration pull request
  does not also fix bugs or change behavior; send those separately.
- Refactor only for real pressure: a feature that needs it, repeated review
  pain, unclear ownership, or logic leaking across layers.
- Keep dependencies pointing one way: adapters (`commands/`, `web.rs`) call
  `application/` and the stores, never the reverse. Keep `pub(crate)` exposure
  to what callers actually use.
- Agents working in parallel use separate git worktrees with disjoint write
  scopes. In a new worktree, symlink `node_modules` from the main checkout
  instead of reinstalling.
- A SQLite transaction that reads before it writes, and can run while
  another connection writes, begins with `BEGIN IMMEDIATE`. A deferred
  transaction can fail when it upgrades to a writer instead of waiting for the
  busy timeout.
- New colors use theme tokens ([docs/theme-tokens.md](docs/theme-tokens.md));
  `npm run lint:css-tokens` rejects raw color literals.
- A change to a `LANTOR_EVENT` type, a context tool or the agent prompt updates
  [docs/control-events.md](docs/control-events.md) or
  [docs/agent-runtime.md](docs/agent-runtime.md), `prompts.rs`, and its tests
  together.

## Checks

- Before pushing, run what CI runs (`.github/workflows/ci.yml`):
  `npm run lint:css-tokens`, `npm run build`, `npm test`,
  `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and
  `cargo test` (each cargo command with `--manifest-path src-tauri/Cargo.toml`),
  plus `git diff --check`. In a fresh worktree, CI's
  `--config 'profile.dev.package."*".opt-level=0'` override compiles the Rust
  checks much faster.
- For UI changes, run `npm run build` and then the matching `tests/*.e2e.mjs`
  suite; CI does not run them.
- A regression test must fail without the fix. Assert observable behavior,
  not source text or mock call counts. Prefer explicit signals and controlled
  clocks to real sleeps.
- In the pull request, say what you ran and what you did not run.

## Shipping

- Frontend changes reach browsers only after `npm run build`, because the web
  server serves `dist/` ([details](docs/web-app-shell.md)).
- Backend and prompt changes reach running agents only after the supervisor
  restarts. Say in the pull request whether a restart is needed.
- Commit subjects are short imperative sentences; pull requests are squash
  merged. Agent-authored commits end with a `Co-Authored-By:` trailer naming
  the model.
