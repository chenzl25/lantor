# Tailscale Web Access

Lantor exposes a browser-accessible web UI from the same desktop process so you
can open it locally or proxy it to another device. It listens only on
`127.0.0.1:8787` by default.

```bash
npm run build
npm run tauri:dev
```

Loopback requests from the same Mac can use:

```text
http://127.0.0.1:8787/
```

## Tailscale Serve

Install Tailscale on the Mac and the other device, sign both into the same
tailnet, then proxy Lantor's loopback listener:

```bash
tailscale serve --bg http://127.0.0.1:8787
tailscale serve status
```

Open the HTTPS URL reported by Tailscale from the other device:

```text
https://<mac-name>.<tailnet-name>.ts.net/
```

Tailscale Serve keeps the endpoint inside the tailnet; Lantor does not need to
listen on the Mac's LAN or Tailscale IP. Use tailnet grants or ACLs when access
should be narrower than the entire tailnet.

## Cloudflare Tunnel

A Cloudflare Tunnel running on the same Mac can also use
`http://127.0.0.1:8787` as its origin. Protect the hostname with Cloudflare
Access: a Tunnel without Access would expose Lantor's unauthenticated API to
the public internet.

## Bind overrides

To turn the web server off, set `LANTOR_WEB_BIND=off` (also accepts `none`,
`disabled`, `false`, or `0`). You can explicitly set a different address, but
non-loopback binds should only be used on a trusted network.

The web UI does not perform its own token check. Only expose Lantor on a
trusted private path such as loopback, Tailscale Serve, or a Cloudflare Tunnel
protected by Cloudflare Access.

The web UI uses HTTP endpoints under `/api/` for the subset of Tauri commands
the chat surface needs, including:

- bootstrap and runtime health checks
- sending messages, creating/updating/deleting channels and agents
- managing channel agent membership and saved messages
- inbox dismissal and read state, channel read state
- reminders (completing) and tasks (status, title, claim)
- cancelling and retrying agent work
- installing and uninstalling the supervisor LaunchAgent
- opening agent DMs
- reading artifacts and attachment preview
- agent workspace listing and file preview
- owner profile updates

Live refresh is delivered over an SSE stream at `/api/events`. Desktop Tauri
still uses native IPC for the same operations.

## Supervisor LaunchAgent

The Runtime panel can install a user LaunchAgent at:

```text
~/Library/LaunchAgents/local.lantor.supervisor.plist
```

That lets macOS keep the `--supervisor` process alive via `launchctl`.
Uninstall removes the plist and unloads the service.

## Push notifications

The web UI can send system notifications to a phone or browser when an agent
needs you: a new decision card, or a task moved to review. Open **Needs you**
and tap **Turn on** on the device that should get them; **Test** sends a sample
notification. Tapping a notification opens the conversation.

- Push needs HTTPS (Tailscale Serve or a Cloudflare Tunnel) and the production
  web build, because it runs in the app-shell service worker.
- On iPhone and iPad, Safari only offers push to the installed web app: use
  Share → **Add to Home Screen**, open Lantor from the Home Screen icon, then
  turn notifications on there. The icon badge shows the Needs-you count.
- The server keeps its VAPID key and subscriptions in the SQLite database and
  delivers through `curl`. It only accepts browser push services (Apple,
  Google, Mozilla, Microsoft) as endpoints. Set `LANTOR_PUSH_CONTACT` to a
  `mailto:` or `https:` contact if you want one other than the project URL.
- Items already waiting when the web process starts, or older than ten
  minutes, are not announced, so a restart never floods the phone.

## Expired proxy sessions

When Cloudflare Access expires a browser session, Lantor shows **Sign in again**
and pauses API calls and event-stream retries. Sign in in the new tab, then
return to the original tab: Lantor checks the session and resumes synchronization
without discarding unsent text or attachments. Failed mutations are not
automatically resubmitted. Keep the original tab open until you have sent or
saved its drafts.

The sign-in navigation uses `/?lantor-auth=1`, which bypasses the app-shell
service worker cache so the request reaches Access. API requests send
`X-Requested-With: XMLHttpRequest` so Access returns an explicit `401` on
expiry instead of a background login redirect. Event-stream failures trigger a
throttled check of `/api/health`; this endpoint must be protected by the same
Access policy as the rest of the application. Network failures, `403` policy
denials, and server errors are not treated as session expiry.

See [Cloudflare Access session management](https://developers.cloudflare.com/cloudflare-one/access-controls/access-settings/session-management/#ajax).
