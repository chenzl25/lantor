import { convertFileSrc, invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen, type UnlistenFn } from "@tauri-apps/api/event";

const UI_REFRESH_EVENT = "lantor://refresh";

declare global {
  interface Window {
    __TAURI_INTERNALS__?: unknown;
  }
}

export function isTauriRuntime() {
  return typeof window !== "undefined" && Boolean(window.__TAURI_INTERNALS__);
}

export async function openExternalUrl(url: string): Promise<void> {
  if (isTauriRuntime()) {
    await tauriInvoke("open_external_url", { url });
    return;
  }
  window.open(url, "_blank", "noopener,noreferrer");
}

export async function downloadAttachment(storagePath: string, originalName: string): Promise<string> {
  if (!isTauriRuntime()) {
    throw new Error("downloadAttachment is only available in the desktop app");
  }
  return tauriInvoke<string>("download_attachment", { storagePath, originalName });
}

function apiPath(command: string) {
  return `/api/${command}`;
}

export async function apiInvoke<T>(command: string, args: Record<string, unknown> = {}): Promise<T> {
  if (isTauriRuntime()) {
    return tauriInvoke<T>(command, args);
  }

  const response = command === "bootstrap"
    ? await fetch(apiPath("bootstrap"))
    : await fetch(apiPath(command), {
      method: "POST",
      headers: {
        "content-type": "application/json",
      },
      body: JSON.stringify(args),
    });

  const contentType = response.headers.get("content-type") || "";
  const payload = contentType.includes("application/json")
    ? await response.json()
    : await response.text();
  if (!response.ok) {
    const message = typeof payload === "object" && payload && "message" in payload
      ? String((payload as { message: unknown }).message)
      : String(payload || `${command} failed`);
    throw new Error(message);
  }
  return payload as T;
}

export async function subscribeBackendEvents(handler: (payload: string) => void): Promise<UnlistenFn> {
  if (isTauriRuntime()) {
    return tauriListen<string>(UI_REFRESH_EVENT, (event) => handler(event.payload));
  }

  const source = new EventSource("/api/events");
  source.addEventListener("lantor", (event) => {
    handler((event as MessageEvent<string>).data);
  });
  source.onerror = () => {
    console.error("Lantor web event stream disconnected");
  };
  return () => source.close();
}

export function attachmentAssetUrl(storagePath: string, attachmentId: string) {
  if (isTauriRuntime()) {
    return convertFileSrc(storagePath);
  }
  return `/api/attachments/${attachmentId}`;
}
