import type { ButtonHTMLAttributes, ReactNode } from "react";

type IconButtonProps = Omit<ButtonHTMLAttributes<HTMLButtonElement>, "aria-label" | "children"> & {
  /** Accessible name; the button shows only an icon. */
  label: string;
  tone?: "danger";
  children: ReactNode;
};

// The icon-only button for dialog and panel headers. `.icon-button` owns its
// size, shape and states; a caller's class only places or hides it.
export function IconButton({ label, tone, className, type = "button", children, ...rest }: IconButtonProps) {
  return <button {...rest} type={type} aria-label={label} data-tone={tone}
    className={className ? `icon-button ${className}` : "icon-button"}>
    {children}
  </button>;
}
