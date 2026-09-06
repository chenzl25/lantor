import type { Style } from "@dicebear/core";

class LruCache<T> {
  private entries = new Map<string, T>();
  constructor(private readonly limit: number) {}
  get(key: string): T | undefined {
    const value = this.entries.get(key);
    if (value !== undefined) {
      this.entries.delete(key);
      this.entries.set(key, value);
    }
    return value;
  }
  set(key: string, value: T) {
    this.entries.delete(key);
    this.entries.set(key, value);
    if (this.entries.size > this.limit) this.entries.delete(this.entries.keys().next().value!);
  }
}

const DICEBEAR_STYLE_LOADERS = {
  adventurer: () => import("@dicebear/adventurer"),
  "bottts-neutral": () => import("@dicebear/bottts-neutral"),
  dylan: () => import("@dicebear/dylan"),
  identicon: () => import("@dicebear/identicon"),
  initials: () => import("@dicebear/initials"),
  lorelei: () => import("@dicebear/lorelei"),
  notionists: () => import("@dicebear/notionists"),
  personas: () => import("@dicebear/personas"),
  "pixel-art": () => import("@dicebear/pixel-art"),
  shapes: () => import("@dicebear/shapes"),
} as const;
type DiceBearStyleName = keyof typeof DICEBEAR_STYLE_LOADERS;
export type DiceBearSpec = { style: DiceBearStyleName; seed: string };
export const diceBearKey = ({ style, seed }: DiceBearSpec) => `${style}:${seed}`;

export function parseDiceBearAvatar(value: string, fallbackSeed: string): DiceBearSpec | null {
  const trimmed = value.trim();
  const lower = trimmed.toLowerCase();
  if (lower !== "dicebear" && !lower.startsWith("dicebear:")) return null;
  const [, rawStyle = "dylan", ...seedParts] = trimmed.split(":");
  const normalized = rawStyle.trim().replace(/([a-z0-9])([A-Z])/g, "$1-$2").replace(/[_\s]+/g, "-").toLowerCase();
  const style = Object.prototype.hasOwnProperty.call(DICEBEAR_STYLE_LOADERS, normalized) ? normalized as DiceBearStyleName : "dylan";
  return { style, seed: seedParts.join(":").trim() || fallbackSeed };
}

async function renderDiceBearAvatar({ style, seed }: DiceBearSpec) {
  const [{ createAvatar }, definition] = await Promise.all([import("@dicebear/core"), DICEBEAR_STYLE_LOADERS[style]()]);
  return createAvatar(definition as unknown as Style<Record<string, unknown>>, { seed }).toDataUri();
}

// In-flight work survives a row unmount, allowing its next mount and other rows
// to share it. Failed requests are removed so a later mount can retry.
export function createDiceBearCache(render = renderDiceBearAvatar, limit = 256) {
  const resolved = new LruCache<string>(limit);
  const pending = new Map<string, Promise<string>>();
  return {
    get(spec: DiceBearSpec) { return resolved.get(diceBearKey(spec)); },
    load(spec: DiceBearSpec): Promise<string> {
      const key = diceBearKey(spec);
      const cached = resolved.get(key);
      if (cached !== undefined) return Promise.resolve(cached);
      const existing = pending.get(key);
      if (existing) return existing;
      const request = Promise.resolve().then(() => render(spec)).then((uri) => {
        resolved.set(key, uri);
        return uri;
      }).finally(() => pending.delete(key));
      pending.set(key, request);
      return request;
    },
  };
}
export const diceBearAvatarCache = createDiceBearCache();

function hashSeed(seed: string) {
  let hash = 2166136261;
  for (let index = 0; index < seed.length; index += 1) {
    hash ^= seed.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  return hash >>> 0;
}

function nextSeed(seed: number) {
  let next = seed + 0x6d2b79f5;
  next = Math.imul(next ^ (next >>> 15), next | 1);
  next ^= next + Math.imul(next ^ (next >>> 7), next | 61);
  return (next ^ (next >>> 14)) >>> 0;
}

const IDENTICON_SIZE = 5;
const IDENTICON_MIRROR_WIDTH = Math.ceil(IDENTICON_SIZE / 2);
function generateIdenticon(seedText: string) {
  let seed = hashSeed(seedText);
  const cells = Array.from({ length: IDENTICON_SIZE * IDENTICON_SIZE }, () => false);
  for (let y = 0; y < IDENTICON_SIZE; y += 1) {
    for (let x = 0; x < IDENTICON_MIRROR_WIDTH; x += 1) {
      seed = nextSeed(seed);
      const isFilled = seed % 100 < 58;
      cells[y * IDENTICON_SIZE + x] = isFilled;
      cells[y * IDENTICON_SIZE + (IDENTICON_SIZE - 1 - x)] = isFilled;
    }
  }
  const hue = hashSeed(`${seedText}:color`) % 360;
  return { cells, foreground: `hsl(${hue} 68% 42%)`, background: `hsl(${hue} 36% 94%)` };
}

const identicons = new LruCache<ReturnType<typeof generateIdenticon>>(1024);
export function cachedIdenticon(seed: string) {
  let icon = identicons.get(seed);
  if (!icon) {
    icon = generateIdenticon(seed);
    identicons.set(seed, icon);
  }
  return icon;
}
