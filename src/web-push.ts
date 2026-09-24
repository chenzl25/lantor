import { isTauriRuntime } from "./apiClient";

/**
 * Web Push for the mobile web app. The server announces new "needs you" items
 * (open decision cards, tasks moved to review); this module subscribes the
 * current browser and routes notification taps back into a conversation.
 */
export type PushState =
  | "unsupported" // Desktop app, dev server, insecure origin, or no Push API.
  | "install" // iOS Safari tab: push only exists in the Home Screen app.
  | "denied"
  | "off"
  | "on";

/** Where a notification tap should land. Snake case matches the server payload. */
export type PushTarget = {
  channel_id: string;
  thread_root_id: string | null;
  message_id: string;
};

type PushConfig = { public_key: string; subscription_count: number };
export type PushTestResult = { delivered: boolean; error: string | null };

export const OPEN_TARGET_MESSAGE = "LANTOR_OPEN_TARGET";
const OPEN_TARGET_HASH = "#/open/";

function isIos() {
  return /iPad|iPhone|iPod/.test(navigator.userAgent)
    || (navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1);
}

function isStandalone() {
  return window.matchMedia?.("(display-mode: standalone)").matches
    || (navigator as Navigator & { standalone?: boolean }).standalone === true;
}

function pushApiAvailable() {
  return !isTauriRuntime() && window.isSecureContext && "serviceWorker" in navigator
    && "PushManager" in window && "Notification" in window;
}

async function workerRegistration() {
  if (!pushApiAvailable()) return null;
  return (await navigator.serviceWorker.getRegistration("/")) ?? null;
}

async function pushApi<T>(path: string, body?: unknown): Promise<T> {
  const response = await fetch(`/api/push/${path}`, body === undefined ? undefined : {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  const payload = await response.json().catch(() => null) as { message?: unknown } | null;
  if (!response.ok) throw new Error(typeof payload?.message === "string" ? payload.message : `Notifications: ${path} failed`);
  return payload as T;
}

export function decodeBase64Url(value: string): Uint8Array<ArrayBuffer> {
  const base64 = value.replace(/-/g, "+").replace(/_/g, "/").padEnd(Math.ceil(value.length / 4) * 4, "=");
  const binary = atob(base64);
  const bytes = new Uint8Array(new ArrayBuffer(binary.length));
  for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index);
  return bytes;
}

function sameKey(current: ArrayBuffer | null | undefined, expected: Uint8Array) {
  if (!current || current.byteLength !== expected.byteLength) return false;
  const bytes = new Uint8Array(current);
  return bytes.every((byte, index) => byte === expected[index]);
}

function saveSubscription(subscription: PushSubscription) {
  return pushApi<{ ok: true }>("subscribe", { ...subscription.toJSON(), userAgent: navigator.userAgent });
}

export async function readPushState(): Promise<PushState> {
  if (isTauriRuntime()) return "unsupported";
  if (!pushApiAvailable()) return isIos() && !isStandalone() ? "install" : "unsupported";
  const registration = await workerRegistration();
  if (!registration) return "unsupported";
  if (Notification.permission === "denied") return "denied";
  const subscription = await registration.pushManager.getSubscription();
  return subscription && Notification.permission === "granted" ? "on" : "off";
}

/** Must be called from a tap: Safari only prompts inside a user gesture. */
export async function enablePush(): Promise<PushState> {
  if (!pushApiAvailable()) return "unsupported";
  // Ask first, before any other await can use up the gesture.
  const permission = await Notification.requestPermission();
  if (permission !== "granted") return permission === "denied" ? "denied" : "off";
  const registration = await workerRegistration();
  if (!registration) return "unsupported";
  const key = decodeBase64Url((await pushApi<PushConfig>("config")).public_key);
  let subscription = await registration.pushManager.getSubscription();
  if (subscription && !sameKey(subscription.options.applicationServerKey, key)) {
    await subscription.unsubscribe();
    subscription = null;
  }
  subscription ??= await registration.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: key });
  await saveSubscription(subscription);
  return "on";
}

export async function disablePush(): Promise<PushState> {
  const subscription = await (await workerRegistration())?.pushManager.getSubscription();
  if (subscription) {
    await pushApi("unsubscribe", { endpoint: subscription.endpoint }).catch(() => {});
    await subscription.unsubscribe();
  }
  return readPushState();
}

export async function sendTestPush(): Promise<PushTestResult> {
  const subscription = await (await workerRegistration())?.pushManager.getSubscription();
  if (!subscription) throw new Error("Notifications are off on this device");
  return pushApi<PushTestResult>("test", { endpoint: subscription.endpoint });
}

/**
 * Re-register an existing subscription on startup, so a reset database or a
 * rotated server key heals itself without the owner toggling anything.
 */
export async function syncPushSubscription(): Promise<void> {
  if (!pushApiAvailable() || Notification.permission !== "granted") return;
  const registration = await navigator.serviceWorker.ready;
  const subscription = await registration.pushManager.getSubscription();
  if (!subscription) return;
  const key = decodeBase64Url((await pushApi<PushConfig>("config")).public_key);
  if (sameKey(subscription.options.applicationServerKey, key)) {
    await saveSubscription(subscription);
    return;
  }
  await subscription.unsubscribe();
  await saveSubscription(await registration.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: key }));
}

/** Mirror the Needs-you count on the Home Screen icon where supported. */
export function setAppBadge(count: number) {
  if (isTauriRuntime() || !("setAppBadge" in navigator)) return;
  const update = count > 0 ? navigator.setAppBadge(count) : navigator.clearAppBadge();
  void update.catch(() => {});
}

export function isPushTarget(value: unknown): value is PushTarget {
  if (!value || typeof value !== "object") return false;
  const target = value as Record<string, unknown>;
  return typeof target.channel_id === "string" && typeof target.message_id === "string"
    && (target.thread_root_id === null || typeof target.thread_root_id === "string");
}

/** `#/open/<channel>/<thread root>/<message>`, used when a tap cold-starts the app. */
export function parseOpenTargetHash(hash: string): PushTarget | null {
  if (!hash.startsWith(OPEN_TARGET_HASH)) return null;
  const [channel, threadRoot, message] = hash.slice(OPEN_TARGET_HASH.length).split("/").map(decodeURIComponent);
  if (!channel || !message) return null;
  return { channel_id: channel, thread_root_id: threadRoot || null, message_id: message };
}

export function openTargetHash(target: PushTarget) {
  const parts = [target.channel_id, target.thread_root_id ?? "", target.message_id].map(encodeURIComponent);
  return `${OPEN_TARGET_HASH}${parts.join("/")}`;
}
