import { ExternalLink } from "lucide-react";
import { Artifact } from "../types";
import { MessageMarkdown } from "./MessageMarkdown";
import { Modal } from "./Modal";

type ArtifactModalProps = {
  artifact: Artifact | null;
  onClose: () => void;
  onOpenRaw: (artifact: Artifact) => void;
};

function ArtifactBody({ artifact }: { artifact: Artifact }) {
  if (artifact.kind === "markdown") {
    return <MessageMarkdown body={artifact.content} scrollKey={`artifact-modal:${artifact.id}`} />;
  }

  return <pre className="artifact-modal-raw">{artifact.content}</pre>;
}

export function ArtifactModal({ artifact, onClose, onOpenRaw }: ArtifactModalProps) {
  return (
    <Modal
      open={Boolean(artifact)}
      title={artifact?.title || "Artifact"}
      onClose={onClose}
      width={920}
    >
      {artifact && (
        <div className="artifact-modal">
          <div className="artifact-modal-meta">
            <span>{artifact.kind} · artifact {artifact.id.slice(0, 8)}</span>
            <button type="button" onClick={() => onOpenRaw(artifact)}>
              <ExternalLink size={15} />
              <span>Open raw</span>
            </button>
          </div>
          {artifact.summary && <p className="artifact-modal-summary">{artifact.summary}</p>}
          <div className="artifact-modal-content">
            <ArtifactBody artifact={artifact} />
          </div>
        </div>
      )}
    </Modal>
  );
}
