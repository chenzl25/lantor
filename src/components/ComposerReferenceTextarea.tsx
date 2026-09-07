import { forwardRef, useImperativeHandle, useLayoutEffect, useRef, type CompositionEventHandler, type ReactNode, type TextareaHTMLAttributes } from "react";

type ComposerReferenceTextareaProps = Omit<TextareaHTMLAttributes<HTMLTextAreaElement>, "value"> & {
  value: string;
};

const REFERENCE_TOKEN_PATTERN = /\[\[(message|thread):([^\]\s]+)\]\]/gi;

function renderReferenceText(value: string) {
  const parts: ReactNode[] = [];
  let lastIndex = 0;
  let match: RegExpExecArray | null;
  while ((match = REFERENCE_TOKEN_PATTERN.exec(value)) !== null) {
    if (match.index > lastIndex) {
      parts.push(value.slice(lastIndex, match.index));
    }
    const kind = match[1].toLowerCase();
    parts.push(
      // Render the full token text verbatim so the overlay mirrors the
      // underlying textarea character-for-character — the highlight must not
      // change glyph advance width, otherwise the caret / click hit-testing /
      // selection drift after the token. The shortened chip lives in the
      // separate composer reference-preview strip instead.
      <span key={`${match.index}:${match[0]}`} className={`composer-reference-token reference-${kind}`}>
        {match[0]}
      </span>,
    );
    lastIndex = match.index + match[0].length;
  }
  if (lastIndex < value.length) {
    parts.push(value.slice(lastIndex));
  }
  if (parts.length === 0) return value;
  return parts;
}

export const ComposerReferenceTextarea = forwardRef<HTMLTextAreaElement, ComposerReferenceTextareaProps>(
  function ComposerReferenceTextarea({ value, className, onCompositionStart, onCompositionEnd, ...props }, ref) {
    const textareaRef = useRef<HTMLTextAreaElement | null>(null);
    const isComposingRef = useRef(false);
    useImperativeHandle(ref, () => textareaRef.current as HTMLTextAreaElement);
    useLayoutEffect(() => {
      const textarea = textareaRef.current;
      if (!textarea || isComposingRef.current || textarea.value === value) return;
      textarea.value = value;
    }, [value]);

    const handleCompositionStart: CompositionEventHandler<HTMLTextAreaElement> = (event) => {
      isComposingRef.current = true;
      onCompositionStart?.(event);
    };
    const handleCompositionEnd: CompositionEventHandler<HTMLTextAreaElement> = (event) => {
      isComposingRef.current = false;
      onCompositionEnd?.(event);
    };
    const textareaProps = {
      ref: textareaRef,
      className,
      defaultValue: value,
      onCompositionStart: handleCompositionStart,
      onCompositionEnd: handleCompositionEnd,
      ...props,
    };

    REFERENCE_TOKEN_PATTERN.lastIndex = 0;
    const hasReferenceToken = REFERENCE_TOKEN_PATTERN.test(value);
    REFERENCE_TOKEN_PATTERN.lastIndex = 0;
    return (
      <div className={`composer-reference-input ${hasReferenceToken ? "has-reference-token" : ""}`}>
        <textarea {...textareaProps} />
        {hasReferenceToken && (
          <div className="composer-reference-overlay" aria-hidden="true">
            {renderReferenceText(value)}
          </div>
        )}
      </div>
    );
  },
);
