import { FileText, LoaderCircle } from "lucide-react";

import type { Artifact } from "../types";
import { MessageMarkdown } from "./MessageMarkdown";
import { Modal } from "./Modal";

type ArtifactViewerModalProps = {
  artifact: Artifact | null;
  loading: boolean;
  error: string | null;
  onClose: () => void;
};

export function ArtifactViewerModal({
  artifact,
  loading,
  error,
  onClose,
}: ArtifactViewerModalProps) {
  return (
    <Modal
      open={Boolean(artifact)}
      title={artifact?.title ?? "Artifact"}
      onClose={onClose}
      width={960}
    >
      {artifact && (
        <article className="artifact-viewer">
          <header className="artifact-viewer-meta">
            <span><FileText size={15} /> {artifact.kind}</span>
            <span>artifact {artifact.id.slice(0, 8)}</span>
            {artifact.creator_agent_handle && <span>@{artifact.creator_agent_handle.replace(/^@/, "")}</span>}
          </header>

          {artifact.summary && <p className="artifact-viewer-summary">{artifact.summary}</p>}

          {loading ? (
            <div className="artifact-viewer-status">
              <LoaderCircle className="spin" size={20} />
              <span>Loading report…</span>
            </div>
          ) : error ? (
            <div className="artifact-viewer-status error" role="alert">{error}</div>
          ) : artifact.kind === "markdown" ? (
            <div className="artifact-viewer-content">
              <MessageMarkdown
                body={artifact.content}
                scrollKey={`artifact-viewer:${artifact.id}`}
                channelId={artifact.channel_id}
              />
            </div>
          ) : (
            <pre className="artifact-viewer-plain">{artifact.content}</pre>
          )}
        </article>
      )}
    </Modal>
  );
}
