import { useEffect, useId, useState } from "react";
import { Check, Copy, Download, LoaderCircle, RotateCcw, Share } from "lucide-react";

import { apiInvoke, saveTextDownload } from "../apiClient";
import { artifactFileName, cachedArtifact, readArtifact, withoutRepeatedTitle } from "../artifacts";
import { copyText } from "../clipboard";
import type { Artifact } from "../types";
import { formatTime } from "../ui-utils";
import { DialogHeader } from "./DialogHeader";
import { DialogSurface } from "./DialogSurface";
import { deliveryLabel, useFileDelivery, type ReadyFile } from "./FileDelivery";
import { MessageMarkdown } from "./MessageMarkdown";

type LoadState =
  | { status: "loading" }
  | { status: "ready"; artifact: Artifact }
  | { status: "error"; message: string };

function errorMessage(error: unknown) {
  return error instanceof Error ? error.message : String(error || "Unknown error");
}

function readFromServer(artifactId: string) {
  return apiInvoke("artifact_read", { artifactId });
}

/** Mounted once per app; `artifact` may come without its content. */
export function ArtifactReader({ artifact, onClose }: { artifact: Artifact | null; onClose: () => void }) {
  if (!artifact) return null;
  return <ArtifactReaderSheet key={artifact.id} artifact={artifact} onClose={onClose} />;
}

function ArtifactReaderSheet({ artifact, onClose }: { artifact: Artifact; onClose: () => void }) {
  const titleId = useId();
  const [attempt, setAttempt] = useState(0);
  const [state, setState] = useState<LoadState>(() => {
    const cached = cachedArtifact(artifact);
    return cached ? { status: "ready", artifact: cached } : { status: "loading" };
  });
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    const cached = cachedArtifact(artifact);
    if (cached) {
      setState({ status: "ready", artifact: cached });
      return;
    }
    let cancelled = false;
    setState({ status: "loading" });
    readArtifact(artifact, readFromServer).then(
      (full) => { if (!cancelled) setState({ status: "ready", artifact: full }); },
      (error: unknown) => { if (!cancelled) setState({ status: "error", message: errorMessage(error) }); },
    );
    return () => { cancelled = true; };
  }, [artifact, attempt]);

  useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(false), 1600);
    return () => window.clearTimeout(timer);
  }, [copied]);

  const full = state.status === "ready" ? state.artifact : null;
  // Share needs the file up front: iOS opens the share sheet only within the tap.
  const [ready, setReady] = useState<ReadyFile | null>(null);
  useEffect(() => {
    if (!full) {
      setReady(null);
      return;
    }
    const file = new File([full.content], artifactFileName(full), {
      type: full.kind === "markdown" ? "text/markdown" : "text/plain",
    });
    const url = URL.createObjectURL(file);
    setReady({ file, url });
    return () => URL.revokeObjectURL(url);
  }, [full]);
  const { delivery, saving, deliver, notice, clearNotice, showError } = useFileDelivery(ready,
    async (file) => saveTextDownload(file.name, await file.text()));

  async function copyContent() {
    if (!full) return;
    clearNotice();
    try {
      await copyText(full.content);
      setCopied(true);
    } catch (error) {
      showError("Couldn’t copy", error);
    }
  }

  const author = artifact.creator_agent_handle ? `@${artifact.creator_agent_handle.replace(/^@/, "")}` : null;
  const subtitle = [author, artifact.created_at ? formatTime(artifact.created_at) : null].filter(Boolean).join(" · ");
  const copyLabel = copied ? "Copied" : full?.kind === "markdown" ? "Copy Markdown" : "Copy text";
  return <DialogSurface label={artifact.title} labelledBy={titleId} className="modal-card attachment-sheet artifact-reader"
    onClose={onClose}>
    <DialogHeader className="artifact-reader-head" title={artifact.title} titleId={titleId}
      subtitle={subtitle || undefined} onClose={onClose}
      actions={<>
        <button type="button" className="artifact-reader-action" disabled={!full} onClick={() => void copyContent()}
          aria-label={copyLabel}>
          {copied ? <Check size={16} /> : <Copy size={16} />}
          <span>{copied ? "Copied" : "Copy"}</span>
        </button>
        <button type="button" className="artifact-reader-action" disabled={!ready || saving} onClick={deliver}
          aria-label={deliveryLabel(delivery, saving)}>
          {delivery === "share" ? <Share size={16} /> : <Download size={16} />}
          <span>{deliveryLabel(delivery, saving)}</span>
        </button>
      </>} />
    <div className="modal-body attachment-sheet-body artifact-reader-body">
      {notice}
      {state.status === "loading" && <div className="artifact-reader-status" role="status">
        <LoaderCircle className="spin" size={20} />
        <span>Loading report…</span>
      </div>}
      {state.status === "error" && <div className="artifact-reader-status error" role="alert">
        <p>Couldn’t open this report: {state.message}</p>
        <button type="button" className="attachment-sheet-retry" onClick={() => setAttempt((value) => value + 1)}>
          <RotateCcw size={15} /> Retry
        </button>
      </div>}
      {full && (full.kind === "markdown"
        ? <div className="artifact-reader-content">
          <MessageMarkdown body={withoutRepeatedTitle(full.content, full.title)}
            scrollKey={`artifact-reader:${full.id}`} channelId={full.channel_id} />
        </div>
        : <pre className="attachment-sheet-text">{full.content}</pre>)}
    </div>
  </DialogSurface>;
}
