// Whether the Lantor server behind the web proxy answers. A phone reaches the
// Mac through a tunnel; while the Mac sleeps, the tunnel's edge answers with
// its own HTML error page, or the request fails outright.
let unreachable = false;
const listeners = new Set<() => void>();

export const serverUnreachableMessage = "Can’t reach your Mac. It may be asleep.";
export const offlineMessage = "You’re offline.";

export const isServerUnreachable = () => unreachable;

export function subscribeServerReachability(listener: () => void) {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
}

function setUnreachable(value: boolean) {
  if (unreachable === value) return;
  unreachable = value;
  for (const listener of listeners) listener();
}

export function markServerReachable() {
  setUnreachable(false);
}

export function markServerUnreachable() {
  setUnreachable(true);
}

/** Lantor answers errors with JSON. A 5xx page in any other format comes from
 * the proxy or tunnel in front of it. */
export function isProxyErrorResponse(response: Response) {
  return response.status >= 500 && !response.headers.get("content-type")?.includes("application/json");
}

export function isBrowserOffline() {
  return typeof navigator !== "undefined" && navigator.onLine === false;
}
