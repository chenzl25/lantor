# Configuration

Lantor reads configuration from environment variables. All of them are
optional — the defaults work for local development. The most common ones
also appear in [`.env.example`](../.env.example).

## Core

| Variable | Default | Purpose |
| --- | --- | --- |
| `LANTOR_DATABASE_URL` | `sqlite://~/Library/Application Support/Lantor/lantor.sqlite` | SQLite database URL used by both the desktop process and the web server. `DATABASE_URL` is honored as a fallback only when it is also a `sqlite:` URL. |
| `LANTOR_WEB_BIND` | `127.0.0.1:8787` | Address the embedded web/SSE server binds to. Set to `off` / `none` to disable the browser UI; use a non-loopback address only on a trusted network. |

## Web UI

| Variable | Default | Purpose |
| --- | --- | --- |
| `LANTOR_WEB_PUBLIC_URL` | derived from `LANTOR_WEB_BIND` | Public base URL Lantor uses when generating links that point at itself. Set this to the HTTPS URL when using Tailscale Serve or an authenticated Cloudflare Tunnel. |
| `LANTOR_WEB_DIST` | auto-detected `dist/` next to the repo or current working directory | Override the static web bundle directory served by the desktop process. Useful when running a packaged build from a custom location. |

## Attachments

| Variable | Default | Purpose |
| --- | --- | --- |
| `LANTOR_ATTACHMENT_DIR` | `~/Library/Application Support/Lantor/attachments` | Disk location where Lantor copies inbound message attachments. Point this at an alternative volume if you want attachments stored outside `~/Library`. |

## Runtime tuning

| Variable | Default | Purpose |
| --- | --- | --- |
| `LANTOR_CLAUDE_CONTEXT_ROTATE_TOKENS` | `200000` | Start a new Claude Code session instead of resuming the stored one when the last turn's context (input plus cache read and write tokens) reached this many tokens. Values below `50000` are ignored and the default applies. |
| `LANTOR_CODEX_CONTEXT_ROTATE_INPUT_TOKENS` | `180000` | Rotate warm Codex sessions when the last stopped run exceeds this many input tokens. Lantor ignores values below `50000` to avoid churning sessions unnecessarily. |

## Notes

- Lantor does not perform its own browser auth. Keep the backend on loopback
  and use Tailscale Serve or a Cloudflare Tunnel protected by Cloudflare
  Access for remote use. See
  [Tailscale web access](web-access.md) for the recommended setup.
- Schema migrations run automatically the first time the desktop process
  connects to a fresh database — there is no separate migration step.
- Agent-local environment such as `LANTOR_AGENT_ID`, `LANTOR_RUN_ID`,
  `LANTOR_CONTEXT_TOOL`, and `LANTOR_WORK_ITEM_*` is injected into agent
  subprocesses by the supervisor and is not meant to be set by users.
