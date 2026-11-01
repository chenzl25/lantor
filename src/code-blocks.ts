// Fence metadata and diff parsing for Markdown code blocks. Kept free of the
// highlighter so the eager bundle only pays for string handling.

// Every emitted chunk is precached by the web app shell, so heavy grammars are
// aliased to close relatives: C++ uses C (Shiki's C++ is ~800 KB), JSX uses TSX.
export const HIGHLIGHT_LANGUAGES = [
  "c", "css", "docker", "go", "html", "ini", "java", "javascript", "json", "kotlin",
  "make", "markdown", "proto", "python", "rust", "scala", "shellscript", "shellsession", "sql",
  "toml", "tsx", "typescript", "xml", "yaml",
] as const;
export type HighlightLanguage = typeof HIGHLIGHT_LANGUAGES[number];

/** Blocks longer than this collapse (to `--code-collapsed-lines` in CSS) once complete. */
export const CODE_BLOCK_COLLAPSE_LINES = 30;
/** Larger blocks stay plain so a pasted log cannot stall rendering. */
export const MAX_HIGHLIGHT_CHARS = 60_000;

const LANGUAGE_ALIASES: Record<string, HighlightLanguage> = {
  bash: "shellscript", c: "c", "c++": "c", cc: "c", cjs: "javascript", console: "shellsession",
  cpp: "c", css: "css", cts: "typescript", cxx: "c", docker: "docker", dockerfile: "docker",
  go: "go", golang: "go", h: "c", hpp: "c", htm: "html", html: "html", ini: "ini", java: "java",
  javascript: "javascript", js: "javascript", json: "json", jsonc: "json", jsx: "tsx", kotlin: "kotlin",
  kt: "kotlin", make: "make", makefile: "make", markdown: "markdown", md: "markdown", mjs: "javascript",
  mts: "typescript", pgsql: "sql", postgres: "sql", postgresql: "sql", proto: "proto", protobuf: "proto",
  py: "python", python: "python", rs: "rust", rust: "rust", scala: "scala", sh: "shellscript",
  shell: "shellscript", shellscript: "shellscript", shellsession: "shellsession", sql: "sql",
  toml: "toml", ts: "typescript", tsx: "tsx", typescript: "typescript", xml: "xml", yaml: "yaml",
  yml: "yaml", zsh: "shellscript",
};

export function resolveHighlightLanguage(name: string | null | undefined): HighlightLanguage | null {
  if (!name) return null;
  return LANGUAGE_ALIASES[name.toLowerCase()] ?? null;
}

export function languageFromPath(path: string | null | undefined): HighlightLanguage | null {
  if (!path) return null;
  const file = path.replace(/[?#].*$/, "").split("/").pop() ?? "";
  if (/^dockerfile$/i.test(file)) return "docker";
  if (/^(gnu)?makefile$/i.test(file)) return "make";
  const extension = file.includes(".") ? file.split(".").pop() : null;
  return resolveHighlightLanguage(extension);
}

export type CodeFence = {
  /** Language used for syntax highlighting (inside the diff for diff fences). */
  language: HighlightLanguage | null;
  /** Fence language as written, shown in the header. */
  label: string | null;
  diff: boolean;
  /** Header text for `path=` / `title=`. */
  title: string | null;
  /** External link for GitHub (or other http) permalinks. */
  href: string | null;
  startLine: number | null;
  endLine: number | null;
};

const META_TOKEN = /([A-Za-z][\w-]*)=(?:"([^"]*)"|'([^']*)'|(\S+))|"([^"]*)"|'([^']*)'|(\S+)/g;

function parseMeta(meta: string) {
  const attributes = new Map<string, string>();
  const words: string[] = [];
  for (const match of meta.matchAll(META_TOKEN)) {
    if (match[1]) attributes.set(match[1].toLowerCase(), match[2] ?? match[3] ?? match[4] ?? "");
    else words.push(match[5] ?? match[6] ?? match[7] ?? "");
  }
  return { attributes, words };
}

function lineNumber(value: string | undefined) {
  if (!value) return null;
  const number = Number(value);
  return Number.isSafeInteger(number) && number > 0 ? number : null;
}

const LINE_ANCHOR = /(?:#L(\d+)(?:-L?(\d+))?|:(\d+)(?:-(\d+))?)$/;
const GITHUB_BLOB = /^https?:\/\/github\.com\/[^/]+\/[^/]+\/(?:blob|tree)\/[^/]+\/([^#?]+)/;

function parseLocation(value: string) {
  const anchor = value.match(LINE_ANCHOR);
  const startLine = lineNumber(anchor?.[1] ?? anchor?.[3]);
  const endLine = lineNumber(anchor?.[2] ?? anchor?.[4]);
  if (/^https?:\/\//i.test(value)) {
    let title = value.replace(/^https?:\/\//i, "").replace(/[?#].*$/, "");
    const blob = value.match(GITHUB_BLOB);
    if (blob) {
      try {
        title = decodeURIComponent(blob[1]);
      } catch {
        title = blob[1];
      }
    }
    // Only `#L…` is a line anchor in a URL; `:123` would be a port or path.
    const urlAnchor = anchor?.[1] ? { startLine, endLine } : { startLine: null, endLine: null };
    return { title, href: value, ...urlAnchor };
  }
  const title = anchor && startLine ? value.slice(0, anchor.index) : value;
  return { title, href: null, startLine: anchor && startLine ? startLine : null, endLine: anchor && startLine ? endLine : null };
}

/**
 * Reads the CommonMark info string: ```rust path=src/lib.rs#L42 or
 * ```diff rust path=https://github.com/o/r/blob/sha/src/lib.rs.
 */
export function parseCodeFence(lang: string | null | undefined, meta: string | null | undefined): CodeFence {
  const { attributes, words } = parseMeta(meta ?? "");
  const fenceLanguage = lang?.trim() || null;
  const diff = fenceLanguage?.toLowerCase() === "diff" || fenceLanguage?.toLowerCase() === "patch";
  const path = attributes.get("path") ?? attributes.get("file") ?? null;
  const location = path ? parseLocation(path) : null;
  const title = location?.title || attributes.get("title") || null;
  const start = lineNumber(attributes.get("start")) ?? location?.startLine ?? null;
  const pathLanguage = languageFromPath(location?.title);
  const language = diff
    ? resolveHighlightLanguage(words[0]) ?? pathLanguage
    : resolveHighlightLanguage(fenceLanguage) ?? (fenceLanguage ? null : pathLanguage);
  const label = diff && words[0] ? `${fenceLanguage} · ${words[0]}` : fenceLanguage;
  return {
    language,
    label,
    diff,
    title,
    href: location?.href ?? null,
    startLine: start,
    endLine: start !== null && location?.endLine && location.endLine >= start ? location.endLine : null,
  };
}

export type DiffLineKind = "add" | "del" | "context" | "hunk" | "meta";
export type DiffLine = {
  kind: DiffLineKind;
  /** `+`, `-` or ` ` for body lines; empty for headers. */
  marker: string;
  /** Line text without its marker; the whole line for headers. */
  content: string;
  oldLine: number | null;
  newLine: number | null;
};

const HUNK_HEADER = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/;
const FILE_HEADER = /^(?:diff |index |new file mode|deleted file mode|old mode|new mode|similarity index|rename from|rename to|Binary files )/;

/**
 * Classifies unified-diff lines. Counts from real hunk headers disambiguate
 * `--- ` deletions from file headers; hand-written hunks without counts stay
 * readable by falling back to the leading marker.
 */
export function parseDiffLines(code: string): { lines: DiffLine[]; numbered: boolean } {
  const source = code.split("\n");
  const lines: DiffLine[] = [];
  let oldLine: number | null = null;
  let newLine: number | null = null;
  let oldLeft = 0;
  let newLeft = 0;
  let numbered = false;
  for (let index = 0; index < source.length; index += 1) {
    const text = source[index];
    const hunk = text.match(HUNK_HEADER);
    if (hunk) {
      oldLine = Number(hunk[1]);
      newLine = Number(hunk[3]);
      oldLeft = hunk[2] === undefined ? 1 : Number(hunk[2]);
      newLeft = hunk[4] === undefined ? 1 : Number(hunk[4]);
      numbered = true;
      lines.push({ kind: "hunk", marker: "", content: text, oldLine: null, newLine: null });
      continue;
    }
    if (text.startsWith("@@")) {
      oldLine = newLine = null;
      oldLeft = newLeft = 0;
      lines.push({ kind: "hunk", marker: "", content: text, oldLine: null, newLine: null });
      continue;
    }
    const insideCountedHunk = oldLeft > 0 || newLeft > 0;
    const fileHeader = !insideCountedHunk && (
      FILE_HEADER.test(text)
      || (text.startsWith("--- ") && (source[index + 1] ?? "").startsWith("+++ "))
      || (text.startsWith("+++ ") && (source[index - 1] ?? "").startsWith("--- "))
    );
    if (fileHeader || text.startsWith("\\ ")) {
      lines.push({ kind: "meta", marker: "", content: text, oldLine: null, newLine: null });
      continue;
    }
    const marker = text[0];
    if (marker === "+") {
      lines.push({ kind: "add", marker, content: text.slice(1), oldLine: null, newLine });
      if (newLine !== null) newLine += 1;
      newLeft = Math.max(0, newLeft - 1);
    } else if (marker === "-") {
      lines.push({ kind: "del", marker, content: text.slice(1), oldLine, newLine: null });
      if (oldLine !== null) oldLine += 1;
      oldLeft = Math.max(0, oldLeft - 1);
    } else {
      // Hand-written diffs often drop the leading space on unchanged lines.
      const content = marker === " " ? text.slice(1) : text;
      lines.push({ kind: "context", marker: " ", content, oldLine, newLine });
      if (oldLine !== null) oldLine += 1;
      if (newLine !== null) newLine += 1;
      oldLeft = Math.max(0, oldLeft - 1);
      newLeft = Math.max(0, newLeft - 1);
    }
  }
  return { lines, numbered };
}
