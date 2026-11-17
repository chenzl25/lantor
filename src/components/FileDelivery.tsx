import { useState } from "react";
import { revealInFileManager } from "../apiClient";
import {
  attachmentSheetDelivery,
  displaySavedPath,
  revealActionLabel,
  triggerBrowserDownload,
  type AttachmentSheetDelivery,
} from "../attachment-sheet";

type DeliveryNotice = { kind: "success" | "error"; message: string; savedPath?: string };

export type ReadyFile = { file: File; url: string };

function errorMessage(error: unknown) {
  return error instanceof Error ? error.message : String(error || "Unknown error");
}

function canShareFile(file: File) {
  try {
    return typeof navigator.share === "function" && navigator.canShare?.({ files: [file] }) === true;
  } catch {
    return false;
  }
}

export function deliveryLabel(delivery: AttachmentSheetDelivery, saving: boolean) {
  if (delivery === "share") return "Share";
  return saving ? "Saving…" : "Download";
}

/**
 * The share, download or save button of the file sheet and the report reader
 * (see attachmentSheetDelivery). `saveToDownloads` writes the file in the
 * desktop app and returns where it went.
 */
export function useFileDelivery(ready: ReadyFile | null, saveToDownloads: (file: File) => Promise<string>) {
  const [notice, setNotice] = useState<DeliveryNotice | null>(null);
  const [saving, setSaving] = useState(false);
  const delivery = attachmentSheetDelivery({ shareable: ready ? canShareFile(ready.file) : true });

  async function save(file: File) {
    setSaving(true);
    try {
      const path = await saveToDownloads(file);
      setNotice({ kind: "success", message: `Saved to ${displaySavedPath(path)}`, savedPath: path });
    } catch (error) {
      setNotice({ kind: "error", message: `Couldn’t save: ${errorMessage(error)}` });
    } finally {
      setSaving(false);
    }
  }

  async function reveal(path: string) {
    try {
      await revealInFileManager(path);
    } catch (error) {
      setNotice({ kind: "error", message: `Couldn’t show this file: ${errorMessage(error)}` });
    }
  }

  // iOS opens the share sheet only if navigator.share runs synchronously within the tap.
  function deliver() {
    if (!ready) return;
    setNotice(null);
    if (delivery === "save") {
      void save(ready.file);
      return;
    }
    if (delivery === "download") {
      triggerBrowserDownload(ready.url, ready.file.name);
      return;
    }
    navigator.share({ files: [ready.file] }).catch((error: unknown) => {
      if (error instanceof DOMException && error.name === "AbortError") return;
      setNotice({ kind: "error", message: `Couldn’t share: ${errorMessage(error)}` });
    });
  }

  function showError(message: string, error: unknown) {
    setNotice({ kind: "error", message: `${message}: ${errorMessage(error)}` });
  }

  const savedPath = notice?.savedPath;
  const noticeView = notice && <p className={notice.kind === "error" ? "attachment-sheet-error" : "attachment-sheet-notice"}
    role={notice.kind === "error" ? "alert" : "status"}>
    <span>{notice.message}</span>
    {savedPath && <button type="button" className="attachment-sheet-reveal"
      onClick={() => void reveal(savedPath)}>{revealActionLabel()}</button>}
  </p>;

  return { delivery, saving, deliver, notice: noticeView, clearNotice: () => setNotice(null), showError };
}
