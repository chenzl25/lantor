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
    "/ws/out/report.pdf:3",
    "/ws/out/missing.md",
    "https://example.com/report.pdf",
  ]);
  assert.deepEqual(hrefs("[deck](</ws/out/meeting deck.pptx>)"), ["/ws/out/meeting%20deck.pptx"]);
});
