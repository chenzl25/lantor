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

let openAttachment: MessageAttachment | null = null;
const listeners = new Set<() => void>();

function setOpenAttachment(attachment: MessageAttachment | null) {
  openAttachment = attachment;
  listeners.forEach((listener) => listener());
}

export function openAttachmentSheet(attachment: MessageAttachment) {
  setOpenAttachment(attachment);
}

export function closeAttachmentSheet() {
  setOpenAttachment(null);
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function useOpenAttachment() {
  return useSyncExternalStore(subscribe, () => openAttachment, () => null);
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
