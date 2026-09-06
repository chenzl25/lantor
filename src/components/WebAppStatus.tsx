import { useEffect, useState } from "react";
import { RefreshCw, WifiOff } from "lucide-react";
import { isTauriRuntime } from "../apiClient";
import { useWebOnline } from "../hooks/useWebOnline";
import { startAppShell } from "../web-app-shell";

// A sibling of App: connection/update notices never invalidate message rows.
export function WebAppStatus() {
  const online = useWebOnline();
  const [updateAvailable, setUpdateAvailable] = useState(false);
  useEffect(() => startAppShell(() => setUpdateAvailable(true)), []);
  if (isTauriRuntime() || (online && !updateAvailable)) return null;
  return <aside className="web-app-status" role="status" aria-live="polite">
    {!online && <span><WifiOff size={16} aria-hidden="true" /> Offline — reconnect to sync your workspace.</span>}
    {updateAvailable && <span><RefreshCw size={16} aria-hidden="true" /><span>A new version is available.</span>
      <button type="button" onClick={() => window.location.reload()}>Refresh</button>
    </span>}
  </aside>;
}
