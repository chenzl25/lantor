import { GitPullRequest } from "lucide-react";
import { useEffect, useState } from "react";

import type { Agent } from "../types";
import { Modal } from "./Modal";
import { TaskAssigneePicker } from "./TaskAssigneePicker";

export type GithubReviewTarget = {
  /** `owner/repo`. */
  repository: string;
  number: number;
  title: string;
  authorLogin: string | null;
};

type GithubReviewDialogProps = {
  target: GithubReviewTarget | null;
  agents: Agent[];
  /** Creates the review task; the dialog closes when it resolves. */
  onStart: (agentId: string) => Promise<unknown>;
  onClose: () => void;
};

function errorMessage(error: unknown) {
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === "string" && error.trim()) return error;
  return "Couldn’t start the review";
}

/** Picks the agent for a pull request review task, from a channel's GitHub tab or the GitHub inbox. */
export function GithubReviewDialog({ target, agents, onStart, onClose }: GithubReviewDialogProps) {
  const [selectedAgentId, setSelectedAgentId] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const targetKey = target ? `${target.repository}#${target.number}` : "";

  useEffect(() => {
    setSelectedAgentId(agents[0]?.id ?? "");
    setError(null);
    // A new target starts with the first agent; a changed agent list keeps the choice.
  }, [targetKey]);

  const selectedAgent = agents.find((agent) => agent.id === selectedAgentId) ?? null;

  async function start() {
    if (!target || !selectedAgentId || busy) return;
    setBusy(true);
    setError(null);
    try {
      await onStart(selectedAgentId);
      onClose();
    } catch (startError) {
      setError(errorMessage(startError));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal
      open={Boolean(target)}
      title={target ? `Review ${target.repository}#${target.number}` : "Review PR"}
      onClose={() => {
        if (!busy) onClose();
      }}
      closeOnBackdrop={!busy}
      closeOnEscape={!busy}
      width={540}
    >
      {target && (
        <div className="modal-form">
          <div className="github-review-target">
            <GitPullRequest size={20} />
            <div>
              <strong>{target.title}</strong>
              {target.authorLogin && <span>by @{target.authorLogin}</span>}
            </div>
          </div>
          <div className="modal-field github-agent-assignment">
            <span className="modal-field-label">Assign agent</span>
            <TaskAssigneePicker
              agents={agents}
              assignee={selectedAgent}
              allowUnassigned={false}
              ariaLabel={`Assign review agent for ${target.repository}#${target.number}`}
              onChange={setSelectedAgentId}
            />
          </div>
          {selectedAgent && (
            <p className="github-modal-note">
              A Task and canonical thread will be created and dispatched to @{selectedAgent.handle}.
            </p>
          )}
          <p className="github-modal-note">
            The task is anchored to the latest PR head SHA. The Agent reports in Lantor and
            will not publish a GitHub review.
          </p>
          {error && <div className="github-inline-error" role="alert">{error}</div>}
          <div className="modal-actions">
            <button type="button" disabled={busy} onClick={onClose}>
              Cancel
            </button>
            <button
              type="button"
              className="primary"
              disabled={!selectedAgentId || busy}
              onClick={() => void start()}
            >
              {busy ? "Creating…" : "Start review"}
            </button>
          </div>
        </div>
      )}
    </Modal>
  );
}
