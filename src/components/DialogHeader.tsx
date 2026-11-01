import type { ReactNode } from "react";
import { X } from "lucide-react";
import { IconButton } from "./IconButton";

type DialogHeaderProps = {
  title: ReactNode;
  titleId?: string;
  eyebrow?: ReactNode;
  subtitle?: ReactNode;
  /** Controls shown before the close button. */
  actions?: ReactNode;
  onClose: () => void;
  closeLabel?: string;
  closeDisabled?: boolean;
  autoFocusClose?: boolean;
  className?: string;
};

// Title row with a close button, shared by every dialog built on DialogSurface.
export function DialogHeader({ title, titleId, eyebrow, subtitle, actions, onClose, closeLabel = "Close",
  closeDisabled, autoFocusClose, className }: DialogHeaderProps) {
  return <header className={className ? `dialog-header ${className}` : "dialog-header"}>
    <div className="dialog-header-title">
      {eyebrow && <span className="dialog-header-eyebrow">{eyebrow}</span>}
      <h2 id={titleId}>{title}</h2>
      {subtitle && <p>{subtitle}</p>}
    </div>
    {actions && <div className="dialog-header-actions">{actions}</div>}
    <IconButton className="dialog-header-close" label={closeLabel} onClick={onClose} disabled={closeDisabled}
      autoFocus={autoFocusClose}>
      <X size={18} />
    </IconButton>
  </header>;
}
