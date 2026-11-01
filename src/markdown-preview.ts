// Plain-text previews of Markdown message bodies for lists, chips and search
// results: `**bold**`, backticks, `[text](url)` and fences become their text.
// Reuses the CommonMark/GFM parser that ReactMarkdown already bundles.
import { fromMarkdown } from "mdast-util-from-markdown";
import { gfmFromMarkdown } from "mdast-util-gfm";
import { gfm } from "micromark-extension-gfm";

type MarkdownNode = {
  type: string;
  value?: string;
  alt?: string | null;
  children?: MarkdownNode[];
};

// Previews show a few lines; parsing the whole of a long report is wasted work.
const PREVIEW_SOURCE_LIMIT = 2000;
const CACHE_LIMIT = 500;
const cache = new Map<string, string>();

const BLOCK_TYPES = new Set(["paragraph", "heading", "code", "html", "tableRow", "thematicBreak"]);
const SKIPPED_TYPES = new Set(["definition", "footnoteDefinition", "footnoteReference", "thematicBreak", "yaml"]);

function inlineText(node: MarkdownNode): string {
  switch (node.type) {
    case "text":
    case "inlineCode":
      return node.value ?? "";
    case "break":
      return " ";
    case "image":
    case "imageReference":
      return node.alt ?? "";
    case "html":
      return (node.value ?? "").replace(/<[^>]*>/g, "");
    default:
      return SKIPPED_TYPES.has(node.type) ? "" : node.children?.map(inlineText).join("") ?? "";
  }
}

function collectLines(node: MarkdownNode, lines: string[]) {
  if (SKIPPED_TYPES.has(node.type)) return;
  if (node.type === "code") {
    lines.push(...(node.value ?? "").split("\n"));
    return;
  }
  if (node.type === "tableRow") {
    lines.push(node.children?.map(inlineText).map((cell) => cell.trim()).join(" · ") ?? "");
    return;
  }
  if (BLOCK_TYPES.has(node.type)) {
    lines.push(inlineText(node));
    return;
  }
  node.children?.forEach((child) => collectLines(child, lines));
}

/**
 * Markdown → plain text with one line per block (paragraph, heading, list
 * item, table row, code line). Callers cut it with `firstLines`.
 */
export function markdownPreviewText(body: string): string {
  const source = body.length > PREVIEW_SOURCE_LIMIT ? body.slice(0, PREVIEW_SOURCE_LIMIT) : body;
  const cached = cache.get(source);
  if (cached !== undefined) return cached;

  let text: string;
  try {
    const tree = fromMarkdown(source, { extensions: [gfm()], mdastExtensions: [gfmFromMarkdown()] }) as MarkdownNode;
    const lines: string[] = [];
    collectLines(tree, lines);
    text = lines
      .flatMap((line) => line.split("\n"))
      .map((line) => line.replace(/[ \t]+/g, " ").trim())
      .filter(Boolean)
      .join("\n");
  } catch {
    text = source;
  }
  if (cache.size >= CACHE_LIMIT) cache.delete(cache.keys().next().value as string);
  cache.set(source, text);
  return text;
}
