import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { MarkdownRenderer } from "../src/components/MarkdownRenderer";
import type { MessageAttachment } from "../src/types";

const snapshot = (id: string, sourcePath: string | null): MessageAttachment => ({
  id, message_id: "message", original_name: id, mime_type: "application/octet-stream", size_bytes: 1,
  storage_path: `/lantor/attachments/message/${id}`, source_path: sourcePath, created_at: "2026-10-01T00:00:00Z",
});

const hrefs = (body: string, attachments?: MessageAttachment[]) => Array.from(
  renderToStaticMarkup(createElement(MarkdownRenderer, { body, attachments })).matchAll(/<a [^>]*href="([^"]*)"/g),
  (match) => match[1].replace(/&amp;/g, "&"),
);

test("browser local file links resolve to the message's snapshot attachments", () => {
  const attachments = [
    snapshot("deck", "/ws/out/meeting deck.pptx"),
    snapshot("cn", "/ws/out/德芙.docx"),
    snapshot("pdf", "/ws/out/report.pdf"),
    snapshot("upload", null),
  ];
  const body = [
    "[deck](</ws/out/meeting deck.pptx>)",
    "[cn](/ws/out/%E5%BE%B7%E8%8A%99.docx)",
    "[pdf](file:///ws/out/report.pdf)",
    "[line](/ws/out/report.pdf:3)",
    "[other](/ws/out/missing.md)",
    "[web](https://example.com/report.pdf)",
  ].join(" ");

  assert.deepEqual(hrefs(body, attachments), [
    "/api/attachments/deck",
    "/api/attachments/cn",
    "/api/attachments/pdf",
    "/api/attachments/pdf",
    "/ws/out/missing.md",
    "https://example.com/report.pdf",
  ]);
  assert.deepEqual(hrefs("[deck](</ws/out/meeting deck.pptx>)"), ["/ws/out/meeting%20deck.pptx"]);
});


test("position links resolve the underlying snapshot and preserve encoded filename punctuation", () => {
  const attachments = [snapshot("code", "/ws/@owner/code.rs"), snapshot("literal", "/ws/literal:42")];
  assert.deepEqual(hrefs([
    "[line](/ws/@owner/code.rs:42)",
    "[column](/ws/@owner/code.rs:42:5)",
    "[hash](/ws/@owner/code.rs#L42)",
    "[range](/ws/@owner/code.rs#L42-L50)",
    "[literal](/ws/literal%3A42)",
  ].join(" "), attachments), [
    "/api/attachments/code", "/api/attachments/code", "/api/attachments/code", "/api/attachments/code", "/api/attachments/literal",
  ]);
});


test("invalid position suffixes do not match an unrelated snapshot", () => {
  const paths = ["/ws/code.rs:0", "/ws/code.rs:0:5", "/ws/code.rs:4294967296:5", "/ws/code.rs#L0", "/ws/code.rs#L42-L0"];
  assert.deepEqual(hrefs(paths.map((path) => `[file](${path})`).join(" "), [snapshot("code", "/ws/code.rs")]), paths);
});
