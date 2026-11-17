import { ChevronRight, FileText } from "lucide-react";
import { Artifact } from "../types";

type MessageArtifactsProps = {
  artifacts?: Artifact[] | null;
  onOpenArtifact: (artifact: Artifact) => void;
};

type SafeArtifact = Artifact & {
  id: string;
  kind: string;
  title: string;
  summary: string;
  content: string;
};

function asText(value: unknown, fallback = "") {
  if (typeof value === "string") return value;
  if (value === null || value === undefined) return fallback;
  return String(value);
}

function normalizeKind(kind: unknown) {
  return asText(kind, "text").trim().toLowerCase() || "text";
}

function safeArtifact(artifact: Partial<Artifact> | null | undefined, index: number): SafeArtifact {
  return {
    ...(artifact ?? {}),
    id: asText(artifact?.id, `artifact-${index}`),
    message_id: asText(artifact?.message_id),
    channel_id: asText(artifact?.channel_id),
    thread_root_id: typeof artifact?.thread_root_id === "string" ? artifact.thread_root_id : null,
    creator_agent_id: typeof artifact?.creator_agent_id === "string" ? artifact.creator_agent_id : null,
    creator_agent_handle: typeof artifact?.creator_agent_handle === "string" ? artifact.creator_agent_handle : null,
    kind: normalizeKind(artifact?.kind),
    title: asText(artifact?.title, "Untitled report"),
    summary: asText(artifact?.summary),
    content: asText(artifact?.content),
    metadata: artifact?.metadata && typeof artifact.metadata === "object" ? artifact.metadata : {},
    created_at: asText(artifact?.created_at),
    updated_at: asText(artifact?.updated_at),
  };
}

function ArtifactCard({ artifact, onOpenArtifact }: { artifact: SafeArtifact; onOpenArtifact: (artifact: Artifact) => void }) {
  return (
    // The click must not reach the row, which would also select a thread reply.
    <button type="button" className="message-artifact" onClick={(event) => {
      event.stopPropagation();
      onOpenArtifact(artifact);
    }}>
      <span className="artifact-icon"><FileText size={16} /></span>
      <span className="message-artifact-text">
        <strong>{artifact.title}</strong>
        {artifact.summary && <small>{artifact.summary}</small>}
      </span>
      <ChevronRight className="message-artifact-chevron" size={16} aria-hidden="true" />
    </button>
  );
}

export function MessageArtifacts({ artifacts, onOpenArtifact }: MessageArtifactsProps) {
  if (!Array.isArray(artifacts) || artifacts.length === 0) return null;

  return (
    <div className="message-artifacts">
      {artifacts.map((rawArtifact, index) => {
        const artifact = safeArtifact(rawArtifact, index);
        return <ArtifactCard key={artifact.id} artifact={artifact} onOpenArtifact={onOpenArtifact} />;
      })}
    </div>
  );
}
