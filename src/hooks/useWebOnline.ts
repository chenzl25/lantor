import { useSyncExternalStore } from "react";
import { isTauriRuntime } from "../apiClient";

function subscribe(onChange: () => void) {
  window.addEventListener("online", onChange);
  window.addEventListener("offline", onChange);
  return () => {
    window.removeEventListener("online", onChange);
    window.removeEventListener("offline", onChange);
  };
}
const snapshot = () => isTauriRuntime() || navigator.onLine;

export function useWebOnline() {
  return useSyncExternalStore(subscribe, snapshot, () => true);
}
