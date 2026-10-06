import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { DraftAttachmentsPreview } from "../../../src/components/DraftAttachmentsPreview";
import { MessageAttachments } from "../../../src/components/MessageAttachments";
import "../../../src/styles.css";

// A 1600x1200 grid, so screenshots show where the zoom landed.
const lines = Array.from({ length: 15 }, (_, i) => `<path d="M${(i + 1) * 100} 0V1200M0 ${(i + 1) * 100}H1600" stroke="white" stroke-opacity="0.4"/>`).join("");
const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="1600" height="1200"><rect width="1600" height="1200" fill="#2a6f97"/>${lines}<circle cx="1200" cy="300" r="60" fill="#f4a261"/><text x="80" y="1120" font-size="96" fill="white">chart.svg</text></svg>`;
const image = { id: "image", message_id: "message", original_name: "chart.svg", mime_type: "image/svg+xml", size_bytes: svg.length,
  storage_path: "/fixture/chart.svg", local_url: `data:image/svg+xml,${encodeURIComponent(svg)}`, created_at: "" };
const draft = { id: "draft", original_name: "draft.svg", mime_type: "image/svg+xml", size_bytes: svg.length,
  file: new File([svg], "draft.svg", { type: "image/svg+xml" }) };

createRoot(document.getElementById("root")!).render(<StrictMode>
  <main style={{ padding: 32 }}>
    <MessageAttachments attachments={[image]} showImageThumbnails />
    <DraftAttachmentsPreview attachments={[draft]} onRemove={() => {}} />
  </main>
</StrictMode>);
