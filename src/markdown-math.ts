import { fromMarkdown } from "mdast-util-from-markdown";

type MarkdownNode = {
  type: string;
  lang?: string | null;
  position?: { start: { offset?: number }; end: { offset?: number } };
  children?: MarkdownNode[];
};

// Keep the existing syntax: single-dollar math is disabled. Bare \(...\),
// \[...\] and \begin{...} were not parsed by remark-math and remain plain text.
export function hasMathMarkdown(body: string): boolean {
  if (!body.includes("$$") && !/[`~]{3}/.test(body)) return false;

  // Reuse the CommonMark parser already bundled by ReactMarkdown, but only for
  // candidates. An AST avoids mistaking code samples (including nested fences,
  // indented code and multi-backtick spans) for formulas. No math extension is
  // loaded here. Unclosed $$ blocks intentionally qualify during streaming.
  function visit(node: MarkdownNode): boolean {
    if (node.type === "code") return node.lang === "math";
    if (node.type === "text" && node.position) {
      const raw = body.slice(node.position.start.offset, node.position.end.offset);
      // Test source spelling: escaped/entity-encoded dollars are not delimiters.
      if (/(^|[^\\])(?:\\\\)*\$\$/.test(raw)) return true;
    }
    return node.children?.some(visit) ?? false;
  }
  return visit(fromMarkdown(body));
}
