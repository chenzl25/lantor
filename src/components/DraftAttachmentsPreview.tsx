import { type MouseEvent, type PointerEvent, useEffect, useState } from "react";
import { FileText, X } from "lucide-react";
import { DraftAttachment } from "../types";

type DraftAttachmentsPreviewProps = {
  attachments: DraftAttachment[];
  onRemove: (id: string) => void;
};

type ImagePreview = {
  src: string;
  alt: string;
};

type DraftAttachmentPreviewItemProps = {
  attachment: DraftAttachment;
  onRemove: (id: string) => void;
  onOpenImage: (preview: ImagePreview) => void;
};

function isolateDraftAttachmentEvent(event: MouseEvent<HTMLElement> | PointerEvent<HTMLElement>) {
  event.stopPropagation();
}

function DraftAttachmentPreviewItem({ attachment, onRemove, onOpenImage }: DraftAttachmentPreviewItemProps) {
  const isImage = attachment.mime_type.startsWith("image/");
  const [objectUrl, setObjectUrl] = useState("");

  useEffect(() => {
    if (!isImage) {
      setObjectUrl("");
      return;
    }

    const nextUrl = URL.createObjectURL(attachment.file);
    setObjectUrl(nextUrl);
    return () => URL.revokeObjectURL(nextUrl);
  }, [attachment.file, isImage]);

  if (isImage) {
    return (
      <div className="draft-attachment image">
        <button
          type="button"
          className="draft-attachment-trigger"
          aria-label={`Preview ${attachment.original_name || "image"}`}
          onPointerDown={isolateDraftAttachmentEvent}
          onClick={(event) => {
            event.stopPropagation();
            if (!objectUrl) return;
            onOpenImage({
              src: objectUrl,
              alt: attachment.original_name || "image",
            });
          }}
        >
          {objectUrl && <img src={objectUrl} alt="" />}
        </button>
        <button
          type="button"
          className="draft-attachment-remove"
          onPointerDown={isolateDraftAttachmentEvent}
          onClick={() => onRemove(attachment.id)}
          aria-label={`Remove ${attachment.original_name || "image"}`}
        >
          <X size={14} />
        </button>
      </div>
    );
  }

  return (
    <div className="draft-attachment file">
      <FileText size={14} />
      <span>{attachment.original_name || "attachment"}</span>
      <button
        type="button"
        className="draft-attachment-remove"
        onPointerDown={isolateDraftAttachmentEvent}
        onClick={() => onRemove(attachment.id)}
        aria-label={`Remove ${attachment.original_name || "attachment"}`}
      >
        <X size={12} />
      </button>
    </div>
  );
}

export function DraftAttachmentsPreview({ attachments, onRemove }: DraftAttachmentsPreviewProps) {
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
      <div className="draft-attachments">
        {attachments.map((attachment) => (
          <DraftAttachmentPreviewItem
            key={attachment.id}
            attachment={attachment}
            onRemove={onRemove}
            onOpenImage={setImagePreview}
          />
        ))}
      </div>
      {imagePreview && (
        <div
          className="attachment-lightbox"
          role="dialog"
          aria-modal="true"
          aria-label="Image preview"
          onPointerDown={isolateDraftAttachmentEvent}
          onClick={isolateDraftAttachmentEvent}
        >
          <button
            type="button"
            className="attachment-lightbox-backdrop"
            aria-label="Close image preview"
            onPointerDown={isolateDraftAttachmentEvent}
            onClick={closeImagePreview}
          />
          <button
            type="button"
            className="attachment-lightbox-close"
            aria-label="Close image preview"
            onPointerDown={isolateDraftAttachmentEvent}
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
