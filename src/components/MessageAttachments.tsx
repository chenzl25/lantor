import { type MouseEvent, type PointerEvent, useEffect, useRef, useState } from "react";
import { Download, FileText, Image, X, ZoomIn, ZoomOut } from "lucide-react";
import { attachmentAssetUrl, downloadAttachment, isTauriRuntime, openExternalUrl } from "../apiClient";
import { MessageAttachment } from "../types";

type MessageAttachmentsProps = {
  attachments: MessageAttachment[];
  showImageThumbnails: boolean;
};

type ImagePreview = {
  src: string;
  alt: string;
  attachment: MessageAttachment;
};

type DownloadFeedback = {
  kind: "success" | "error";
  message: string;
};

function formatBytes(value: number) {
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
  return `${(value / 1024 / 1024).toFixed(1)} MB`;
}

function errorMessage(error: unknown, fallback: string) {
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === "string" && error) return error;
  return fallback;
}

async function openStoredAttachment(event: MouseEvent<HTMLAnchorElement>, attachment: MessageAttachment) {
  if (attachment.local_url || !isTauriRuntime()) return;

  event.preventDefault();
  try {
    await openExternalUrl(attachment.storage_path);
  } catch (error) {
    console.error("Failed to open attachment", error);
  }
}

async function downloadStoredAttachment(event: MouseEvent<HTMLElement>, attachment: MessageAttachment): Promise<DownloadFeedback> {
  event.preventDefault();
  event.stopPropagation();

  if (!attachment.local_url && isTauriRuntime()) {
    try {
      const savedPath = await downloadAttachment(attachment.storage_path, attachment.original_name);
      return {
        kind: "success",
        message: `Saved to Downloads: ${savedPath.split(/[\\/]/).pop() || attachment.original_name}`,
      };
    } catch (error) {
      console.error("Failed to download attachment", error);
      return {
        kind: "error",
        message: errorMessage(error, `Download failed: ${attachment.original_name}`),
      };
    }
  }

  try {
    triggerBrowserDownload(
      attachment.local_url ?? attachmentAssetUrl(attachment.storage_path, attachment.id),
      attachment.original_name,
    );
    return {
      kind: "success",
      message: `Download started: ${attachment.original_name}`,
    };
  } catch (error) {
    console.error("Failed to download attachment", error);
    return {
      kind: "error",
      message: errorMessage(error, `Download failed: ${attachment.original_name}`),
    };
  }
}

function triggerBrowserDownload(url: string, filename: string) {
  const link = document.createElement("a");
  link.href = url;
  link.download = filename;
  link.rel = "noreferrer";
  document.body.appendChild(link);
  link.click();
  link.remove();
}

function isolateAttachmentEvent(event: MouseEvent<HTMLElement> | PointerEvent<HTMLElement>) {
  event.stopPropagation();
}

export function MessageAttachments({ attachments, showImageThumbnails }: MessageAttachmentsProps) {
  const [imagePreview, setImagePreview] = useState<ImagePreview | null>(null);
  const [imagePreviewZoomed, setImagePreviewZoomed] = useState(false);
  const [downloadFeedback, setDownloadFeedback] = useState<DownloadFeedback | null>(null);
  const downloadFeedbackTimerRef = useRef<number | null>(null);

  function closeImagePreview(event: MouseEvent<HTMLButtonElement>) {
    event.preventDefault();
    event.stopPropagation();
    setImagePreview(null);
    setImagePreviewZoomed(false);
  }

  function openImagePreview(preview: ImagePreview) {
    setImagePreview(preview);
    setImagePreviewZoomed(false);
  }

  function toggleImagePreviewZoom(event: MouseEvent<HTMLElement>) {
    event.preventDefault();
    event.stopPropagation();
    setImagePreviewZoomed((zoomed) => !zoomed);
  }

  function showDownloadFeedback(feedback: DownloadFeedback) {
    if (downloadFeedbackTimerRef.current !== null) {
      window.clearTimeout(downloadFeedbackTimerRef.current);
    }
    setDownloadFeedback(feedback);
    downloadFeedbackTimerRef.current = window.setTimeout(() => {
      setDownloadFeedback(null);
      downloadFeedbackTimerRef.current = null;
    }, 4200);
  }

  function clearDownloadFeedback() {
    if (downloadFeedbackTimerRef.current !== null) {
      window.clearTimeout(downloadFeedbackTimerRef.current);
      downloadFeedbackTimerRef.current = null;
    }
    setDownloadFeedback(null);
  }

  function handleDownloadAttachment(event: MouseEvent<HTMLElement>, attachment: MessageAttachment) {
    void downloadStoredAttachment(event, attachment).then(showDownloadFeedback);
  }

  useEffect(() => {
    if (!imagePreview) return;
    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        setImagePreview(null);
        setImagePreviewZoomed(false);
      }
    }
    function handleHistoryNavigation() {
      setImagePreview(null);
      setImagePreviewZoomed(false);
    }
    window.addEventListener("keydown", handleKeyDown);
    window.addEventListener("popstate", handleHistoryNavigation);
    return () => {
      window.removeEventListener("keydown", handleKeyDown);
      window.removeEventListener("popstate", handleHistoryNavigation);
    };
  }, [imagePreview]);

  useEffect(() => {
    return () => {
      if (downloadFeedbackTimerRef.current !== null) {
        window.clearTimeout(downloadFeedbackTimerRef.current);
      }
    };
  }, []);

  if (attachments.length === 0) return null;

  return (
    <>
      <div className="message-attachments">
        {attachments.map((attachment) => {
          const src = attachment.local_url ?? attachmentAssetUrl(attachment.storage_path, attachment.id);
          const isImage = attachment.mime_type.startsWith("image/");
          if (isImage) {
            return (
              <div
                key={attachment.id}
                className={`message-attachment image ${showImageThumbnails ? "" : "compact-image"} ${attachment.local_url ? "pending" : ""}`}
                data-attachment-name={attachment.original_name}
                onPointerDown={isolateAttachmentEvent}
              >
                <button
                  type="button"
                  className="attachment-preview-trigger"
                  aria-label={`Preview ${attachment.original_name}`}
                  onClick={(event) => {
                    event.stopPropagation();
                    openImagePreview({ src, alt: attachment.original_name, attachment });
                  }}
                >
                  {showImageThumbnails ? (
                    <img src={src} alt="" loading="lazy" />
                  ) : (
                    <>
                      <span className="attachment-icon"><Image size={18} /></span>
                      <span className="attachment-meta">
                        <span className="attachment-name">{attachment.original_name}</span>
                        <small className="attachment-type">{attachment.mime_type || "image"}</small>
                        <small className="attachment-size">{formatBytes(attachment.size_bytes)}</small>
                      </span>
                    </>
                  )}
                </button>
                <button
                  type="button"
                  className="attachment-download"
                  aria-label={`Download ${attachment.original_name}`}
                  title={`Download ${attachment.original_name}`}
                  onPointerDown={isolateAttachmentEvent}
                  onClick={(event) => {
                    handleDownloadAttachment(event, attachment);
                  }}
                >
                  <Download size={15} />
                </button>
              </div>
            );
          }
          return (
            <div
              key={attachment.id}
              className={`message-attachment ${attachment.local_url ? "pending" : ""}`}
              data-attachment-name={attachment.original_name}
              onPointerDown={isolateAttachmentEvent}
            >
              <a
                className="attachment-open"
                href={src}
                target="_blank"
                rel="noreferrer"
                aria-label={`Open ${attachment.original_name}`}
                onClick={(event) => {
                  event.stopPropagation();
                  void openStoredAttachment(event, attachment);
                }}
              >
                <span className="attachment-icon"><FileText size={18} /></span>
                <span className="attachment-meta">
                  <span className="attachment-name">{attachment.original_name}</span>
                  <small className="attachment-type">{attachment.mime_type || "file"}</small>
                  <small className="attachment-size">{formatBytes(attachment.size_bytes)}</small>
                </span>
              </a>
              <button
                type="button"
                className="attachment-download"
                aria-label={`Download ${attachment.original_name}`}
                title={`Download ${attachment.original_name}`}
                onPointerDown={isolateAttachmentEvent}
                onClick={(event) => {
                  handleDownloadAttachment(event, attachment);
                }}
              >
                <Download size={15} />
              </button>
            </div>
          );
        })}
      </div>
      {imagePreview && (
        <div
          className="attachment-lightbox"
          role="dialog"
          aria-modal="true"
          aria-label="Image preview"
          onPointerDown={isolateAttachmentEvent}
          onClick={isolateAttachmentEvent}
        >
          <button
            type="button"
            className="attachment-lightbox-backdrop"
            aria-label="Close image preview"
            onPointerDown={isolateAttachmentEvent}
            onClick={closeImagePreview}
          />
          <button
            type="button"
            className="attachment-lightbox-close"
            aria-label="Close image preview"
            onPointerDown={isolateAttachmentEvent}
            onClick={closeImagePreview}
          >
            <X size={18} />
          </button>
          <button
            type="button"
            className="attachment-lightbox-zoom"
            aria-label={imagePreviewZoomed ? "Fit image to screen" : "View image at full size"}
            aria-pressed={imagePreviewZoomed}
            onPointerDown={isolateAttachmentEvent}
            onClick={toggleImagePreviewZoom}
          >
            {imagePreviewZoomed ? <ZoomOut size={18} /> : <ZoomIn size={18} />}
          </button>
          <button
            type="button"
            className="attachment-lightbox-download"
            aria-label={`Download ${imagePreview.alt}`}
            title={`Download ${imagePreview.alt}`}
            onPointerDown={isolateAttachmentEvent}
            onClick={(event) => {
              handleDownloadAttachment(event, imagePreview.attachment);
            }}
          >
            <Download size={18} />
          </button>
          <div className={`attachment-lightbox-content ${imagePreviewZoomed ? "zoomed" : ""}`}>
            <button
              type="button"
              className="attachment-lightbox-image-button"
              aria-label={imagePreviewZoomed ? "Fit image to screen" : "View image at full size"}
              onPointerDown={isolateAttachmentEvent}
              onClick={toggleImagePreviewZoom}
            >
              <img src={imagePreview.src} alt={imagePreview.alt} />
            </button>
          </div>
        </div>
      )}
      {downloadFeedback && (
        <div className={`app-toast ${downloadFeedback.kind}`} role={downloadFeedback.kind === "error" ? "alert" : "status"}>
          <span>{downloadFeedback.message}</span>
          <button type="button" onClick={clearDownloadFeedback} aria-label="Dismiss download notification">Dismiss</button>
        </div>
      )}
    </>
  );
}
