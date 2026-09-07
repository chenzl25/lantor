import type { DraftAttachment } from "../types";

export type FailedMessageDraft = {
  id: string;
  text: string;
  attachments: DraftAttachment[];
  error: string;
  asTask: boolean;
};

export type FailedMessageDraftProps = {
  failedDrafts?: FailedMessageDraft[];
  onRecoverFailedDraft?: (draft: FailedMessageDraft) => void;
  onDiscardFailedDraft?: (id: string) => void;
};

export function appendRecoveredText(current: string, recovered: string) {
  return current && recovered ? `${current}\n\n${recovered}` : current || recovered;
}

export function FailedMessageDrafts({ failedDrafts = [], onRecoverFailedDraft, onDiscardFailedDraft }: FailedMessageDraftProps) {
  if (!failedDrafts.length) return null;
  return <div className="failed-message-drafts" aria-label="Unsent messages">
    {failedDrafts.map((draft) => <div className="failed-message-draft" key={draft.id}>
      <strong role="status">Message could not be sent</strong>
      <p>{draft.error}</p>
      {draft.text && <details><summary>View unsent text</summary><p>{draft.text}</p></details>}
      {draft.attachments.length > 0 && <p>{draft.attachments.map((attachment) => attachment.file.name).join(", ")}</p>}
      {/* Focusing a button moves the mobile composer before pointer-up. Keep
          its position stable so the first tap completes the action. */}
      <div>
        <button type="button" onMouseDown={(event) => event.preventDefault()} onClick={() => onRecoverFailedDraft?.(draft)}>Add to draft</button>
        <button type="button" onMouseDown={(event) => event.preventDefault()} onClick={() => onDiscardFailedDraft?.(draft.id)}>Discard</button>
      </div>
    </div>)}
  </div>;
}
