import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { Download, FileText, RotateCcw, Share, X } from "lucide-react";
import { attachmentAssetUrl, downloadAttachment, saveTextDownload } from "../apiClient";
import {
  attachmentPreviewKind,
  attachmentSheetDelivery,
  closeAttachmentSheet,
  triggerBrowserDownload,
  useOpenAttachmentSheetItem,
  type AttachmentSheetDelivery,
  type AttachmentSheetItem,
} from "../attachment-sheet";
import type { MessageAttachment } from "../types";
import { formatByteSize } from "../ui-utils";
import { DialogSurface } from "./DialogSurface";
import { MessageMarkdown } from "./MessageMarkdown";

type LoadState =
  | { status: "loading" }
  | { status: "ready"; file: File; url: string; text: string | null }
  | { status: "error"; message: string };

type SheetNotice = { kind: "success" | "error"; message: string };

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

function sheetFileInfo(item: AttachmentSheetItem) {
  return item.kind === "stored"
    ? { name: item.attachment.original_name, mimeType: item.attachment.mime_type, sizeBytes: item.attachment.size_bytes }
    : { name: item.file.name, mimeType: item.file.type, sizeBytes: item.file.size };
}

async function fetchStoredFile(attachment: MessageAttachment, signal: AbortSignal) {
  const source = attachment.local_url ?? attachmentAssetUrl(attachment.storage_path, attachment.id);
  const response = await fetch(source, { signal });
  if (!response.ok) throw new Error(`HTTP ${response.status}`);
  const blob = await response.blob();
  return new File([blob], attachment.original_name, { type: attachment.mime_type || blob.type });
}

function savedFileName(path: string, fallback: string) {
  return path.split(/[\\/]/).pop() || fallback;
}

/** Mounted once per app. Only one file is open at a time. */
export function AttachmentSheetHost() {
  const item = useOpenAttachmentSheetItem();
  if (!item) return null;
  return <AttachmentSheet key={item.id} item={item} onClose={closeAttachmentSheet} />;
}

function AttachmentSheet({ item, onClose }: { item: AttachmentSheetItem; onClose: () => void }) {
  const titleId = useId();
  const info = sheetFileInfo(item);
  const kind = attachmentPreviewKind({ mime_type: info.mimeType, original_name: info.name, size_bytes: info.sizeBytes });
  const [attempt, setAttempt] = useState(0);
  const [state, setState] = useState<LoadState>({ status: "loading" });
  const [notice, setNotice] = useState<SheetNotice | null>(null);
  const [saving, setSaving] = useState(false);

  // Share needs the bytes up front: iOS only opens the share sheet if
  // navigator.share runs synchronously within the tap.
  useEffect(() => {
    const controller = new AbortController();
    let objectUrl: string | null = null;
    setState({ status: "loading" });
    (async () => {
      const file = item.kind === "generated" ? item.file : await fetchStoredFile(item.attachment, controller.signal);
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
  }, [item, kind, attempt]);

  // A back gesture leaves the sheet, like the image lightbox.
  useEffect(() => {
    window.addEventListener("popstate", onClose);
    return () => window.removeEventListener("popstate", onClose);
  }, [onClose]);

  const ready = state.status === "ready" ? state : null;
  const delivery = attachmentSheetDelivery({ shareable: ready ? canShareFile(ready.file) : true });

  async function saveToDownloads(file: File) {
    setSaving(true);
    try {
      // Generated files are text (SVG); stored attachments are copied by path.
      const path = item.kind === "stored"
        ? await downloadAttachment(item.attachment.storage_path, info.name)
        : await saveTextDownload(info.name, await file.text());
      setNotice({ kind: "success", message: `Saved to Downloads: ${savedFileName(path, info.name)}` });
    } catch (error) {
      setNotice({ kind: "error", message: `Save failed: ${errorMessage(error)}` });
    } finally {
      setSaving(false);
    }
  }

  function deliver() {
    if (!ready) return;
    setNotice(null);
    if (delivery === "save") {
      void saveToDownloads(ready.file);
      return;
    }
    if (delivery === "download") {
      triggerBrowserDownload(ready.url, ready.file.name);
      return;
    }
    navigator.share({ files: [ready.file] }).catch((error: unknown) => {
      if (error instanceof DOMException && error.name === "AbortError") return;
      setNotice({ kind: "error", message: `Share failed: ${errorMessage(error)}` });
    });
  }

  return <DialogSurface label={info.name} labelledBy={titleId} className="modal-card attachment-sheet"
    onClose={onClose}>
    <header className="modal-head attachment-sheet-head">
      <div className="attachment-sheet-title">
        <h3 id={titleId}>{info.name}</h3>
        <small>{[info.mimeType || "file", formatByteSize(info.sizeBytes)].join(" · ")}</small>
      </div>
      <div className="attachment-sheet-actions">
        <button type="button" className="attachment-sheet-share" disabled={!ready || saving} onClick={deliver}>
          {delivery === "share" ? <Share size={16} /> : <Download size={16} />}
          <span>{delivery === "share" ? "Share" : saving ? "Saving…" : "Download"}</span>
        </button>
        <button type="button" className="modal-close" onClick={onClose} aria-label="Close">
          <X size={18} />
        </button>
      </div>
    </header>
    <div className="modal-body attachment-sheet-body">
      {notice && <p className={notice.kind === "error" ? "attachment-sheet-error" : "attachment-sheet-notice"}
        role={notice.kind === "error" ? "alert" : "status"}>{notice.message}</p>}
      {state.status === "loading" && <p className="attachment-sheet-status" role="status">Loading…</p>}
      {state.status === "error" && <div className="attachment-sheet-status" role="alert">
        <p>Could not load this file: {state.message}</p>
        <button type="button" className="attachment-sheet-retry" onClick={() => setAttempt((value) => value + 1)}>
          <RotateCcw size={15} /> Retry
        </button>
      </div>}
      {ready && <AttachmentPreview kind={kind} name={info.name} url={ready.url} text={ready.text} delivery={delivery} />}
    </div>
  </DialogSurface>;
}

function AttachmentPreview({ kind, name, url, text, delivery }: {
  kind: ReturnType<typeof attachmentPreviewKind>;
  name: string;
  url: string;
  text: string | null;
  delivery: AttachmentSheetDelivery;
}) {
  if (kind === "image" && /\.svg$/i.test(name)) return <SvgPreview url={url} name={name} />;
  if (kind === "image") return <img className="attachment-sheet-media" src={url} alt={name} />;
  if (kind === "video") return <video className="attachment-sheet-media" src={url} controls playsInline />;
  if (kind === "audio") return <audio className="attachment-sheet-audio" src={url} controls />;
  if (kind === "markdown" && text !== null) return <div className="attachment-sheet-markdown"><MessageMarkdown body={text} /></div>;
  if (kind === "text" && text !== null) return <pre className="attachment-sheet-text">{text}</pre>;
  return <div className="attachment-sheet-placeholder">
    <FileText size={36} />
    <p>No preview for this file here.</p>
    <p>{delivery === "share" ? "Use Share to save it to Files or open it in another app." : "Use Download to save it."}</p>
  </div>;
}

/**
 * WebKit (iPhone, and the macOS desktop app) paints an SVG's foreignObject
 * content unscaled when the <img> is laid out smaller than its intrinsic size,
 * so a narrow sheet clipped thread exports, which are HTML in a foreignObject.
 * Lay the image out at its intrinsic size and fit it with a transform instead.
 */
function SvgPreview({ url, name }: { url: string; name: string }) {
  const frameRef = useRef<HTMLDivElement>(null);
  const [natural, setNatural] = useState<{ width: number; height: number } | null>(null);
  const [frameWidth, setFrameWidth] = useState(0);

  useLayoutEffect(() => {
    const frame = frameRef.current;
    if (!frame) return;
    setFrameWidth(frame.clientWidth);
    const observer = new ResizeObserver(() => setFrameWidth(frame.clientWidth));
    observer.observe(frame);
    return () => observer.disconnect();
  }, []);

  const scale = natural && natural.width > 0 && frameWidth > 0 ? Math.min(1, frameWidth / natural.width) : 1;
  return <div ref={frameRef} className="attachment-sheet-svg-frame"
    style={natural ? { height: natural.height * scale } : undefined}>
    <img className="attachment-sheet-media attachment-sheet-svg" src={url} alt={name}
      style={natural
        ? { width: natural.width, height: natural.height, transform: `scale(${scale})` }
        : { visibility: "hidden" }}
      onLoad={(event) => setNatural({ width: event.currentTarget.naturalWidth, height: event.currentTarget.naturalHeight })} />
  </div>;
}
