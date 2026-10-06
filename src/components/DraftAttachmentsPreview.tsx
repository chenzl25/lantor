import { ImageLightbox } from "./ImageLightbox";
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
        <ImageLightbox key={imagePreview.src} src={imagePreview.src} alt={imagePreview.alt}
          onClose={() => setImagePreview(null)} />
      )}
    </>
  );
}
