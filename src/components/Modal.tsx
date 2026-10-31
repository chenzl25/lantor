import { useId, type ReactNode } from "react";
import { DialogHeader } from "./DialogHeader";
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
    <DialogHeader title={title} titleId={titleId} onClose={onClose} />
    <div className="modal-body">{children}</div>
  </DialogSurface>;
}
