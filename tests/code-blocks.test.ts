import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { HIGHLIGHT_LANGUAGES, parseCodeFence, parseDiffLines } from "../src/code-blocks";
import { loadHighlightLanguage, tokenizeLoaded } from "../src/code-highlighter";
import { MarkdownRenderer } from "../src/components/MarkdownRenderer";

test("fence metadata yields header, line anchors and the highlight language", () => {
  assert.deepEqual(parseCodeFence("rust", "path=core/src/strategy.rs#L922"), {
    language: "rust", label: "rust", diff: false, title: "core/src/strategy.rs", href: null, startLine: 922, endLine: null,
  });
  assert.deepEqual(
    [parseCodeFence("ts", "path=src/a.ts:5-9"), parseCodeFence("rs", "path=src/lib.rs#L10-L20")]
      .map(({ language, title, startLine, endLine }) => [language, title, startLine, endLine]),
    [["typescript", "src/a.ts", 5, 9], ["rust", "src/lib.rs", 10, 20]],
  );
  const permalink = "https://github.com/nimtable/iceberg-compaction/blob/d29d6fa/core/src/file_selection/benefit.rs#L80-L106";
  assert.deepEqual(parseCodeFence("rust", `path=${permalink}`), {
    language: "rust", label: "rust", diff: false, title: "core/src/file_selection/benefit.rs", href: permalink, startLine: 80, endLine: 106,
  });
  const quoted = parseCodeFence("rust", 'title="Plan strategy" start=40');
  assert.deepEqual([quoted.title, quoted.startLine, quoted.href], ["Plan strategy", 40, null]);
  assert.equal(parseCodeFence("text", null).language, null);
  assert.equal(parseCodeFence(null, null).label, null);
  assert.equal(parseCodeFence("sh", "path=http://127.0.0.1:8787/x").startLine, null, "a port is not a line anchor");
  // Diff fences highlight the inner language, written or inferred from the path.
  assert.deepEqual(
    [parseCodeFence("diff", "rust path=src/a.rs"), parseCodeFence("diff", "path=src/a.tsx"), parseCodeFence("patch", null)]
      .map(({ diff, language, label }) => [diff, language, label]),
    [[true, "rust", "diff · rust"], [true, "tsx", "diff"], [true, null, "patch"]],
  );
});

test("unified diffs keep line numbers and do not mistake deletions for file headers", () => {
  const { lines, numbered } = parseDiffLines([
    "diff --git a/q.sql b/q.sql",
    "index 1111111..2222222 100644",
    "--- a/q.sql",
    "+++ b/q.sql",
    "@@ -10,3 +10,3 @@ select",
    " select 1;",
    "--- old comment",
    "+-- new comment",
    " select 2;",
    "\\ No newline at end of file",
  ].join("\n"));
  assert.equal(numbered, true);
  assert.deepEqual(lines.map((line) => [line.kind, line.content, line.oldLine, line.newLine]), [
    ["meta", "diff --git a/q.sql b/q.sql", null, null],
    ["meta", "index 1111111..2222222 100644", null, null],
    ["meta", "--- a/q.sql", null, null],
    ["meta", "+++ b/q.sql", null, null],
    ["hunk", "@@ -10,3 +10,3 @@ select", null, null],
    ["context", "select 1;", 10, 10],
    ["del", "-- old comment", 11, null],
    ["add", "-- new comment", null, 11],
    ["context", "select 2;", 12, 12],
    ["meta", "\\ No newline at end of file", null, null],
  ]);

  const handWritten = parseDiffLines("@@ fn execute @@\nlet a = 1;\n-let b = 2;\n+let b = 3;");
  assert.equal(handWritten.numbered, false);
  assert.deepEqual(handWritten.lines.map((line) => [line.kind, line.content]), [
    ["hunk", "@@ fn execute @@"], ["context", "let a = 1;"], ["del", "let b = 2;"], ["add", "let b = 3;"],
  ]);
});

test("every registered grammar loads under the JavaScript regex engine", async () => {
  for (const language of HIGHLIGHT_LANGUAGES) {
    await loadHighlightLanguage(language);
    assert.ok(tokenizeLoaded("x = 1", language), language);
  }
  const [line] = tokenizeLoaded("pub fn run() -> u32 { 1 }", "rust")!;
  const keyword = line.find((token) => token.content === "fn");
  assert.equal(keyword?.color, "var(--shiki-token-keyword)");
});

function render(body: string, streaming = false) {
  return renderToStaticMarkup(createElement(MarkdownRenderer, { body, streaming, scrollKey: "message:test" }));
}

test("Markdown fences render headers, diff rows and collapse long blocks", () => {
  const header = render("```rust path=core/src/strategy.rs#L922\nfn a() {}\nfn b() {}\n```");
  assert.match(header, /class="code-block-title"[^>]*>core\/src\/strategy\.rs</);
  assert.match(header, /class="code-block-range">L922</);
  assert.match(header, /data-line="922"/);
  assert.match(header, /data-line="923"/);
  assert.match(header, /aria-label="Copy code"/);

  const diff = render("```diff rust\n@@ -1,2 +1,2 @@\n-let a = 1;\n+let a = 2;\n let b = 3;\n```");
  assert.match(diff, /<pre class="is-diff has-gutter"/);
  assert.match(diff, /class="code-line diff-del"><span class="code-gutter code-gutter-diff" aria-hidden="true" data-old="1" data-new=""><\/span><span class="code-diff-marker">-<\/span>let a = 1;/);
  assert.match(diff, /class="code-line diff-add">.*data-old="" data-new="1"/);

  const plain = render("```\nplain text\n```");
  assert.match(plain, /<pre><code>plain text<\/code><\/pre>/, "undecorated blocks keep a single text node");

  const long = `\`\`\`text\n${Array.from({ length: 45 }, (_, index) => `line ${index + 1}`).join("\n")}\n\`\`\``;
  assert.match(render(long), /class="code-block-shell expandable collapsed"/);
  assert.match(render(long), /Show all 45 lines/);
  assert.doesNotMatch(render(long, true), /code-block-expand/, "streaming blocks are not collapsed");
});
