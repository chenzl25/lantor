// Reverse-proxy authentication (for example Cloudflare Access) lives outside
// Lantor. A fetch/SSE redirect cannot navigate the browser to its login page.
let expired = false;
let revision = 0;
const listeners = new Set<() => void>();
let probe: Promise<void> | null = null;
let lastProbeAt = -Infinity;

export const sessionExpiredMessage = "Your session has expired. Sign in again to reconnect.";
export const isWebSessionExpired = () => expired;
export function subscribeWebSession(listener: () => void) {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
}

function setExpired(value: boolean) {
  if (expired === value) return;
  expired = value;
  revision += 1;
  for (const listener of listeners) listener();
}

function requestHeaders(init?: RequestInit) {
  const headers = new Headers(init?.headers);
  // Access returns 401 instead of a cross-origin login redirect for AJAX.
  headers.set("X-Requested-With", "XMLHttpRequest");
  return headers;
}

/** For same-origin Lantor API calls only. Never replay a failed mutation. */
export async function sessionFetch(url: string, init?: RequestInit): Promise<Response> {
  if (expired) throw new Error(sessionExpiredMessage);
  const startedAtRevision = revision;
  const response = await fetch(url, { ...init, headers: requestHeaders(init) });
  if (response.status === 401) {
    if (revision === startedAtRevision) setExpired(true);
    throw new Error(sessionExpiredMessage);
  }
  return response;
}

/** EventSource hides HTTP status. Probe a cheap, protected endpoint to tell
 * authentication expiry from network failures, 403 policy denials and 5xx. */
export function checkWebSession(): Promise<void> {
  if (probe) return probe;
  if (Date.now() - lastProbeAt < 5_000 || (typeof navigator !== "undefined" && navigator.onLine === false)) return Promise.resolve();
  lastProbeAt = Date.now();
  const startedAtRevision = revision;
  probe = (async () => {
    try {
      const response = await fetch("/api/health", {
        headers: requestHeaders(), cache: "no-store", signal: AbortSignal.timeout(10_000),
      });
      if (revision !== startedAtRevision) return;
      if (response.status === 401) { setExpired(true); return; }
      if (!response.ok || !response.headers.get("content-type")?.includes("application/json")) return;
      const body = await response.json();
      if (revision === startedAtRevision && body?.ok === true) setExpired(false);
    } catch { /* Offline, timeout or a CORS error is not proof of expiry. */ }
  })().finally(() => { probe = null; });
  return probe;
}

export function webSignInUrl() {
  const url = new URL(window.location.href);
  url.pathname = "/";
  url.search = "?lantor-auth=1";
  url.hash = "";
  return url.href;
}

/** Keep the original page (including File objects and drafts) alive while the
 * user signs in in a new tab. Returning here resumes sync without a reload. */
export function watchWebSessionRecovery() {
  const check = () => {
    if (expired && document.visibilityState === "visible") void checkWebSession();
  };
  window.addEventListener("focus", check);
  window.addEventListener("pageshow", check);
  window.addEventListener("online", check);
  document.addEventListener("visibilitychange", check);
  const timer = window.setInterval(check, 5_000);
  return () => {
    window.clearInterval(timer);
    window.removeEventListener("focus", check);
    window.removeEventListener("pageshow", check);
    window.removeEventListener("online", check);
    document.removeEventListener("visibilitychange", check);
  };
}
