import type { MouseEvent } from "react";

export function isPrimaryUnmodifiedClick(event: MouseEvent<HTMLElement>) {
  return event.button === 0 && !event.ctrlKey && !event.metaKey && !event.altKey && !event.shiftKey;
}
