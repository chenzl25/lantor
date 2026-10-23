// Remark plugin for message text: links GitHub references (`#123`, `rw#123`,
// `owner/repo#123`) and shows absolute UTC times (`2026-10-13T19:00:00+00:00`,
// `07:46Z`) in the viewer's local time. Only plain text is touched: code,
// links, HTML and image alt text are left alone. It also marks the first
// reference to each PR/issue in a message so the link can show its title.

type MdastNode = {
  type: string;
  value?: string;
  url?: string;
  title?: string | null;
  children?: MdastNode[];
  data?: Record<string, unknown>;
};

export type InlineEnrichmentOptions = {
  /** Returns the link for a reference, or `null` to leave the text as is. */
  resolveGithubReference?: (prefix: string | null, number: number) => string | null;
  /** Defaults to now; tests pin it. Decides whether the year is shown. */
  now?: Date;
  /** `owner/repo#123` for a GitHub PR/issue URL, else `null`; enables title marking. */
  githubReferenceKey?: (url: string) => string | null;
};

/** Set on links that should show the PR/issue title after the reference. */
export const REFERENCE_TITLE_PROPERTY = "data-reference-title";

type Replacement = {
  start: number;
  end: number;
  node: MdastNode;
};

const SKIPPED_PARENTS = new Set(["link", "linkReference", "inlineCode", "code", "html", "image", "imageReference", "definition"]);

// `(^|boundary)` instead of a lookbehind so older Safari keeps working.
const GITHUB_REFERENCE = /(^|[^A-Za-z0-9_/@.&#-])(?:([A-Za-z0-9][A-Za-z0-9_.-]*\/[A-Za-z0-9_.-]+|[A-Za-z][A-Za-z0-9_-]*)#|#)([0-9]+)(?![A-Za-z0-9_-])/g;
const ISO_TIMESTAMP = /(^|[^0-9A-Za-z:])([0-9]{4})-([0-9]{2})-([0-9]{2})[T ]([0-9]{2}):([0-9]{2})(?::([0-9]{2})(?:\.[0-9]+)?)?(Z|[+-][0-9]{2}:?[0-9]{2})(?![0-9A-Za-z])/g;
// `07:46Z`, optionally with a `10-14 ` month-day prefix.
const UTC_CLOCK = /(^|[^0-9A-Za-z:-])(?:([0-9]{1,2})-([0-9]{2}) )?([01][0-9]|2[0-3]):([0-5][0-9])Z(?![0-9A-Za-z])/g;

// Link text that is just a reference: `#123`, `rw#123` or `owner/repo#123`.
const REFERENCE_TEXT = /^(?:[A-Za-z0-9][A-Za-z0-9_.-]*\/[A-Za-z0-9_.-]+|[A-Za-z][A-Za-z0-9_-]*)?#[0-9]+$/;

const pad = (value: number) => String(value).padStart(2, "0");

function localDateTime(date: Date, now: Date) {
  const day = `${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
  const time = `${pad(date.getHours())}:${pad(date.getMinutes())}`;
  return date.getFullYear() === now.getFullYear()
    ? `${day} ${time}`
    : `${date.getFullYear()}-${day} ${time}`;
}

function utcLabel(date: Date) {
  return `${date.toISOString().slice(0, 16).replace("T", " ")} UTC`;
}

function timeNode(display: string, date: Date, title: string): MdastNode {
  return {
    type: "text",
    value: display,
    data: {
      hName: "time",
      hProperties: {
        dateTime: date.toISOString(),
        title,
        className: ["lantor-time"],
      },
    },
  };
}

function offsetMinutes(offset: string) {
  if (offset === "Z") return 0;
  const sign = offset.startsWith("-") ? -1 : 1;
  const digits = offset.slice(1).replace(":", "");
  return sign * (Number(digits.slice(0, 2)) * 60 + Number(digits.slice(2, 4)));
}

function isoReplacements(text: string, now: Date): Replacement[] {
  const replacements: Replacement[] = [];
  for (const match of text.matchAll(ISO_TIMESTAMP)) {
    const [whole, boundary, year, month, day, hour, minute, second, offset] = match;
    const utc = Date.UTC(Number(year), Number(month) - 1, Number(day), Number(hour), Number(minute), Number(second ?? 0))
      - offsetMinutes(offset) * 60_000;
    const date = new Date(utc);
    if (Number.isNaN(date.getTime()) || Number(month) > 12 || Number(day) > 31) continue;
    const start = match.index + boundary.length;
    const original = whole.slice(boundary.length);
    replacements.push({ start, end: start + original.length, node: timeNode(localDateTime(date, now), date, utcLabel(date)) });
  }
  return replacements;
}

function clockReplacements(text: string, now: Date): Replacement[] {
  const replacements: Replacement[] = [];
  for (const match of text.matchAll(UTC_CLOCK)) {
    const [whole, boundary, month, day, hour, minute] = match;
    const start = match.index + boundary.length;
    const original = whole.slice(boundary.length);
    if (month !== undefined) {
      const date = new Date(Date.UTC(now.getUTCFullYear(), Number(month) - 1, Number(day), Number(hour), Number(minute)));
      if (Number(month) < 1 || Number(month) > 12 || date.getUTCDate() !== Number(day)) continue;
      const display = `${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
      replacements.push({ start, end: start + original.length, node: timeNode(display, date, utcLabel(date)) });
      continue;
    }
    // A bare clock time has no date: convert it on today's UTC date and mark
    // a day change so `19:00Z` reads as `03:00 +1d` in UTC+8.
    const date = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate(), Number(hour), Number(minute)));
    const utcDay = Date.UTC(date.getUTCFullYear(), date.getUTCMonth(), date.getUTCDate());
    const localDay = Date.UTC(date.getFullYear(), date.getMonth(), date.getDate());
    const shift = Math.round((localDay - utcDay) / 86_400_000);
    const display = `${pad(date.getHours())}:${pad(date.getMinutes())}${shift ? ` ${shift > 0 ? "+" : ""}${shift}d` : ""}`;
    replacements.push({ start, end: start + original.length, node: timeNode(display, date, `${original.slice(0, -1)} UTC`) });
  }
  return replacements;
}

function referenceReplacements(
  text: string,
  resolve: NonNullable<InlineEnrichmentOptions["resolveGithubReference"]>,
): Replacement[] {
  const replacements: Replacement[] = [];
  for (const match of text.matchAll(GITHUB_REFERENCE)) {
    const [whole, boundary, prefix, digits] = match;
    // A bare `#7` is too likely to be prose; prefixed references can be short.
    if (prefix === undefined && digits.length < 2) continue;
    const url = resolve(prefix ?? null, Number(digits));
    if (!url) continue;
    const start = match.index + boundary.length;
    const label = whole.slice(boundary.length);
    replacements.push({
      start,
      end: start + label.length,
      node: { type: "link", url, title: null, children: [{ type: "text", value: label }] },
    });
  }
  return replacements;
}

function enrichText(text: string, options: InlineEnrichmentOptions, now: Date): MdastNode[] | null {
  const candidates = [
    ...isoReplacements(text, now),
    ...clockReplacements(text, now),
    ...(options.resolveGithubReference ? referenceReplacements(text, options.resolveGithubReference) : []),
  ].sort((left, right) => left.start - right.start || right.end - left.end);
  if (candidates.length === 0) return null;

  const nodes: MdastNode[] = [];
  let cursor = 0;
  for (const replacement of candidates) {
    if (replacement.start < cursor) continue;
    if (replacement.start > cursor) nodes.push({ type: "text", value: text.slice(cursor, replacement.start) });
    nodes.push(replacement.node);
    cursor = replacement.end;
  }
  if (cursor < text.length) nodes.push({ type: "text", value: text.slice(cursor) });
  return nodes;
}

function visit(node: MdastNode, options: InlineEnrichmentOptions, now: Date) {
  if (!node.children || SKIPPED_PARENTS.has(node.type)) return;
  const children: MdastNode[] = [];
  for (const child of node.children) {
    if (child.type === "text" && child.value && !child.data) {
      const enriched = enrichText(child.value, options, now);
      if (enriched) {
        children.push(...enriched);
        continue;
      }
    }
    visit(child, options, now);
    children.push(child);
  }
  node.children = children;
}

function nodeText(node: MdastNode): string {
  if (node.type === "text") return node.value ?? "";
  return node.children?.map(nodeText).join("") ?? "";
}

// Marks the first link to each PR/issue whose text is the bare URL or a bare
// reference; links with their own text, repeats and table cells are left as is.
function markReferenceTitles(node: MdastNode, keyOf: (url: string) => string | null, seen: Set<string>) {
  if (node.type === "tableCell") return;
  if (node.type === "link" && node.url) {
    const key = keyOf(node.url);
    const text = nodeText(node).trim();
    if (key && !seen.has(key) && (text === node.url || REFERENCE_TEXT.test(text))) {
      seen.add(key);
      const hProperties = (node.data?.hProperties ?? {}) as Record<string, unknown>;
      node.data = { ...node.data, hProperties: { ...hProperties, [REFERENCE_TITLE_PROPERTY]: "" } };
    }
    return;
  }
  for (const child of node.children ?? []) markReferenceTitles(child, keyOf, seen);
}

export function remarkInlineEnrichment(options: InlineEnrichmentOptions = {}) {
  return (tree: MdastNode) => {
    visit(tree, options, options.now ?? new Date());
    if (options.githubReferenceKey) markReferenceTitles(tree, options.githubReferenceKey, new Set());
  };
}
