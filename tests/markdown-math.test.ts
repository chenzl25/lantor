import assert from "node:assert/strict";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import ReactMarkdown from "react-markdown";
import rehypeKatex from "rehype-katex";
import remarkMath from "remark-math";
import test from "node:test";
import { hasMathMarkdown, mightHaveMathMarkdown } from "../src/markdown-math";

test("ordinary code fences bypass even the CommonMark math preflight", () => {
  for (const body of ["```js\nconst math = 1;\n```", "~~~text\nhello\n~~~", "```js\nstreaming", "plain $5"]) {
    assert.equal(mightHaveMathMarkdown(body), false, body);
  }
  for (const body of ["$$x$$", "```math\nx", "~~~math\nx", "```m&#97;th\nx"]) {
    assert.equal(mightHaveMathMarkdown(body), true, body);
  }
});

const formulas = [
  "Inline $$x^2$$ formula",
  "$$\nx^2 + y^2 = z^2\n$$",
  "$$\nx^2", // streaming block, before the closing delimiter
  "$$$x^2$$$",
  "```math\nx^2\n```",
  "~~~math\nx^2\n~~~",
  "```math title\nx^2", // streaming math fence
  "> ```math\n> x^2\n> ```",
  "- ```math\n  x^2\n  ```",
  "```m&#97;th\nx^2\n```",
  "> $$x^2$$",
  "- $$x^2$$",
  "```text\n$$literal$$\n```\n\nThen $$x$$",
  "> ```text\n> $$literal$$\n\n$$x$$", // quoted fence ends with its container
  "`unclosed code before $$x$$",
  "Escaped backtick \\` before $$x$$",
  "[formula $$x$$](https://example.com)",
  "$$\\frac{a}{b}$$",
];

test("lazy gate preserves formulas supported by the original math pipeline", () => {
  for (const body of formulas) {
    assert.ok(hasMathMarkdown(body), body);
    const rendered = renderToStaticMarkup(createElement(ReactMarkdown, {
      children: body,
      remarkPlugins: [[remarkMath, { singleDollarTextMath: false }]],
      rehypePlugins: [[rehypeKatex, { strict: false, trust: false }]],
    }));
    assert.match(rendered, /class="katex(?: |")/, body);
  }
});

test("ordinary prose, prices and literal code do not request math resources", () => {
  for (const body of [
    "Hello **Lantor**", "$5 and $10", "$x^2$", "math and mathematics",
    "`$$literal$$`", "``code with ` and $$literal$$``",
    "`multiline\n$$literal$$\ncode`",
    "```js\nconst pid = '$$';\n```",
    "~~~text\n$$literal$$\n~~~",
    "```text\n$$still streaming code",
    "> ```js\n> $$literal$$\n> ```",
    "- ```text\n  $$literal$$\n  ```",
    "    $$indented code$$",
    "````text\n```math\nx^2\n```\n````",
    "\\$\\$escaped\\$\\$", "&#36;&#36;literal&#36;&#36;",
    "\\(x^2\\)", "\\[x^2\\]", "\\begin{equation}x^2\\end{equation}",
  ]) {
    assert.equal(hasMathMarkdown(body), false, body);
  }
});
