import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { openGeneratedFileSheet } from "../../../src/attachment-sheet";
import { AttachmentSheetHost } from "../../../src/components/AttachmentSheet";
import { MessageAttachments } from "../../../src/components/MessageAttachments";
import { MessageMarkdown } from "../../../src/components/MessageMarkdown";
import type { MessageAttachment } from "../../../src/types";
import "../../../src/styles.css";

const attachment = (id: string, original_name: string, mime_type: string, size_bytes: number, source_path: string | null = null): MessageAttachment => ({
  id, message_id: "message", original_name, mime_type, size_bytes, storage_path: `/fixture/${id}`, source_path, created_at: "2026-10-01T00:00:00Z",
});
// A generated file, like a thread SVG export, is previewed before it is saved.
export const exportedSvg = '<svg xmlns="http://www.w3.org/2000/svg" width="240" height="120"><rect width="240" height="120" fill="navy"/></svg>';
// Sizes match the bytes the e2e route serves.
const attachments = [
  attachment("markdown", "implementation.md", "text/markdown", 46, "/ws/out/implementation.md"),
  attachment("pdf", "report.pdf", "application/pdf", 1200, "/ws/out/report.pdf"),
  attachment("image", "chart.svg", "image/svg+xml", 140),
  attachment("flaky", "flaky.txt", "text/plain", 11),
];

createRoot(document.getElementById("root")!).render(<StrictMode>
  <main style={{ padding: 16 }}>
    <MessageMarkdown body="Deliverables: [the report](/ws/out/report.pdf) and [notes](/ws/out/implementation.md), [line notes](/ws/out/implementation.md:2), [hash notes](/ws/out/implementation.md#L2)." attachments={attachments} />
    <MessageAttachments attachments={attachments} showImageThumbnails />
    <button type="button" onClick={() => openGeneratedFileSheet(new File([exportedSvg], "thread-ui-review.svg", { type: "image/svg+xml" }))}>
      Export thread SVG
    </button>
    <AttachmentSheetHost />
  </main>
</StrictMode>);
