import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { Download, FileText, RotateCcw, Share } from "lucide-react";
import { attachmentAssetUrl, downloadAttachment, saveTextDownload } from "../apiClient";
import {
  attachmentPreviewKind,
  closeAttachmentSheet,
  useOpenAttachmentSheetItem,
  type AttachmentSheetDelivery,
  type AttachmentSheetItem,
} from "../attachment-sheet";
import type { MessageAttachment } from "../types";
import { formatByteSize } from "../ui-utils";
import { DialogHeader } from "./DialogHeader";
import { DialogSurface } from "./DialogSurface";
import { deliveryLabel, useFileDelivery } from "./FileDelivery";
import { MessageMarkdown } from "./MessageMarkdown";

type LoadState =
  | { status: "loading" }
  | { status: "ready"; file: File; url: string; text: string | null }
  | { status: "error"; message: string };

function errorMessage(error: unknown) {
  return error instanceof Error ? error.message : String(error || "Unknown error");
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
  // Generated files are text (SVG); stored attachments are copied by path.
  const { delivery, saving, deliver, notice } = useFileDelivery(ready, async (file) => item.kind === "stored"
    ? downloadAttachment(item.attachment.storage_path, info.name)
    : saveTextDownload(info.name, await file.text()));

  return <DialogSurface label={info.name} labelledBy={titleId} className="modal-card attachment-sheet"
    onClose={onClose}>
    <DialogHeader className="attachment-sheet-head" title={info.name} titleId={titleId}
      subtitle={[info.mimeType || "file", formatByteSize(info.sizeBytes)].join(" · ")} onClose={onClose}
      actions={<button type="button" className="attachment-sheet-share" disabled={!ready || saving} onClick={deliver}>
        {delivery === "share" ? <Share size={16} /> : <Download size={16} />}
        <span>{deliveryLabel(delivery, saving)}</span>
      </button>} />
    <div className="modal-body attachment-sheet-body">
      {notice}
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
    <p>Can’t preview this file.</p>
    <p>{delivery === "share" ? "Tap Share to save it or open it elsewhere." : "Use Download to save it."}</p>
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
