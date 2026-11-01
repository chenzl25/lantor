import { Bookmark, Copy, MessageSquare } from "lucide-react";
import { useEffect, useRef } from "react";

type MessageActionMenuProps = {
  x: number;
  y: number;
  isSaved: boolean;
  onCopyThreadReference: () => void;
  onCopyMarkdown: () => void;
  onToggleSaved: () => void;
  onClose: () => void;
};

export function MessageActionMenu({
  x,
  y,
  isSaved,
  onCopyThreadReference,
  onCopyMarkdown,
  onToggleSaved,
  onClose,
}: MessageActionMenuProps) {
  const openedAtRef = useRef(Date.now());

  useEffect(() => {
    function handleClose() {
      if (Date.now() - openedAtRef.current < 350) return;
      onClose();
    }

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") onClose();
    }

    window.addEventListener("click", handleClose);
    window.addEventListener("scroll", handleClose, true);
    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.removeEventListener("click", handleClose);
      window.removeEventListener("scroll", handleClose, true);
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [onClose]);

  return (
    <div
      className="message-action-menu"
      style={{
        left: Math.max(12, Math.min(x, window.innerWidth - 248)),
        top: Math.max(12, Math.min(y, window.innerHeight - 160)),
      }}
      onPointerDown={(event) => event.stopPropagation()}
      onClick={(event) => event.stopPropagation()}
      onContextMenu={(event) => event.stopPropagation()}
      role="menu"
    >
      <button type="button" onClick={onCopyThreadReference}>
        <MessageSquare size={18} />
        <span>Copy thread link</span>
      </button>
      <button type="button" onClick={onCopyMarkdown}>
        <Copy size={18} />
        <span>Copy text</span>
      </button>
      <button type="button" onClick={onToggleSaved}>
        <Bookmark size={18} />
        <span>{isSaved ? "Remove from Saved" : "Save"}</span>
      </button>
    </div>
  );
}
