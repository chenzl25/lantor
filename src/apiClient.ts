import { convertFileSrc, invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";

import type {
  ApiArgs,
  ApiArgsTuple,
  ApiCommand,
  ApiResult,
} from "./api-contract";

import { subscribeWebEvents, type EventSubscription } from "./web-event-stream";

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

export async function completeStartupSplash(): Promise<void> {
  if (!isTauriRuntime()) return;
  await tauriInvoke("complete_startup_splash");
}

function apiPath(command: string) {
  return `/api/${command}`;
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
  if (!response.ok) {
    const message = typeof payload === "object" && payload && "message" in payload
      ? String((payload as { message: unknown }).message)
      : String(payload || fallbackMessage);
    throw new Error(message);
  }
  return payload as T;
}

export async function apiInvoke<C extends ApiCommand>(
  command: C,
  ...argsTuple: ApiArgsTuple<C>
): Promise<ApiResult<C>> {
  const args = (argsTuple[0] ?? {}) as Record<string, unknown>;
  if (isTauriRuntime()) {
    return tauriInvoke<ApiResult<C>>(command, args);
  }

  const response = command === "bootstrap"
    ? await fetch(bootstrapApiPath(args))
    : await fetch(apiPath(command), {
      method: "POST",
      headers: {
        "content-type": "application/json",
      },
      body: JSON.stringify(args),
    });

  return readApiResponse<ApiResult<C>>(response, `${command} failed`);
}

type SendMessageArgs = Omit<ApiArgs<"send_message">, "attachments">;

export async function sendMessage(
  args: SendMessageArgs,
  attachments: readonly File[],
): Promise<ApiResult<"send_message">> {
  if (isTauriRuntime()) {
    const uploads = await Promise.all(
      attachments.map(async (file) => {
        const buffer = await file.arrayBuffer();
        return {
          originalName: file.name,
          mimeType: file.type || "application/octet-stream",
          bytes: Array.from(new Uint8Array(buffer)),
        };
      }),
    );
    return apiInvoke("send_message", { ...args, attachments: uploads });
  }

  if (attachments.length === 0) {
    return apiInvoke("send_message", { ...args, attachments: [] });
  }

  const formData = new FormData();
  formData.append("request", JSON.stringify(args));
  for (const file of attachments) {
    formData.append("attachments", file, file.name || "attachment");
  }
  const response = await fetch(apiPath("send_message"), {
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
  if (isTauriRuntime()) {
    const payload = await tauriInvoke<ApiResult<C>>(command, args);
    return {
      payload,
      measurement: {
        roundtripMs: performance.now() - startedAt,
      },
    };
  }

  const response = command === "bootstrap"
    ? await fetch(bootstrapApiPath(args))
    : await fetch(apiPath(command), {
      method: "POST",
      headers: {
        "content-type": "application/json",
      },
      body: JSON.stringify(args),
    });

  const contentType = response.headers.get("content-type") || "";
  const rawPayload = await response.text();
  const roundtripMs = performance.now() - startedAt;
  const payloadBytes = new TextEncoder().encode(rawPayload).length;
  const parseStartedAt = performance.now();
  const payload = contentType.includes("application/json")
    ? JSON.parse(rawPayload)
    : rawPayload;
  const parseMs = performance.now() - parseStartedAt;
  if (!response.ok) {
    const message = typeof payload === "object" && payload && "message" in payload
      ? String((payload as { message: unknown }).message)
      : String(payload || `${command} failed`);
    throw new Error(message);
  }
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

type DesktopUiEventReplay = {
  cursor: number;
  replayGap: boolean;
  events: DesktopUiEventDelivery[];
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
      const replay = await tauriInvoke<DesktopUiEventReplay>("replay_ui_events", {
        cursor: startingCursor,
      });
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

export function attachmentAssetUrl(storagePath: string, attachmentId: string) {
  if (isTauriRuntime()) {
    return convertFileSrc(storagePath);
  }
  return `/api/attachments/${attachmentId}`;
}
