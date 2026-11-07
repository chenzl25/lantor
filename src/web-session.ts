import {
  isBrowserOffline, isProxyErrorResponse, isServerUnreachable, markServerReachable, markServerUnreachable,
  offlineMessage, serverUnreachableMessage,
} from "./web-reachability";

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
  let response: Response;
  try {
    response = await fetch(url, { ...init, headers: requestHeaders(init) });
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError") throw error;
    if (isBrowserOffline()) throw new Error(offlineMessage);
    markServerUnreachable();
    throw new Error(serverUnreachableMessage);
  }
  if (response.status === 401) {
    if (revision === startedAtRevision) setExpired(true);
    throw new Error(sessionExpiredMessage);
  }
  // Never show the proxy's error page as a message.
  if (isProxyErrorResponse(response)) {
    markServerUnreachable();
    throw new Error(serverUnreachableMessage);
  }
  markServerReachable();
  return response;
}

/** EventSource hides HTTP status. Probe a cheap, protected endpoint to tell
 * authentication expiry from network failures, 403 policy denials and 5xx,
 * and whether the Mac behind the proxy answers at all. */
export function checkWebSession(): Promise<void> {
  if (probe) return probe;
  if (Date.now() - lastProbeAt < 5_000 || isBrowserOffline()) return Promise.resolve();
  lastProbeAt = Date.now();
  const startedAtRevision = revision;
  probe = (async () => {
    try {
      const response = await fetch("/api/health", {
        headers: requestHeaders(), cache: "no-store", signal: AbortSignal.timeout(10_000),
      });
      if (isProxyErrorResponse(response)) markServerUnreachable();
      else if (response.headers.get("content-type")?.includes("application/json")) markServerReachable();
      if (revision !== startedAtRevision) return;
      if (response.status === 401) { setExpired(true); return; }
      if (!response.ok || !response.headers.get("content-type")?.includes("application/json")) return;
      const body = await response.json();
      if (revision === startedAtRevision && body?.ok === true) setExpired(false);
    } catch {
      // Offline, timeout or a CORS error is not proof of expiry. Online, a
      // request that never gets an answer means the Mac is out of reach.
      if (!isBrowserOffline()) markServerUnreachable();
    }
  })().finally(() => { probe = null; });
  return probe;
}

const SIGN_IN_MARKER = "lantor-auth";

export function webSignInUrl() {
  const url = new URL(window.location.href);
  url.pathname = "/";
  url.search = `?${SIGN_IN_MARKER}=1`;
  url.hash = "";
  return url.href;
}

/** Access returns to the sign-in URL. Drop its marker so later reloads use the
 * cached app shell again. */
export function clearWebSignInMarker() {
  const url = new URL(window.location.href);
  if (!url.searchParams.has(SIGN_IN_MARKER)) return;
  url.searchParams.delete(SIGN_IN_MARKER);
  window.history.replaceState(window.history.state, "", `${url.pathname}${url.search}${url.hash}`);
}

/** Keep the original page (including File objects and drafts) alive while the
 * user signs in in a new tab, or while the Mac is out of reach. Returning here
 * resumes sync without a reload. */
export function watchWebSessionRecovery() {
  const check = () => {
    if ((expired || isServerUnreachable()) && document.visibilityState === "visible") void checkWebSession();
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
