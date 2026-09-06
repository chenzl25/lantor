// The build injects a version and integrity-checked, static allowlist here.
const { version, entries } = self.__LANTOR_APP_SHELL__;
const PREFIX = "lantor-shell-v1-";
const CACHE = PREFIX + version;
const paths = new Set(entries.map((entry) => entry.url));
const hashedAsset = /^\/assets\/[^/]+-[\w-]{8,}\.(?:js|css|woff2?|ttf|otf|png|svg|webp|avif|ico)$/;
const privatePath = (path) => /^(?:\/api|\/attachments)(?:\/|$)/.test(path);

self.addEventListener("install", (event) => {
  event.waitUntil((async () => {
    const cache = await caches.open(CACHE);
    try {
      // addAll is atomic; integrity rejects a partial deploy or SPA fallback
      // masquerading as a missing bundle. Leave the active version intact.
      await cache.addAll(entries.map(({ url, integrity }) => new Request(url, { integrity, cache: "reload", credentials: "same-origin" })));
      await self.skipWaiting();
    } catch (error) {
      await caches.delete(CACHE);
      throw error;
    }
  })());
});

function reportedVersion(target, type) {
  return new Promise((resolve) => {
    const channel = new MessageChannel();
    const finish = (value) => { clearTimeout(timer); channel.port1.close(); resolve(value); };
    const timer = setTimeout(() => finish(null), 1500);
    channel.port1.onmessage = (event) => finish(typeof event.data?.version === "string" ? event.data.version : null);
    try { target.postMessage({ type }, [channel.port2]); }
    catch { channel.port2.close(); finish(null); }
  });
}

let cleanup;
function cleanUnusedVersions() {
  if (cleanup) return cleanup;
  cleanup = (async () => {
    const active = self.registration.active;
    const updating = () => self.registration.installing || self.registration.waiting || self.registration.active !== active;
    // An old worker's foreground cleanup can overlap installation/activation
    // of its replacement. Only the current worker may remove retired caches.
    if (!active || updating() || await reportedVersion(active, "LANTOR_SHELL_VERSION") !== version) return;
    const clients = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
    const versions = await Promise.all(clients.map((client) => reportedVersion(client, "LANTOR_SHELL_CLIENT_VERSION")));
    // A suspended/starting tab may still need its old lazy chunks. Be
    // conservative until every live client can identify its loaded document.
    if (versions.includes(null)) return;
    const keep = new Set([CACHE, ...versions.map((value) => PREFIX + value)]);
    for (const name of await caches.keys()) {
      if (updating()) return;
      if (name.startsWith(PREFIX) && !keep.has(name)) await caches.delete(name);
    }
  })().catch(() => {}).finally(() => { cleanup = null; }); // Optional GC must not fail activation.
  return cleanup;
}

self.addEventListener("activate", (event) => {
  event.waitUntil((async () => {
    await self.clients.claim();
    await cleanUnusedVersions();
  })());
});

self.addEventListener("message", (event) => {
  if (event.data?.type === "LANTOR_SHELL_VERSION") event.ports[0]?.postMessage({ version });
  if (event.data?.type === "LANTOR_SHELL_READY") event.waitUntil(cleanUnusedVersions());
});

async function shellResponse(request, path) {
  const cache = await caches.open(CACHE);
  const key = request.mode === "navigate" ? "/index.html" : path;
  const current = await cache.match(key);
  if (current) return current;
  if (hashedAsset.test(path)) {
    // skipWaiting claims old pages immediately. Their previous hashed lazy
    // imports must remain usable until the user chooses to refresh.
    for (const name of await caches.keys()) {
      if (!name.startsWith(PREFIX) || name === CACHE) continue;
      const previous = await (await caches.open(name)).match(path);
      if (previous) return previous;
    }
  }
  return fetch(request); // No runtime writes, even when a static entry is absent.
}

self.addEventListener("fetch", (event) => {
  const request = event.request;
  const url = new URL(request.url);
  // Deliberately do not respondWith: API/SSE/uploads use the normal network
  // stack, including POSTs, direct navigations and streamed/Range responses.
  if (request.method !== "GET" || url.origin !== self.location.origin
    || privatePath(url.pathname) || request.headers.has("range")) return;
  const navigation = request.mode === "navigate" && (url.pathname === "/" || url.pathname === "/index.html");
  if (!navigation && !paths.has(url.pathname) && !hashedAsset.test(url.pathname)) return;
  event.respondWith(shellResponse(request, url.pathname));
});
