/// <reference types="vite/client" />
import { isTauriRuntime } from "./apiClient";

export function startAppShell(onUpdate: () => void) {
  const version = document.querySelector<HTMLMetaElement>('meta[name="lantor-shell-version"]')?.content;
  if (!import.meta.env.PROD || isTauriRuntime() || !window.isSecureContext
    || !["http:", "https:"].includes(location.protocol) || !("serviceWorker" in navigator) || !version) return () => {};

  const workers = navigator.serviceWorker;
  let registration: ServiceWorkerRegistration | undefined;
  let disposed = false;
  let lastUpdateCheck = 0;
  const pending = new Map<MessagePort, ReturnType<typeof setTimeout>>();
  function inspectController() {
    const controller = workers.controller;
    if (!controller || disposed) return;
    const channel = new MessageChannel();
    const close = () => { clearTimeout(pending.get(channel.port1)); pending.delete(channel.port1); channel.port1.close(); };
    pending.set(channel.port1, setTimeout(close, 1500));
    channel.port1.onmessage = (event) => {
      close();
      if (!disposed && typeof event.data?.version === "string" && event.data.version !== version) onUpdate();
    };
    try {
      controller.postMessage({ type: "LANTOR_SHELL_VERSION" }, [channel.port2]);
      controller.postMessage({ type: "LANTOR_SHELL_READY" });
    } catch { channel.port2.close(); close(); } // The controller may have just been replaced.
  }
  function identifyDocument(event: MessageEvent) {
    if (event.data?.type === "LANTOR_SHELL_CLIENT_VERSION") event.ports[0]?.postMessage({ version });
  }
  function onForeground() {
    if (document.visibilityState !== "visible" || !navigator.onLine) return;
    inspectController();
    if (!registration || Date.now() - lastUpdateCheck < 60_000) return;
    lastUpdateCheck = Date.now();
    void registration.update().catch(() => {}); // Keep the installed shell if offline/deploying.
  }
  async function register() {
    try {
      registration = await workers.register("/sw.js", { scope: "/", updateViaCache: "none" });
      if (!disposed) { lastUpdateCheck = Date.now(); inspectController(); }
    } catch (error) {
      console.warn("App shell cache unavailable", error);
    }
  }
  workers.addEventListener("controllerchange", inspectController);
  workers.addEventListener("message", identifyDocument);
  document.addEventListener("visibilitychange", onForeground);
  window.addEventListener("online", onForeground);
  // Precache in the background after startup resources finish loading.
  if (document.readyState === "complete") void register();
  else window.addEventListener("load", register, { once: true });
  inspectController();
  return () => {
    disposed = true;
    workers.removeEventListener("controllerchange", inspectController);
    workers.removeEventListener("message", identifyDocument);
    document.removeEventListener("visibilitychange", onForeground);
    window.removeEventListener("online", onForeground);
    window.removeEventListener("load", register);
    for (const [port, timer] of pending) { clearTimeout(timer); port.close(); }
    pending.clear();
  };
}
