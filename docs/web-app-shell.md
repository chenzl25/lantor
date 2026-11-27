# Web app shell

Production Web builds generate `/sw.js` and a version marker in `index.html`.
The worker precaches the HTML plus Vite's emitted hashed JavaScript, CSS, fonts
and images after the initial page load. This uses a small build plugin instead
of a general runtime caching library, so the allowlist is explicit and testable.
Installation icons in the manifest and HTML use the public GitHub copies of
the checked-in PNGs. Home Screen icon fetches may not carry a Cloudflare Access
cookie; pointing them at a protected application origin can produce a monogram
instead of the logo. Keep `crossorigin="use-credentials"` on the manifest link
so the manifest itself can use the site's login cookie. Startup and in-app
images remain local, and the bundled startup logo is available offline.

Installation PNGs use a full-bleed opaque square background, allowing the
platform to apply its own corner mask without a baked-in rim. Their versioned
filenames avoid reusing the prior icon URL after an artwork update. The source
is `docs/assets/lantor-install-master-v2.png`; derive the 180/192/512px files
directly from that master with high-quality downsampling.

Registration requires HTTPS (including a Tailscale HTTPS origin) or localhost.
Plain HTTP on a LAN/Tailscale IP cannot install a service worker. Vite development
and Tauri never register it. See the browser's [secure-context registration
requirements](https://developer.mozilla.org/en-US/docs/Web/API/ServiceWorkerContainer/register).

## External links

Links in messages and code block headers are `<a target="_blank">`. Browsers
follow them natively; only the desktop app intercepts the click and opens the
URL through the system (`opensLinksNatively` in `src/apiClient.ts`). Do not
open them with `window.open` in browsers: in an iOS Home Screen web app, a
scripted window whose URL another app takes over (a universal link such as a
PropertyGuru listing or GitHub) stays behind as a blank page the user has to
close ([WebKit bug 199029](https://bugs.webkit.org/show_bug.cgi?id=199029)).
Snapshot links in a Home Screen app still open the attachment sheet
([attachment delivery](attachment-delivery.md)).

## On-screen keyboard

On iPhone the on-screen keyboard shrinks neither the layout viewport nor
`100dvh`; Safari pans the visual viewport so the focused field shows, which
pushed the phone layout's header off screen. `src/visual-viewport.ts` copies
the visual viewport into CSS variables on the root element:
`--visual-viewport-height`, `--visual-viewport-top` (Safari's pan) and
`--keyboard-inset` (the height the keyboard takes). At the phone breakpoint
the app, its panes, the thread and agent panels, full-screen panels and dialog
backdrops take that height and top, so the header stays at the top of what is
visible and the composer sits on the keyboard. A pinch zoom keeps the last
unzoomed values, and the document is kept scrolled to the top, because the
phone layout only scrolls inside its panes. The desktop app does not install
it. `npm run test:phone-controls` stands in for the keyboard by replacing
`window.visualViewport`, since Playwright engines show no on-screen keyboard.

## Resume copy on phones

iOS discards a Home Screen app's page while it sits in the background, and
returning to it reloads the page. The app installed on a phone's Home Screen
(standalone display mode, phone breakpoint, production build) therefore keeps
a copy of the workspace it showed last in IndexedDB (`src/resume-cache.ts`):

- **What it keeps:** every bootstrap collection, but messages only for the ten
  channels and ten threads opened most recently (a channel's newest 30
  top-level messages, a thread's root and newest 50 replies). Unsent messages
  and local file previews are left out. It also keeps the view (channel,
  thread, Home) and those channels' reading positions.
- **When it saves:** two seconds after the workspace or the view last
  changed, and at once when the page is hidden, since iOS may discard it then.
  Nothing is saved before the page's first bootstrap.
- **On reload:** the copy is drawn at once while the bootstrap is on its
  way. Saved within the hour, it also reopens the same channel, thread and
  reading position; a thread reopened this way slides in as on any opening.
  The first bootstrap replaces the copy. Because the event stream cannot
  report what changed while the page was gone, that bootstrap drops every
  message from the copy that neither it nor a page loaded since returned.
  Until then a failed bootstrap keeps retrying as on a first load.
- **Limits:** a copy older than seven days, or written by another build, is
  deleted unread. An expired session (a 401 from Cloudflare Access) deletes
  it, and takes it off the screen if no bootstrap has replaced it. A browser
  tab and the desktop app keep no copy.

WebKit exempts a Home Screen app's own storage from Safari's seven-day removal
of script-written storage, but the browser may still drop it under storage
pressure, so the copy is only a cache; the server stays authoritative.

## Cache and network behavior

- Only `/` and `/index.html` navigations use the cached shell; hash links continue
  to work normally. Emitted hashed assets are served from the versioned cache.
- `/api/*`, `/api/events` and `/attachments/*` bypass worker handling entirely,
  including direct navigations. Other origins, non-GET requests and Range
  requests also bypass it. There are no runtime cache writes.
- Workspace messages, credentials, attachments and API responses are never
  stored by this worker. An offline cold launch shows the Lantor shell and an
  offline notice, without workspace data, except in the phone's Home Screen
  app, which keeps its own copy of the last workspace
  ([resume copy](#resume-copy-on-phones)). Returning online retries the initial
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
