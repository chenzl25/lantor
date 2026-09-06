import { useSyncExternalStore } from "react";

export type AgentReplyMode = "streaming" | "final";
export const AGENT_REPLY_MODE_KEY = "lantor.agentReplyMode";
const listeners = new Set<() => void>();
function storedMode(): AgentReplyMode {
  try { return window.localStorage.getItem(AGENT_REPLY_MODE_KEY) === "final" ? "final" : "streaming"; }
  catch { return "streaming"; }
}
let mode = storedMode();
const getSnapshot = () => mode;
const getServerSnapshot = (): AgentReplyMode => "streaming";
function notify() { listeners.forEach((listener) => listener()); }
function onStorage(event: StorageEvent) {
  if (event.key !== null && event.key !== AGENT_REPLY_MODE_KEY) return;
  mode = storedMode();
  notify();
}
function subscribe(listener: () => void) {
  if (!listeners.size) window.addEventListener("storage", onStorage);
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
    if (!listeners.size) window.removeEventListener("storage", onStorage);
  };
}
export function setAgentReplyMode(value: AgentReplyMode) {
  mode = value;
  try { window.localStorage.setItem(AGENT_REPLY_MODE_KEY, value); } catch { /* Session preference still works. */ }
  notify();
}
export function useAgentReplyMode() {
  return useSyncExternalStore(subscribe, getSnapshot, getServerSnapshot);
}
