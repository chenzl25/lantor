import { useEffect, useState, useSyncExternalStore } from "react";
import { RefreshCw, WifiOff } from "lucide-react";
import { isTauriRuntime } from "../apiClient";
import { useWebOnline } from "../hooks/useWebOnline";
import { startAppShell } from "../web-app-shell";
import { isWebSessionExpired, subscribeWebSession, watchWebSessionRecovery, webSignInUrl } from "../web-session";

// A sibling of App: connection/update notices never invalidate message rows.
export function WebAppStatus() {
  const online = useWebOnline();
  const [updateAvailable, setUpdateAvailable] = useState(false);
  const sessionExpired = useSyncExternalStore(subscribeWebSession, isWebSessionExpired);
  useEffect(() => startAppShell(() => setUpdateAvailable(true)), []);
  useEffect(() => isTauriRuntime() ? undefined : watchWebSessionRecovery(), []);
  if (isTauriRuntime() || (online && !updateAvailable && !sessionExpired)) return null;
  return <aside className="web-app-status" role="status" aria-live="polite">
    {!online && <span><WifiOff size={16} aria-hidden="true" /> Offline — reconnect to sync your workspace.</span>}
    {sessionExpired && <div>
      <p>Your session has expired. Your drafts are kept in this tab.</p>
      <button type="button" onClick={() => window.open(webSignInUrl(), "_blank", "noopener,noreferrer")}>Sign in again</button>
      <p>Sign in in the new tab, then return here to reconnect.</p>
    </div>}
    {updateAvailable && !sessionExpired && <span><RefreshCw size={16} aria-hidden="true" /><span>A new version is available.</span>
      <button type="button" onClick={() => window.location.reload()}>Refresh</button>
    </span>}
  </aside>;
}
