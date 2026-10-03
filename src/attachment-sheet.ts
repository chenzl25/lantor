import { useSyncExternalStore } from "react";
import { isTauriRuntime } from "./apiClient";
import { isStandaloneDisplay } from "./display-mode";
import type { MessageAttachment } from "./types";

/**
 * The Home Screen web app has no browser chrome. Any navigation to a file, even
 * a new-window open or a download, replaces the app with a view that has no way
 * back. There, attachments open in an in-app sheet instead.
 */
export function usesAttachmentSheet() {
  return !isTauriRuntime() && isStandaloneDisplay();
}

/**
 * What the sheet shows: a stored attachment fetched from the server, or a file
 * the app generated in the browser (a thread SVG export) that the user previews
 * before saving it.
 */
export type AttachmentSheetItem =
  | { kind: "stored"; id: string; attachment: MessageAttachment }
  | { kind: "generated"; id: string; file: File };

let openItem: AttachmentSheetItem | null = null;
let generatedSequence = 0;
const listeners = new Set<() => void>();

function setOpenItem(item: AttachmentSheetItem | null) {
  openItem = item;
  listeners.forEach((listener) => listener());
}

export function openAttachmentSheet(attachment: MessageAttachment) {
  setOpenItem({ kind: "stored", id: attachment.id, attachment });
}

export function openGeneratedFileSheet(file: File) {
  generatedSequence += 1;
  setOpenItem({ kind: "generated", id: `generated-${generatedSequence}`, file });
}

export function closeAttachmentSheet() {
  setOpenItem(null);
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function useOpenAttachmentSheetItem() {
  return useSyncExternalStore(subscribe, () => openItem, () => null);
}

export type AttachmentSheetDelivery = "save" | "share" | "download";

/**
 * How the sheet's primary button hands the file over:
 * - the desktop app saves into Downloads natively, because its webview ignores
 *   `<a download>`;
 * - the Home Screen web app uses the share sheet, because a download would
 *   leave the app;
 * - everything else downloads normally.
 */
export function attachmentSheetDelivery({ shareable }: { shareable: boolean }): AttachmentSheetDelivery {
  if (isTauriRuntime()) return "save";
  if (usesAttachmentSheet() && shareable) return "share";
  return "download";
}

export type AttachmentPreviewKind = "image" | "video" | "audio" | "markdown" | "text" | "none";

/** Text previews are read fully into memory and rendered, so they stay small. */
export const TEXT_PREVIEW_LIMIT_BYTES = 512 * 1024;

const MARKDOWN_EXTENSION = /\.(md|markdown|mdx)$/i;
const TEXT_EXTENSION = /\.(txt|log|csv|tsv|json|jsonl|ya?ml|toml|ini|xml|diff|patch|sql|sh|py|rs|ts|tsx|js|jsx|mjs|css|html?)$/i;
const TEXT_APPLICATION_TYPES = new Set([
  "application/json",
  "application/x-ndjson",
  "application/xml",
  "application/x-yaml",
  "application/yaml",
  "application/toml",
  "application/sql",
  "application/javascript",
]);

/**
 * PDFs and Office documents are not previewed: iOS shows only the first page of
 * an embedded PDF, so the share sheet is the reliable way to read them.
 */
export function attachmentPreviewKind(attachment: Pick<MessageAttachment, "mime_type" | "original_name" | "size_bytes">): AttachmentPreviewKind {
  const type = attachment.mime_type.toLowerCase().split(";")[0].trim();
  const name = attachment.original_name;
  if (type.startsWith("image/")) return "image";
  if (type.startsWith("video/")) return "video";
  if (type.startsWith("audio/")) return "audio";
  const markdown = type === "text/markdown" || type === "text/x-markdown" || MARKDOWN_EXTENSION.test(name);
  const text = markdown || type.startsWith("text/") || TEXT_APPLICATION_TYPES.has(type)
    || ((type === "" || type === "application/octet-stream") && TEXT_EXTENSION.test(name));
  if (!text || attachment.size_bytes > TEXT_PREVIEW_LIMIT_BYTES) return "none";
  return markdown ? "markdown" : "text";
}

export function triggerBrowserDownload(url: string, filename: string) {
  const link = document.createElement("a");
  link.href = url;
  link.download = filename;
  link.rel = "noreferrer";
  document.body.appendChild(link);
  link.click();
  link.remove();
}
