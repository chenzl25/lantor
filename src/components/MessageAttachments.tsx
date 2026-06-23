import { type MouseEvent, type PointerEvent, useEffect, useState } from "react";
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

function formatBytes(value: number) {
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
  return `${(value / 1024 / 1024).toFixed(1)} MB`;
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

async function downloadStoredAttachment(event: MouseEvent<HTMLElement>, attachment: MessageAttachment) {
  event.preventDefault();
  event.stopPropagation();

  if (!attachment.local_url && isTauriRuntime()) {
    try {
      await downloadAttachment(attachment.storage_path, attachment.original_name);
    } catch (error) {
      console.error("Failed to download attachment", error);
    }
    return;
  }

  triggerBrowserDownload(
    attachment.local_url ?? attachmentAssetUrl(attachment.storage_path, attachment.id),
    attachment.original_name,
  );
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
                    void downloadStoredAttachment(event, attachment);
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
                  void downloadStoredAttachment(event, attachment);
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
              void downloadStoredAttachment(event, imagePreview.attachment);
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
    </>
  );
}
