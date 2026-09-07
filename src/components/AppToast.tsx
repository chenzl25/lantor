import { useSyncExternalStore } from "react";
import { createPortal } from "react-dom";
import { activeDialogPanel, subscribeDialogLayers } from "../dialog-layers";

export function AppToast({ message, kind = "error", onDismiss, className = "" }: {
  message: string; kind?: "error" | "success"; onDismiss: () => void; className?: string;
}) {
  const dialog = useSyncExternalStore(subscribeDialogLayers, activeDialogPanel, () => null);
  return createPortal(<div className={`app-toast ${kind} ${className}`} role={kind === "error" ? "alert" : "status"}>
    <span>{message}</span><button type="button" onClick={onDismiss} aria-label="Dismiss notification">Dismiss</button>
  </div>, dialog ?? document.body);
}
