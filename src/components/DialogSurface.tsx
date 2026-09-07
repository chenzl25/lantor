import { useLayoutEffect, useRef, type CSSProperties, type PointerEvent, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { activeDialog, dialogOpener, registerDialogLayer } from "../dialog-layers";
import { shouldDismissOnEscape } from "../escape-dismiss";
import { useEventCallback } from "../hooks/useEventCallback";

type Props = {
  label: string;
  labelledBy?: string;
  className: string;
  backdropClassName?: string;
  style?: CSSProperties;
  children: ReactNode;
  onClose: () => void;
  closeOnBackdrop?: boolean;
  closeOnEscape?: boolean;
};

function tabStops(panel: HTMLElement) {
  return Array.from(panel.querySelectorAll<HTMLElement>(
    'button, a[href], input, select, textarea, summary, [tabindex], [contenteditable="true"]',
  )).filter((element) => element.tabIndex >= 0 && !element.matches(':disabled, [hidden], [inert], input[type="hidden"]')
    && !element.closest('[inert]') && element.getClientRects().length > 0
    && getComputedStyle(element).visibility !== "hidden");
}

// Shared by standard, full-page and nested image dialogs. Portal roots let
// the active layer make the rest of the application inert without hiding itself.
export function DialogSurface({ label, labelledBy, className, backdropClassName = "modal-backdrop", style,
  children, onClose, closeOnBackdrop = true, closeOnEscape = true }: Props) {
  const rootRef = useRef<HTMLDivElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const returnFocus = useRef([
    ...([dialogOpener()].filter((element): element is HTMLElement => element !== null)),
    ...(activeDialog()?.returnFocus ?? []),
  ]);
  const pointerStartedOutside = useRef(false);
  const close = useEventCallback(() => { if (closeOnEscape) onClose(); });

  useLayoutEffect(() => {
    const root = rootRef.current!;
    const panel = panelRef.current!;
    const unregister = registerDialogLayer({ root, panel, returnFocus: returnFocus.current });
    const focusFirst = () => (tabStops(panel)[0] ?? panel).focus({ preventScroll: true });
    if (!panel.contains(document.activeElement)) focusFirst();
    function onKey(event: KeyboardEvent) {
      if (activeDialog()?.panel !== panel || event.defaultPrevented) return;
      if (shouldDismissOnEscape(event)) {
        event.preventDefault();
        close();
      } else if (event.key === "Tab") {
        const stops = tabStops(panel);
        event.preventDefault();
        if (!stops.length) { panel.focus(); return; }
        const currentIndex = stops.indexOf(document.activeElement as HTMLElement);
        const nextIndex = currentIndex < 0 ? (event.shiftKey ? stops.length - 1 : 0)
          : (currentIndex + (event.shiftKey ? -1 : 1) + stops.length) % stops.length;
        // Move explicitly: WebKit can omit buttons from native Tab navigation
        // when macOS Full Keyboard Access is disabled.
        stops[nextIndex].focus();
      }
    }
    function onFocus(event: FocusEvent) {
      const target = event.target as HTMLElement;
      // React autofocus runs before a newly mounted child dialog registers.
      // Let that pending portal take focus; registered background layers are inert.
      if (target.closest('[data-dialog-layer]') && !target.closest('[inert]')) return;
      if (activeDialog()?.panel === panel && !panel.contains(target)) focusFirst();
    }
    window.addEventListener("keydown", onKey);
    document.addEventListener("focusin", onFocus);
    return () => {
      window.removeEventListener("keydown", onKey);
      document.removeEventListener("focusin", onFocus);
      const wasActive = activeDialog()?.panel === panel;
      unregister();
      if (!wasActive) return;
      const parent = activeDialog()?.panel;
      const previous = returnFocus.current.find((element) => element.isConnected && !element.closest('[inert]')
        && (!parent || parent.contains(element)));
      (previous ?? parent)?.focus({ preventScroll: true });
    };
  }, [close]);

  function onPointerDown(event: PointerEvent<HTMLDivElement>) {
    event.stopPropagation();
    pointerStartedOutside.current = event.button === 0 && event.target === event.currentTarget;
  }
  function onPointerUp(event: PointerEvent<HTMLDivElement>) {
    event.stopPropagation();
    const dismiss = pointerStartedOutside.current && event.target === event.currentTarget;
    pointerStartedOutside.current = false;
    if (dismiss && closeOnBackdrop && activeDialog()?.panel === panelRef.current) onClose();
  }
  return createPortal(<div ref={rootRef} className={backdropClassName} data-dialog-layer=""
    onClick={(event) => event.stopPropagation()} onPointerDown={onPointerDown} onPointerUp={onPointerUp} onPointerCancel={() => { pointerStartedOutside.current = false; }}>
    <div ref={panelRef} role="dialog" aria-modal="true" aria-label={labelledBy ? undefined : label}
      aria-labelledby={labelledBy} tabIndex={-1} data-dialog-panel="" className={className} style={style}>
      {children}
    </div>
  </div>, document.body);
}
