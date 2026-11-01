import {
  ArrowLeft,
  BookOpen,
  History,
  LoaderCircle,
  Pencil,
  TriangleAlert,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";

import { apiInvoke } from "../apiClient";
import { formatRelativeTime } from "../ui-utils";
import type { Channel, ChannelWikiOverview, ChannelWikiRevision } from "../types";
import { MessageMarkdown } from "./MessageMarkdown";

type WikiPanelProps = {
  channel: Channel;
};

type WikiView = "read" | "history" | "edit";

function contentBytes(content: string) {
  return new TextEncoder().encode(content).length;
}

function revisionMeta(revision: ChannelWikiRevision, isHead: boolean) {
  return (
    <div className="wiki-revision-meta">
      <code>rev {revision.short_id}</code>
      {isHead && <span className="wiki-head-badge">Current</span>}
      <span>{revision.author}</span>
      <span title={revision.created_at}>{formatRelativeTime(revision.created_at)}</span>
      {revision.note.trim() && <span className="wiki-revision-note">{revision.note.trim()}</span>}
    </div>
  );
}

export function WikiPanel({ channel }: WikiPanelProps) {
  const [overview, setOverview] = useState<ChannelWikiOverview | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [view, setView] = useState<WikiView>("read");
  const [viewedRevisionId, setViewedRevisionId] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [draftNote, setDraftNote] = useState("");
  const [saving, setSaving] = useState(false);
  const [conflict, setConflict] = useState(false);

  const head = overview?.head ?? null;
  const revisions = overview?.revisions ?? [];
  const maxBytes = overview?.max_bytes ?? 16 * 1024;
  const viewedRevision = useMemo(() => {
    if (!viewedRevisionId) return null;
    return revisions.find((revision) => revision.id === viewedRevisionId) ?? null;
  }, [revisions, viewedRevisionId]);

  const loadWiki = useCallback(async (channelId: string) => {
    setLoading(true);
    setError(null);
    try {
      const result = await apiInvoke("load_channel_wiki", { channelId });
      setOverview(result);
    } catch (loadError) {
      setError(String(loadError));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    setOverview(null);
    setView("read");
    setViewedRevisionId(null);
    setConflict(false);
    void loadWiki(channel.id);
  }, [channel.id, loadWiki]);

  const startEdit = useCallback(() => {
    setDraft(head?.content ?? "");
    setDraftNote("");
    setConflict(false);
    setView("edit");
  }, [head]);

  const saveDraft = useCallback(async () => {
    if (saving) return;
    setSaving(true);
    setError(null);
    try {
      const result = await apiInvoke("publish_channel_wiki", {
        channelId: channel.id,
        parentId: head?.id ?? null,
        content: draft,
        note: draftNote.trim(),
      });
      setOverview(result.overview);
      if (result.outcome === "conflict") {
        setConflict(true);
      } else {
        setConflict(false);
        setView("read");
        setViewedRevisionId(null);
      }
    } catch (saveError) {
      setError(String(saveError));
    } finally {
      setSaving(false);
    }
  }, [channel.id, draft, draftNote, head, saving]);

  const draftBytes = contentBytes(draft);
  const overLimit = draftBytes > maxBytes;

  if (loading && !overview) {
    return (
      <div className="wiki-panel wiki-panel-loading">
        <LoaderCircle size={18} className="spin" />
      </div>
    );
  }

  if (view === "edit") {
    return (
      <div className="wiki-panel">
        <div className="wiki-toolbar">
          <div className="wiki-toolbar-title">
            <Pencil size={15} />
            <strong>{head ? "Edit" : "Write wiki"}</strong>
            <span className={`wiki-byte-count ${overLimit ? "over-limit" : ""}`}>
              {draftBytes.toLocaleString()} / {maxBytes.toLocaleString()} bytes
            </span>
          </div>
          <div className="wiki-toolbar-actions">
            <button type="button" onClick={() => setView("read")} disabled={saving}>
              Cancel
            </button>
            <button
              type="button"
              className="primary"
              onClick={() => void saveDraft()}
              disabled={saving || overLimit || !draft.trim()}
            >
              {saving ? <LoaderCircle size={14} className="spin" /> : null}
              Save
            </button>
          </div>
        </div>
        {conflict && head && (
          <div className="wiki-conflict-banner">
            <TriangleAlert size={15} />
            <span>
              The wiki moved to rev {head.short_id} ({head.author}) while you were editing. Merge
              the latest content into your draft, then publish again.
            </span>
          </div>
        )}
        {error && <div className="wiki-error">{error}</div>}
        <textarea
          className="wiki-editor"
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          placeholder={`# ${channel.name} Channel Wiki\n\nRequired reading before working in this channel: stable facts, conventions, key decisions, links. Keep it index-like; details belong in notes and artifacts.`}
          spellCheck={false}
        />
        <input
          className="wiki-note-input"
          value={draftNote}
          onChange={(event) => setDraftNote(event.target.value)}
          placeholder="What changed? (one line)"
          maxLength={200}
        />
        {conflict && head && (
          <details className="wiki-conflict-latest">
            <summary>Show latest content (rev {head.short_id})</summary>
            <pre>{head.content}</pre>
          </details>
        )}
      </div>
    );
  }

  if (view === "history") {
    return (
      <div className="wiki-panel">
        <div className="wiki-toolbar">
          <div className="wiki-toolbar-title">
            <History size={15} />
            <strong>History</strong>
            <span className="wiki-byte-count">
              {revisions.length} revision{revisions.length === 1 ? "" : "s"}
            </span>
          </div>
          <div className="wiki-toolbar-actions">
            <button
              type="button"
              onClick={() => {
                setView("read");
                setViewedRevisionId(null);
              }}
            >
              <ArrowLeft size={14} /> Current version
            </button>
          </div>
        </div>
        {viewedRevision ? (
          <>
            <div className="wiki-meta-row">
              {revisionMeta(viewedRevision, viewedRevision.id === head?.id)}
              <button type="button" onClick={() => setViewedRevisionId(null)}>
                Back to list
              </button>
            </div>
            <div className="wiki-content wiki-content-historic">
              <MessageMarkdown body={viewedRevision.content} channelId={channel.id} />
            </div>
          </>
        ) : (
          <ul className="wiki-history-list">
            {revisions.map((revision) => (
              <li key={revision.id}>
                <button type="button" onClick={() => setViewedRevisionId(revision.id)}>
                  {revisionMeta(revision, revision.id === head?.id)}
                  <span className="wiki-history-size">
                    {contentBytes(revision.content).toLocaleString()}B
                  </span>
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
    );
  }

  if (!head) {
    return (
      <div className="wiki-panel">
        {error && <div className="wiki-error">{error}</div>}
        <div className="wiki-empty">
          <BookOpen size={28} />
          <h3>No wiki yet</h3>
          <p>
            Key facts every agent here reads first.
          </p>
          <button type="button" className="primary" onClick={startEdit}>
            <Pencil size={14} /> Write wiki
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="wiki-panel">
      <div className="wiki-toolbar">
        <div className="wiki-toolbar-title">
          <BookOpen size={15} />
          <strong>Wiki</strong>
        </div>
        <div className="wiki-toolbar-actions">
          <button type="button" onClick={() => setView("history")}>
            <History size={14} /> History
          </button>
          <button type="button" className="primary" onClick={startEdit}>
            <Pencil size={14} /> Edit
          </button>
        </div>
      </div>
      {error && <div className="wiki-error">{error}</div>}
      <div className="wiki-meta-row">{revisionMeta(head, true)}</div>
      <div className="wiki-content">
        <MessageMarkdown body={head.content} channelId={channel.id} />
      </div>
    </div>
  );
}
