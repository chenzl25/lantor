import { Check, CheckCircle2, Hand, MessageSquareText, Star, X } from "lucide-react";
import { useState } from "react";
import { decisionStatusLabel, useDecisionActions } from "../decisions";
import type { Decision } from "../types";
import { formatTime } from "../ui-utils";
import { MessageMarkdown } from "./MessageMarkdown";

type DecisionCardProps = {
  decision: Decision;
  /** Needs-you list rendering: the list row already shows requester and place. */
  compact?: boolean;
};

/** Short key shown on each option: single-letter ids as-is, otherwise A, B, C… by position. */
export function decisionOptionKey(optionId: string, index: number) {
  return /^[a-z0-9]$/i.test(optionId) ? optionId.toUpperCase() : String.fromCharCode(65 + (index % 26));
}

function errorText(err: unknown) {
  if (err instanceof Error) return err.message;
  return typeof err === "string" ? err : "Failed to send the decision";
}

export function DecisionCard({ decision, compact = false }: DecisionCardProps) {
  const actions = useDecisionActions();
  const [selected, setSelected] = useState<string | null>(null);
  const [noteOpen, setNoteOpen] = useState(false);
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const open = decision.status === "open" && actions !== null;
  const composing = open && (selected !== null || noteOpen);
  const selectedIndex = decision.options.findIndex((option) => option.id === selected);
  const selectedOption = selectedIndex >= 0 ? decision.options[selectedIndex] : null;
  const canSend = open && !busy && (selectedOption !== null || note.trim().length > 0);

  async function run(action: () => Promise<void>) {
    setBusy(true);
    setError(null);
    try {
      await action();
      setSelected(null);
      setNoteOpen(false);
      setNote("");
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  }

  function send() {
    if (!canSend || !actions) return;
    void run(() => actions.answer(decision, selectedOption?.id ?? null, note.trim()));
  }

  function dismiss() {
    if (!actions || busy) return;
    void run(() => actions.dismiss(decision));
  }

  const statusClass = `decision-card status-${decision.status}${compact ? " compact" : ""}`;
  return (
    <div className={statusClass} role="group" aria-label={`Decision: ${decision.title}`}>
      <div className="decision-card-head">
        <span className="decision-card-kicker">
          {decision.status === "open" ? <Hand size={14} /> : <CheckCircle2 size={14} />}
          {decisionStatusLabel(decision)}
          {decision.task_number !== null && <span className="decision-card-task">#{decision.task_number}</span>}
          {decision.resolved_at && decision.status !== "open" && <time dateTime={decision.resolved_at}>{formatTime(decision.resolved_at)}</time>}
        </span>
        <strong className="decision-card-title">{decision.title}</strong>
      </div>
      {decision.context && (
        <div className="decision-card-context">
          <MessageMarkdown body={decision.context} scrollKey={`decision:${decision.id}`} />
        </div>
      )}
      <div className="decision-options" role={open ? "radiogroup" : "list"} aria-label="Options">
        {decision.options.map((option, index) => {
          const chosen = decision.answer_option_id === option.id;
          const isSelected = selected === option.id;
          return (
            <button
              key={option.id}
              type="button"
              role={open ? "radio" : "listitem"}
              aria-checked={open ? isSelected : undefined}
              className={[
                "decision-option",
                option.recommended ? "recommended" : "",
                isSelected ? "selected" : "",
                chosen ? "chosen" : "",
              ].filter(Boolean).join(" ")}
              disabled={!open || busy}
              onClick={() => setSelected(isSelected ? null : option.id)}
            >
              <span className="decision-option-key" aria-hidden="true">
                {chosen || isSelected ? <Check size={14} /> : decisionOptionKey(option.id, index)}
              </span>
              <span className="decision-option-text">
                <strong>
                  {option.label}
                  {option.recommended && <em><Star size={11} /> Recommended</em>}
                </strong>
                {option.detail && <small>{option.detail}</small>}
              </span>
            </button>
          );
        })}
      </div>
      {decision.status !== "open" && decision.answer_note && (
        <p className="decision-card-note">
          <MessageSquareText size={14} />
          <span>{decision.answer_note}</span>
        </p>
      )}
      {open && (
        <div className="decision-card-actions">
          {composing && (
            <textarea
              className="decision-note"
              value={note}
              placeholder={selectedOption ? "Add a note (optional)" : "Write your answer"}
              rows={2}
              disabled={busy}
              onChange={(event) => setNote(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
                  event.preventDefault();
                  send();
                }
              }}
            />
          )}
          <div className="decision-card-buttons">
            {composing ? (
              <>
                <button type="button" className="decision-send" disabled={!canSend} onClick={send}>
                  {busy ? "Sending…" : selectedOption ? `Send ${decisionOptionKey(selectedOption.id, selectedIndex)}` : "Send answer"}
                </button>
                <button type="button" className="decision-secondary" disabled={busy}
                  onClick={() => { setSelected(null); setNoteOpen(false); setNote(""); setError(null); }}>
                  Cancel
                </button>
              </>
            ) : (
              <>
                <button type="button" className="decision-secondary" onClick={() => setNoteOpen(true)}>
                  <MessageSquareText size={14} /> Answer in words
                </button>
                <button type="button" className="decision-secondary subtle" disabled={busy} onClick={dismiss}
                  title="Close without answering (e.g. settled in chat)">
                  <X size={14} /> Dismiss
                </button>
              </>
            )}
          </div>
          {error && <p className="decision-card-error" role="alert">{error}</p>}
        </div>
      )}
    </div>
  );
}
