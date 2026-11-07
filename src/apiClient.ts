import { convertFileSrc, invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";

import type {
  ApiArgs,
  ApiArgsTuple,
  ApiCommand,
  ApiResult,
} from "./api-contract";

import { subscribeWebEvents, type EventSubscription } from "./web-event-stream";
import { sessionFetch } from "./web-session";

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

/** Desktop only: writes generated text (a thread SVG export) into Downloads. */
export async function saveTextDownload(fileName: string, contents: string): Promise<string> {
  if (!isTauriRuntime()) {
    throw new Error("saveTextDownload is only available in the desktop app");
  }
  return tauriInvoke<string>("save_text_download", { fileName, contents });
}

/** Desktop only: shows a saved download in Finder with the file selected. */
export async function revealInFileManager(path: string): Promise<void> {
  if (!isTauriRuntime()) {
    throw new Error("revealInFileManager is only available in the desktop app");
  }
  await tauriInvoke("reveal_in_file_manager", { path });
}

export async function completeStartupSplash(): Promise<void> {
  if (!isTauriRuntime()) return;
  await tauriInvoke("complete_startup_splash");
}

function apiPath(command: string) {
  return `/api/${command}`;
}

/**
 * The desktop window's transport: the request travels over Tauri IPC as raw
 * bytes and the backend serves it with the same `/api` routes browsers use.
 */
async function desktopFetch(path: string, init: RequestInit = {}): Promise<Response> {
  const request = new Request(new URL(path, "http://lantor.desktop"), init);
  const body = new Uint8Array(await request.arrayBuffer());
  // Tauri describes the IPC body with its own Content-Type, so the API
  // request's content type travels in a header of its own.
  const headers: Record<string, string> = {
    "lantor-method": request.method,
    "lantor-path": path,
  };
  const contentType = request.headers.get("content-type");
  if (contentType) headers["lantor-content-type"] = contentType;
  try {
    // The IPC protocol returns an ArrayBuffer; its postMessage fallback
    // returns the same bytes as an array of numbers.
    const payload = await tauriInvoke<ArrayBuffer | number[]>("api_request", body, { headers });
    return new Response(Array.isArray(payload) ? new Uint8Array(payload) : payload, {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  } catch (error) {
    return new Response(JSON.stringify({ message: String(error) }), {
      status: 400,
      headers: { "content-type": "application/json" },
    });
  }
}

function apiFetch(path: string, init?: RequestInit): Promise<Response> {
  return isTauriRuntime() ? desktopFetch(path, init) : sessionFetch(path, init);
}

function requestApi(command: string, args: Record<string, unknown>): Promise<Response> {
  if (command === "bootstrap") return apiFetch(bootstrapApiPath(args));
  return apiFetch(apiPath(command), {
    method: "POST",
    headers: {
      "content-type": "application/json",
    },
    body: JSON.stringify(args),
  });
}

function bootstrapApiPath(args: Record<string, unknown>) {
  const channelId = typeof args.channelId === "string" ? args.channelId.trim() : "";
  const params = new URLSearchParams();
  if (args.currentChannelOnly === true) params.set("currentChannelOnly", "true");
  if (channelId) params.set("channelId", channelId);
  if (params.size === 0) return apiPath("bootstrap");
  return `${apiPath("bootstrap")}?${params.toString()}`;
}

async function readApiResponse<T>(response: Response, fallbackMessage: string): Promise<T> {
  const contentType = response.headers.get("content-type") || "";
  const payload = contentType.includes("application/json")
    ? await response.json()
    : await response.text();
  if (!response.ok) throw new Error(apiErrorMessage(payload, response.status, fallbackMessage));
  return payload as T;
}

/** A proxy or server error page is markup, not a message: never show it. */
export function apiErrorMessage(payload: unknown, status: number, fallbackMessage: string) {
  if (typeof payload === "object" && payload && "message" in payload) {
    return String((payload as { message: unknown }).message);
  }
  const text = typeof payload === "string" ? payload.trim() : "";
  if (!text || text.startsWith("<") || text.length > 300) return `${fallbackMessage} (HTTP ${status})`;
  return text;
}

export async function apiInvoke<C extends ApiCommand>(
  command: C,
  ...argsTuple: ApiArgsTuple<C>
): Promise<ApiResult<C>> {
  const args = (argsTuple[0] ?? {}) as Record<string, unknown>;
  const response = await requestApi(command, args);
  return readApiResponse<ApiResult<C>>(response, `${command} failed`);
}

type SendMessageArgs = Omit<ApiArgs<"send_message">, "attachments">;

export async function sendMessage(
  args: SendMessageArgs,
  attachments: readonly File[],
): Promise<ApiResult<"send_message">> {
  if (attachments.length === 0) {
    return apiInvoke("send_message", { ...args, attachments: [] });
  }

  const formData = new FormData();
  formData.append("request", JSON.stringify(args));
  for (const file of attachments) {
    formData.append("attachments", file, file.name || "attachment");
  }
  const response = await apiFetch(apiPath("send_message"), {
    method: "POST",
    body: formData,
  });
  return readApiResponse<ApiResult<"send_message">>(
    response,
    "send_message failed",
  );
}

export type ApiInvokeMeasurement = {
  roundtripMs: number;
  payloadBytes?: number;
  parseMs?: number;
};

export async function apiInvokeMeasured<C extends ApiCommand>(
  command: C,
  ...argsTuple: ApiArgsTuple<C>
): Promise<{ payload: ApiResult<C>; measurement: ApiInvokeMeasurement }> {
  const args = (argsTuple[0] ?? {}) as Record<string, unknown>;
  const startedAt = performance.now();
  const response = await requestApi(command, args);

  const contentType = response.headers.get("content-type") || "";
  const rawPayload = await response.text();
  const roundtripMs = performance.now() - startedAt;
  const payloadBytes = new TextEncoder().encode(rawPayload).length;
  const parseStartedAt = performance.now();
  const payload = contentType.includes("application/json")
    ? JSON.parse(rawPayload)
    : rawPayload;
  const parseMs = performance.now() - parseStartedAt;
  if (!response.ok) throw new Error(apiErrorMessage(payload, response.status, `${command} failed`));
  return {
    payload: payload as ApiResult<C>,
    measurement: {
      roundtripMs,
      payloadBytes,
      parseMs,
    },
  };
}

type BackendEventSubscriptionOptions = {
  cursor?: number | null;
  onCursor?: (cursor: number) => void;
};

type DesktopUiEventDelivery = {
  cursor: number;
  event: string;
};

function parseDesktopUiEventDeliveries(
  payload: unknown,
): DesktopUiEventDelivery[] | null {
  let parsed = payload;
  if (typeof parsed === "string") {
    try {
      parsed = JSON.parse(parsed);
    } catch {
      return null;
    }
  }
  if (
    !parsed
    || typeof parsed !== "object"
    || (parsed as { type?: unknown }).type !== "ui_event_delivery"
    || !Array.isArray((parsed as { events?: unknown }).events)
  ) {
    return null;
  }

  const deliveries: DesktopUiEventDelivery[] = [];
  for (const item of (parsed as { events: unknown[] }).events) {
    if (
      !item
      || typeof item !== "object"
      || !Number.isSafeInteger((item as { cursor?: unknown }).cursor)
      || ((item as { cursor: number }).cursor < 0)
      || typeof (item as { event?: unknown }).event !== "string"
    ) {
      return null;
    }
    deliveries.push(item as DesktopUiEventDelivery);
  }
  return deliveries;
}

export async function subscribeBackendEvents(
  handler: (payload: string) => void,
  options: BackendEventSubscriptionOptions = {},
): Promise<EventSubscription> {
  if (isTauriRuntime()) {
    const requestedCursor = options.cursor;
    const startingCursor =
      typeof requestedCursor === "number"
      && Number.isSafeInteger(requestedCursor)
      && requestedCursor >= 0
        ? requestedCursor
        : 0;
    let lastCursor = startingCursor;
    let replayComplete = false;
    let stopped = false;
    let reconciliation: Promise<void> | null = null;
    const pendingDeliveries = new Map<number, string>();
    const legacyPayloads: string[] = [];

    function deliver({ cursor, event }: DesktopUiEventDelivery) {
      if (stopped || cursor <= lastCursor) return;
      lastCursor = cursor;
      options.onCursor?.(cursor);
      handler(event);
    }

    const unlisten = await tauriListen<unknown>(UI_REFRESH_EVENT, (event) => {
      const deliveries = parseDesktopUiEventDeliveries(event.payload);
      if (!deliveries) {
        if (replayComplete && typeof event.payload === "string") {
          handler(event.payload);
        } else if (typeof event.payload === "string") {
          legacyPayloads.push(event.payload);
        }
        return;
      }
      if (!replayComplete) {
        for (const delivery of deliveries) {
          if (delivery.cursor > lastCursor) {
            pendingDeliveries.set(delivery.cursor, delivery.event);
          }
        }
        return;
      }
      for (const delivery of deliveries) deliver(delivery);
    });

    try {
      const replay = await apiInvoke("replay_ui_events", { cursor: startingCursor });
      if (replay.replayGap) {
        handler(JSON.stringify({
          type: "refresh",
          reason: "event_replay_gap",
        }));
      } else {
        for (const delivery of replay.events) deliver(delivery);
      }
      if (Number.isSafeInteger(replay.cursor) && (replay.replayGap || replay.cursor >= lastCursor)) {
        lastCursor = replay.cursor;
        options.onCursor?.(lastCursor);
      }
      replayComplete = true;
      for (const [cursor, event] of Array.from(pendingDeliveries.entries())
        .sort(([left], [right]) => left - right)) {
        deliver({ cursor, event });
      }
      // Compatibility for one hot-reload cycle against an older backend
      // emitter. The current cursor-aware backend never takes this path.
      for (const payload of legacyPayloads) handler(payload);
      return Object.assign(() => { stopped = true; unlisten(); }, {
        reconcile: () => {
          if (stopped) return Promise.resolve();
          if (reconciliation) return reconciliation;
          const requestedCursor = lastCursor;
          reconciliation = apiInvoke("replay_ui_events", { cursor: requestedCursor }).then((replay) => {
            if (stopped) return;
            if (replay.replayGap) {
              if (lastCursor !== requestedCursor) return;
              lastCursor = replay.cursor;
              options.onCursor?.(lastCursor);
              handler(JSON.stringify({ type: "refresh", reason: "event_replay_gap" }));
            } else {
              for (const delivery of replay.events) deliver(delivery);
            }
          }).finally(() => { reconciliation = null; });
          return reconciliation;
        },
      });
    } catch (err) {
      unlisten();
      throw err;
    }
  }

  return subscribeWebEvents(handler, options, (cursor) => apiInvoke("replay_ui_events", { cursor }));
}

export function attachmentAssetUrl(storagePath: string, attachmentId: string, thumbnail = false) {
  if (isTauriRuntime()) {
    return convertFileSrc(storagePath);
  }
  return `/api/attachments/${attachmentId}${thumbnail ? "?w=480" : ""}`;
}
