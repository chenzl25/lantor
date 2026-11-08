import { ChevronDown, ChevronUp, Search, X } from "lucide-react";
import type { KeyboardEvent } from "react";
import type { ThreadFind } from "../hooks/useThreadFind";
import { isImeComposing } from "../input-utils";
import { IconButton } from "./IconButton";

/** The thread's find bar; useThreadFind owns its state. */
export function ThreadFindBar({ find }: { find: ThreadFind }) {
  function onInputKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    if (isImeComposing(event) || event.key !== "Enter") return;
    event.preventDefault();
    if (event.shiftKey) find.previous();
    else find.next();
  }

  // Escape anywhere in the bar closes it, not the thread.
  function onBarKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (event.key !== "Escape" || isImeComposing(event)) return;
    event.preventDefault();
    event.stopPropagation();
    find.close();
  }

  const status = !find.query.trim()
    ? ""
    : find.total > 0
      ? `${find.position}/${find.total}`
      : find.searching ? "Searching…" : "No results";
  return (
    <div className="thread-find-bar" role="search" aria-label="Find in thread" aria-busy={find.searching || find.moving}
      onKeyDown={onBarKeyDown}>
      <Search size={15} aria-hidden="true" className="thread-find-icon" />
      <input
        ref={find.inputRef}
        type="text"
        value={find.query}
        onChange={(event) => find.setQuery(event.target.value)}
        onKeyDown={onInputKeyDown}
        placeholder="Find in thread"
        aria-label="Find in thread"
        spellCheck={false}
        autoCapitalize="off"
        autoCorrect="off"
        autoComplete="off"
      />
      <span className="thread-find-count" aria-live="polite">{status}</span>
      <IconButton label="Previous match" title="Previous match (⇧↩)" disabled={find.total === 0} onClick={find.previous}>
        <ChevronUp size={16} />
      </IconButton>
      <IconButton label="Next match" title="Next match (↩)" disabled={find.total === 0} onClick={find.next}>
        <ChevronDown size={16} />
      </IconButton>
      <IconButton label="Close find" title="Close (Esc)" onClick={find.close}>
        <X size={16} />
      </IconButton>
    </div>
  );
}
