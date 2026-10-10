# Development

Setup and the first launch are in the [README](../README.md#quickstart). This
page covers the other ways to run Lantor while you work on it, the test
suites, and the README screenshots.

## Commands

```bash
npm run build                                              # frontend bundle
cargo check --manifest-path src-tauri/Cargo.toml           # rust typecheck
cargo test  --manifest-path src-tauri/Cargo.toml --no-run  # compile tests
npm test                                                   # frontend unit tests
npm run tauri:dev                                          # desktop app
npm run web:dev                                            # built web UI + backend, no desktop window
npm run web:backend                                        # backend only for Vite hot reload
npm run dev                                                # Vite frontend; proxies /api to web backend
```

## Browser-only mode

```bash
npm run web:dev
```

This builds the web bundle, starts the same local SQLite database, supervisor,
reminder worker, event pruning, and web server, then serves Lantor at
`http://127.0.0.1:8787/` by default. Set `LANTOR_WEB_BIND` only when you need
another bind address, or set it to `off` to disable browser access. Web-only
mode skips the Tauri window and the desktop-only event listener; browsers keep
using the web SSE stream.

During `npm run tauri:dev`, the desktop window uses Vite, while browser access
on port 8787 serves `dist/`. After pulling frontend updates, run `npm run build`
and refresh those browser pages to load the updates; restarting the desktop
development app alone does not rebuild the browser bundle.

## Frontend hot reload

For hot reload in browser-only development, run two terminals:

```bash
# Terminal 1: local backend and API/SSE server, no desktop window
npm run web:backend

# Terminal 2: Vite frontend with /api proxied to the backend above
npm run dev
```

Open `http://127.0.0.1:5173/` for the hot-reload UI. The Vite dev server
proxies `/api` and `/api/events` to `http://127.0.0.1:8787/` by default. If
the backend uses a non-default bind, set `LANTOR_WEB_BIND` in both terminals
or set `LANTOR_WEB_PROXY_TARGET=http://127.0.0.1:<port>` for the Vite terminal.

Do not run `npm run tauri:dev` and `npm run web:dev` / `npm run web:backend`
at the same time against the same SQLite database. Use one backend-owning
process at a time so the local supervisor and background workers have a single
owner.

## Web bundle

`npm run build` also generates gzip/Brotli sidecars for text assets. The web
server negotiates them via `Accept-Encoding` and keeps the original files for
other clients; API/SSE routes are not affected. Serve the complete `dist/`
directory, including the sidecars and local icon assets. Installation metadata
uses the public GitHub copies of the icons so Home Screen setup can fetch them
without the site's Cloudflare Access cookie; startup and in-app images stay
local. See [`web-app-shell.md`](web-app-shell.md) for cache behavior.

Math rendering is loaded on demand, including KaTeX CSS/fonts. Messages use the
existing `$$...$$` / `math` fenced-code syntax; single-dollar prices stay plain
text. While the math chunk loads (or if it fails), the message remains readable
as ordinary Markdown.

## Browser tests

The `tests/*.e2e.mjs` suites run the built app from `dist/` in Playwright
against a synthetic API, so they never read or change your workspace. Run
`npm run build` first; Playwright Chromium (and WebKit for some suites) must be
installed. Each suite has an npm script, for example:

- `npm run test:web-math` checks the math bundle graph and rendering.
- `npm run test:avatar-cache` checks avatar memoization, warm-remount first
  frames, stale asynchronous requests and formatter reuse with React's
  profiling build and test-only counters. It builds a synthetic fixture in a
  temporary directory, without touching the app's `dist/` or local database.
  Avatar results use bounded caches (256 DiceBear images and 1,024 identicons).
- `npm run test:ui-panels`, `test:dialogs`, `test:image-lightbox`,
  `test:attachment-sheet`, `test:activity`, `test:app-shell`, `test:streaming`,
  `test:web-sync`, `test:message-rows`, `test:message-state-races`,
  `test:channel-reading` and `test:mobile-performance` cover the matching UI
  areas.

Composer input latency benchmarks are described in
[`benchmarks.md`](benchmarks.md).

## README screenshots

The screenshots in the README come from a made-up demo workspace (a notes app
called Pebble), not from a real one:

```bash
npm run build
npm run screenshots:readme
```

[`scripts/readme-screenshots.mjs`](../scripts/readme-screenshots.mjs) serves
`dist/` with a fake API in the same way as the browser tests, pins the clock
and time zone, and writes the PNGs into `docs/assets/`. Rerun it after a UI
change and commit the images that changed. To write elsewhere or shoot only
some images:

```bash
node scripts/readme-screenshots.mjs dist /tmp/shots lantor-workspace,lantor-mobile-home
```

The demo data lives at the top of the script. When the app starts calling a new
API endpoint, the script answers it with `{ ok: true }` and lists it at the end
of the run; add a case to the fake API if a screen needs real data from it.
