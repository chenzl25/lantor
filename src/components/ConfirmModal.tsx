import { useEffect, useState } from "react";
import { Modal } from "./Modal";

type ConfirmModalProps = {
  open: boolean;
  title: string;
  body: string;
  confirmLabel: string;
  onCancel: () => void;
  onConfirm: () => Promise<void> | void;
};

export function ConfirmModal({
  open,
  title,
  body,
  confirmLabel,
  onCancel,
  onConfirm,
}: ConfirmModalProps) {
  const [submitting, setSubmitting] = useState(false);

  useEffect(() => {
    if (!open) setSubmitting(false);
  }, [open]);

  async function confirm() {
    if (submitting) return;
    setSubmitting(true);
    try {
      await onConfirm();
      onCancel();
    } catch {
      // The parent mutation path surfaces the error toast.
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <Modal open={open} title={title} onClose={onCancel} width={440}>
      <div className="confirm-modal">
        <p>{body}</p>
        <div className="modal-actions">
          <button type="button" onClick={onCancel} disabled={submitting}>Cancel</button>
          <button type="button" className="danger" onClick={confirm} disabled={submitting}>
            {submitting ? "Deleting…" : confirmLabel}
          </button>
        </div>
      </div>
    </Modal>
  );
}
