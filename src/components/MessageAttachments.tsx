import { type MouseEvent, type PointerEvent, useEffect, useState } from "react";
import { FileText, Image, X } from "lucide-react";
import { attachmentAssetUrl, isTauriRuntime, openExternalUrl } from "../apiClient";
import { MessageAttachment } from "../types";

type MessageAttachmentsProps = {
  attachments: MessageAttachment[];
  showImageThumbnails: boolean;
};

type ImagePreview = {
  src: string;
  alt: string;
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

function isolateAttachmentEvent(event: MouseEvent<HTMLElement> | PointerEvent<HTMLElement>) {
  event.stopPropagation();
}

export function MessageAttachments({ attachments, showImageThumbnails }: MessageAttachmentsProps) {
  const [imagePreview, setImagePreview] = useState<ImagePreview | null>(null);

  function closeImagePreview(event: MouseEvent<HTMLButtonElement>) {
    event.preventDefault();
    event.stopPropagation();
    setImagePreview(null);
  }

  useEffect(() => {
    if (!imagePreview) return;
    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") setImagePreview(null);
    }
    function handleHistoryNavigation() {
      setImagePreview(null);
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
              <button
                key={attachment.id}
                type="button"
                className={`message-attachment image ${showImageThumbnails ? "" : "compact-image"} ${attachment.local_url ? "pending" : ""}`}
                aria-label={`Preview ${attachment.original_name}`}
                title={attachment.original_name}
                onPointerDown={isolateAttachmentEvent}
                onClick={(event) => {
                  event.stopPropagation();
                  setImagePreview({ src, alt: attachment.original_name });
                }}
              >
                {showImageThumbnails ? (
                  <img src={src} alt="" loading="lazy" />
                ) : (
                  <>
                    <span className="attachment-icon"><Image size={18} /></span>
                    <span className="attachment-meta">
                      <span><Image size={13} /> {attachment.original_name}</span>
                      <small>{attachment.mime_type || "image"} · {formatBytes(attachment.size_bytes)}</small>
                    </span>
                  </>
                )}
              </button>
            );
          }
          return (
            <a
              key={attachment.id}
              className={`message-attachment ${attachment.local_url ? "pending" : ""}`}
              href={src}
              target="_blank"
              rel="noreferrer"
              title={attachment.original_name}
              onPointerDown={isolateAttachmentEvent}
              onClick={(event) => {
                event.stopPropagation();
                void openStoredAttachment(event, attachment);
              }}
            >
              <span className="attachment-icon"><FileText size={18} /></span>
              <span className="attachment-meta">
                <span><FileText size={13} /> {attachment.original_name}</span>
                <small>{attachment.mime_type || "file"} · {formatBytes(attachment.size_bytes)}</small>
              </span>
            </a>
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
          <div className="attachment-lightbox-content">
            <img src={imagePreview.src} alt={imagePreview.alt} />
          </div>
        </div>
      )}
    </>
  );
}
