import { useEffect, useState, useSyncExternalStore } from "react";
import { CloudOff, RefreshCw, WifiOff } from "lucide-react";
import { isTauriRuntime } from "../apiClient";
import { isStandaloneDisplay } from "../display-mode";
import { useWebOnline } from "../hooks/useWebOnline";
import { startAppShell } from "../web-app-shell";
import { isServerUnreachable, serverUnreachableMessage, subscribeServerReachability } from "../web-reachability";
import { isWebSessionExpired, subscribeWebSession, watchWebSessionRecovery, webSignInUrl } from "../web-session";

// A sibling of App: connection/update notices never invalidate message rows.
export function WebAppStatus() {
  const online = useWebOnline();
  const [updateAvailable, setUpdateAvailable] = useState(false);
  const sessionExpired = useSyncExternalStore(subscribeWebSession, isWebSessionExpired);
  const unreachable = useSyncExternalStore(subscribeServerReachability, isServerUnreachable);
  const macOutOfReach = online && unreachable && !sessionExpired;
  useEffect(() => startAppShell(() => setUpdateAvailable(true)), []);
  useEffect(() => isTauriRuntime() ? undefined : watchWebSessionRecovery(), []);
  if (isTauriRuntime() || (online && !updateAvailable && !sessionExpired && !macOutOfReach)) return null;
  return <aside className="web-app-status" role="status" aria-live="polite">
    {!online && <span><WifiOff size={16} aria-hidden="true" /> Offline. Reconnect to catch up.</span>}
    {macOutOfReach && <span><CloudOff size={16} aria-hidden="true" /> {serverUnreachableMessage}</span>}
    {sessionExpired && (isStandaloneDisplay() ? <div>
      {/* The Home Screen app has no tabs: sign-in replaces this page. */}
      <p>Your session has expired.</p>
      <button type="button" onClick={() => window.location.assign(webSignInUrl())}>Sign in again</button>
      <p>Signing in reloads Lantor. Copy any unsent text first.</p>
    </div> : <div>
      <p>Your session has expired. Your drafts are kept in this tab.</p>
      <button type="button" onClick={() => window.open(webSignInUrl(), "_blank", "noopener,noreferrer")}>Sign in again</button>
      <p>Sign in in the new tab, then return here to reconnect.</p>
    </div>)}
    {updateAvailable && !sessionExpired && <span><RefreshCw size={16} aria-hidden="true" /><span>New version ready.</span>
      <button type="button" onClick={() => window.location.reload()}>Refresh</button>
    </span>}
  </aside>;
}
