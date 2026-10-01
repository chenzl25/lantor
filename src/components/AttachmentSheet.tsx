import { useEffect, useId, useState } from "react";
import { Download, FileText, RotateCcw, Share, X } from "lucide-react";
import { attachmentAssetUrl } from "../apiClient";
import {
  attachmentPreviewKind,
  closeAttachmentSheet,
  triggerBrowserDownload,
  useOpenAttachment,
} from "../attachment-sheet";
import type { MessageAttachment } from "../types";
import { formatByteSize } from "../ui-utils";
import { DialogSurface } from "./DialogSurface";
import { MessageMarkdown } from "./MessageMarkdown";

type LoadState =
  | { status: "loading" }
  | { status: "ready"; file: File; url: string; text: string | null }
  | { status: "error"; message: string };

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

/** Mounted once per app. Only one attachment is open at a time. */
export function AttachmentSheetHost() {
  const attachment = useOpenAttachment();
  if (!attachment) return null;
  return <AttachmentSheet key={attachment.id} attachment={attachment} onClose={closeAttachmentSheet} />;
}

function AttachmentSheet({ attachment, onClose }: { attachment: MessageAttachment; onClose: () => void }) {
  const titleId = useId();
  const kind = attachmentPreviewKind(attachment);
  const [attempt, setAttempt] = useState(0);
  const [state, setState] = useState<LoadState>({ status: "loading" });
  const [shareError, setShareError] = useState<string | null>(null);

  // Share needs the bytes up front: iOS only opens the share sheet if
  // navigator.share runs synchronously within the tap.
  useEffect(() => {
    const controller = new AbortController();
    let objectUrl: string | null = null;
    setState({ status: "loading" });
    (async () => {
      const source = attachment.local_url ?? attachmentAssetUrl(attachment.storage_path, attachment.id);
      const response = await fetch(source, { signal: controller.signal });
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const blob = await response.blob();
      const file = new File([blob], attachment.original_name, { type: attachment.mime_type || blob.type });
      const text = kind === "markdown" || kind === "text" ? await file.text() : null;
      if (controller.signal.aborted) return;
      objectUrl = URL.createObjectURL(file);
      setState({ status: "ready", file, url: objectUrl, text });
    })().catch((error) => {
      if (!controller.signal.aborted) setState({ status: "error", message: errorMessage(error) });
    });
    return () => {
      controller.abort();
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  }, [attachment, kind, attempt]);

  // A back gesture leaves the sheet, like the image lightbox.
  useEffect(() => {
    window.addEventListener("popstate", onClose);
    return () => window.removeEventListener("popstate", onClose);
  }, [onClose]);

  const ready = state.status === "ready" ? state : null;
  const shareable = ready ? canShareFile(ready.file) : true;

  function deliver() {
    if (!ready) return;
    setShareError(null);
    if (!canShareFile(ready.file)) {
      triggerBrowserDownload(ready.url, ready.file.name);
      return;
    }
    navigator.share({ files: [ready.file] }).catch((error: unknown) => {
      if (error instanceof DOMException && error.name === "AbortError") return;
      setShareError(`Share failed: ${errorMessage(error)}`);
    });
  }

  return <DialogSurface label={attachment.original_name} labelledBy={titleId} className="modal-card attachment-sheet"
    onClose={onClose}>
    <header className="modal-head attachment-sheet-head">
      <div className="attachment-sheet-title">
        <h3 id={titleId}>{attachment.original_name}</h3>
        <small>{[attachment.mime_type || "file", formatByteSize(attachment.size_bytes)].join(" · ")}</small>
      </div>
      <div className="attachment-sheet-actions">
        <button type="button" className="attachment-sheet-share" disabled={!ready} onClick={deliver}>
          {shareable ? <Share size={16} /> : <Download size={16} />}
          <span>{shareable ? "Share" : "Download"}</span>
        </button>
        <button type="button" className="modal-close" onClick={onClose} aria-label="Close">
          <X size={18} />
        </button>
      </div>
    </header>
    <div className="modal-body attachment-sheet-body">
      {shareError && <p className="attachment-sheet-error" role="alert">{shareError}</p>}
      {state.status === "loading" && <p className="attachment-sheet-status" role="status">Loading…</p>}
      {state.status === "error" && <div className="attachment-sheet-status" role="alert">
        <p>Could not load this file: {state.message}</p>
        <button type="button" className="attachment-sheet-retry" onClick={() => setAttempt((value) => value + 1)}>
          <RotateCcw size={15} /> Retry
        </button>
      </div>}
      {ready && <AttachmentPreview kind={kind} name={attachment.original_name} url={ready.url} text={ready.text} />}
    </div>
  </DialogSurface>;
}

function AttachmentPreview({ kind, name, url, text }: {
  kind: ReturnType<typeof attachmentPreviewKind>;
  name: string;
  url: string;
  text: string | null;
}) {
  if (kind === "image") return <img className="attachment-sheet-media" src={url} alt={name} />;
  if (kind === "video") return <video className="attachment-sheet-media" src={url} controls playsInline />;
  if (kind === "audio") return <audio className="attachment-sheet-audio" src={url} controls />;
  if (kind === "markdown" && text !== null) return <div className="attachment-sheet-markdown"><MessageMarkdown body={text} /></div>;
  if (kind === "text" && text !== null) return <pre className="attachment-sheet-text">{text}</pre>;
  return <div className="attachment-sheet-placeholder">
    <FileText size={36} />
    <p>No preview for this file here.</p>
    <p>Use Share to save it to Files or open it in another app.</p>
  </div>;
}
