import { Component, ReactNode } from "react";
import { FileText } from "lucide-react";
import { Artifact } from "../types";
import { MessageMarkdown } from "./MessageMarkdown";

type MessageArtifactsProps = {
  artifacts?: Artifact[] | null;
  onOpenArtifact?: (artifact: Artifact) => void;
};

type SafeArtifact = Artifact & {
  id: string;
  kind: string;
  title: string;
  summary: string;
  content: string;
};

type ArtifactErrorBoundaryProps = {
  artifactTitle: string;
  children: ReactNode;
};

type ArtifactErrorBoundaryState = {
  failed: boolean;
};

class ArtifactErrorBoundary extends Component<ArtifactErrorBoundaryProps, ArtifactErrorBoundaryState> {
  state: ArtifactErrorBoundaryState = { failed: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  render() {
    if (this.state.failed) {
      return (
        <div className="artifact-render-error">
          Could not render artifact "{this.props.artifactTitle}". Open the full artifact or retry after reload.
        </div>
      );
    }
    return this.props.children;
  }
}

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
    title: asText(artifact?.title, "Untitled artifact"),
    summary: asText(artifact?.summary),
    content: asText(artifact?.content),
    metadata: artifact?.metadata && typeof artifact.metadata === "object" ? artifact.metadata : {},
    created_at: asText(artifact?.created_at),
    updated_at: asText(artifact?.updated_at),
  };
}

function safeKindClass(kind: string) {
  return kind.replace(/[^a-z0-9_-]/g, "-") || "text";
}

function artifactIcon(kind: string) {
  switch (kind) {
    case "markdown":
      return FileText;
    default:
      return FileText;
  }
}

function previewContent(artifact: Artifact) {
  const content = asText(artifact.summary || artifact.content);
  const compact = content.trim().replace(/\s+/g, " ");
  return compact.length > 140 ? `${compact.slice(0, 140)}...` : compact;
}

function ArtifactContent({ artifact }: { artifact: Artifact }) {
  if (artifact.kind === "markdown") {
    return (
      <div className="artifact-markdown-content">
        <MessageMarkdown body={artifact.content || previewContent(artifact)} scrollKey={`artifact:${artifact.id}`} />
      </div>
    );
  }

  return <pre>{artifact.content || previewContent(artifact)}</pre>;
}

function ArtifactCard({ artifact, onOpenArtifact }: { artifact: SafeArtifact; onOpenArtifact?: (artifact: Artifact) => void }) {
  const Icon = artifactIcon(artifact.kind);
  return (
    <details className={`message-artifact artifact-${safeKindClass(artifact.kind)}`}>
      <summary>
        <span className="artifact-icon"><Icon size={16} /></span>
        <span>
          <strong>{artifact.title}</strong>
          <small>{artifact.kind} · artifact {artifact.id.slice(0, 8)}</small>
        </span>
      </summary>
      {onOpenArtifact && (
        <div className="artifact-actions">
          <button type="button" onClick={() => onOpenArtifact(artifact)}>Open full artifact</button>
        </div>
      )}
      {artifact.summary && <p>{artifact.summary}</p>}
      <ArtifactContent artifact={artifact} />
    </details>
  );
}

export function MessageArtifacts({ artifacts, onOpenArtifact }: MessageArtifactsProps) {
  if (!Array.isArray(artifacts) || artifacts.length === 0) return null;

  return (
    <div className="message-artifacts">
      {artifacts.map((rawArtifact, index) => {
        const artifact = safeArtifact(rawArtifact, index);
        return (
          <ArtifactErrorBoundary key={artifact.id || `artifact-${index}`} artifactTitle={artifact.title}>
            <ArtifactCard artifact={artifact} onOpenArtifact={onOpenArtifact} />
          </ArtifactErrorBoundary>
        );
      })}
    </div>
  );
}
