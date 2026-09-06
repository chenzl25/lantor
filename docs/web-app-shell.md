# Web app shell

Production Web builds generate `/sw.js` and a version marker in `index.html`.
The worker precaches the HTML plus Vite's emitted hashed JavaScript, CSS, fonts
and images after the initial page load. This uses a small build plugin instead
of a general runtime caching library, so the allowlist is explicit and testable.
The existing PWA manifest and local installation icons remain unchanged.

Registration requires HTTPS (including a Tailscale HTTPS origin) or localhost.
Plain HTTP on a LAN/Tailscale IP cannot install a service worker. Vite development
and Tauri never register it. See the browser's [secure-context registration
requirements](https://developer.mozilla.org/en-US/docs/Web/API/ServiceWorkerContainer/register).

## Cache and network behavior

- Only `/` and `/index.html` navigations use the cached shell; hash links continue
  to work normally. Emitted hashed assets are served from the versioned cache.
- `/api/*`, `/api/events` and `/attachments/*` bypass worker handling entirely,
  including direct navigations. Other origins, non-GET requests and Range
  requests also bypass it. There are no runtime cache writes.
- Workspace messages, credentials, attachments and API responses are never
  stored by this worker. An offline cold launch shows the Lantor shell and an
  offline notice, without workspace data. Returning online retries the initial
  bootstrap; an already loaded workspace retains its existing SSE reconnect
  and cursor replay behavior.
- Source maps, compression sidecars and unversioned public files are excluded.
  The existing HTTP cache still applies independently of CacheStorage.

## Deployment and refresh

Run `npm run build` and deploy the complete `dist/` directory together. The
version covers the HTML, worker code and asset contents. Each precache request
has SHA-256 integrity, so a partial deployment or missing chunk served as HTML
rejects installation and leaves the prior worker intact. Compression sidecars
are generated after the versioned HTML and worker are written.

The current backend already sends `no-cache` for `sw.js`/HTML and immutable
caching for hashed assets. Registration uses `updateViaCache: "none"`; navigation
checks for updates, and foreground/online checks are throttled to once a minute.
There is no additional polling timer.

Successful installation calls `skipWaiting`, then `clients.claim`. A loaded
document compares its build marker with the new controller and offers **Refresh**
only when they differ. First installation has no false update notice. Refresh
is always a user action: worker activation does not reload a page or interrupt
its draft or stream.

Because a new controller can control an older document, caches still needed by
live tabs are retained for lazy imports. Nonresponsive/frozen tabs conservatively
prevent cleanup. Once clients report their loaded versions, unused caches are
removed. Cleanup checks the active worker and any ongoing installation before
deleting caches, and never deletes the cache a replacement is installing.
Storage eviction can still require a network load; this is a cached application
shell, not an offline workspace database. The lifecycle rationale is described
in the [service worker lifecycle guide](https://web.dev/articles/service-worker-lifecycle).

## Verification

`npm test` checks generated allowlists, integrity and deterministic versioning.
`npm run test:app-shell` builds two real production deployments and exercises
Chromium's ServiceWorker, CacheStorage and EventSource against a synthetic HTTP
server. It checks offline open/reload and recovery, API/SSE/attachment bypass,
failed-install rollback, concurrent old-worker cleanup, update notification,
preserved drafts/streaming/old lazy assets, explicit refresh and old cache
cleanup, plus Tauri and Vite development exclusion. No user workspace is read.

Set `SHELL_SCREENSHOT=/absolute/path/prefix` to save offline mobile and update
screenshots. `SHELL_BROWSER=webkit npm run test:app-shell` runs the same lifecycle
checks in WebKit. Its automation offline mode fails before worker navigation
(also reproduced with a constant-response worker), so this variant disconnects
the HTTP server and supplies the online/offline UI signal instead. Chromium
uses the browser's complete offline emulation.

Run the normal build, frontend and streaming/sync E2E checks as
well: shell caching must not change message rendering or event synchronization.
