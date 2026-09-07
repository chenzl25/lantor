import { useId, type ReactNode } from "react";
import { X } from "lucide-react";
import { DialogSurface } from "./DialogSurface";

type ModalProps = {
  open: boolean;
  title: string;
  onClose: () => void;
  children: ReactNode;
  width?: number;
  closeOnBackdrop?: boolean;
  closeOnEscape?: boolean;
};

export function Modal({ open, title, onClose, children, width = 480, closeOnBackdrop = true, closeOnEscape = true }: ModalProps) {
  const titleId = useId();
  if (!open) return null;
  return <DialogSurface label={title} labelledBy={titleId} className="modal-card" style={{ width }}
    onClose={onClose} closeOnBackdrop={closeOnBackdrop} closeOnEscape={closeOnEscape}>
    <header className="modal-head">
      <h3 id={titleId}>{title}</h3>
      <button type="button" className="modal-close" onClick={onClose} aria-label="Close">
        <X size={18} />
      </button>
    </header>
    <div className="modal-body">{children}</div>
  </DialogSurface>;
}
