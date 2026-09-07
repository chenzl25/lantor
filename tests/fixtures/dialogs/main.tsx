import { StrictMode, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { Modal } from "../../../src/components/Modal";
import { DraftAttachmentsPreview } from "../../../src/components/DraftAttachmentsPreview";
import { GithubIssueDrawer } from "../../../src/components/GithubIssueDrawer";
import { MessageAttachments } from "../../../src/components/MessageAttachments";
import { MessageMarkdown } from "../../../src/components/MessageMarkdown";
import { ThreadBrowserModal } from "../../../src/components/ThreadBrowserModal";
import { AppToast } from "../../../src/components/AppToast";
import { UI_ERROR_EVENT } from "../../../src/ui-notice";
import "../../../src/styles.css";

const image = { id: "image", message_id: "message", original_name: "preview.svg", mime_type: "image/svg+xml", size_bytes: 20,
  storage_path: "/fixture/image.svg", local_url: 'data:image/svg+xml,<svg xmlns="http://www.w3.org/2000/svg" width="500" height="240"><rect width="500" height="240" fill="teal"/></svg>', created_at: "" };
const draft = { id: "draft", original_name: "draft.svg", mime_type: "image/svg+xml", size_bytes: 40,
  file: new File(['<svg xmlns="http://www.w3.org/2000/svg" width="400" height="200"><rect width="400" height="200" fill="teal"/></svg>'], "draft.svg", { type: "image/svg+xml" }) };
const issue = { number: 42, title: "Example issue", url: "https://example.com/issue", state: "open", state_reason: null,
  author_login: "author", comments_count: 0, labels: [], assignee_logins: [], milestone: null, created_at: "2026-09-07T00:00:00Z",
  updated_at: "2026-09-07T00:00:00Z", linked_thread_root_id: null, linked_task_number: null };
function Fixture() {
  const [issueOpen, setIssueOpen] = useState(false);
  const [fatal, setFatal] = useState(false);
  const [open, setOpen] = useState(false);
  const [nested, setNested] = useState(false);
  const [locked, setLocked] = useState(false);
  const [threads, setThreads] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  useEffect(() => {
    const handler = (event: Event) => setNotice((event as CustomEvent<string>).detail);
    window.addEventListener(UI_ERROR_EVENT, handler);
    return () => window.removeEventListener(UI_ERROR_EVENT, handler);
  }, []);
  if (fatal) return <div className="fatal-shell"><div className="fatal-card">
    <h1>Something went wrong</h1><p>The workspace could not be displayed.</p><pre>Example render error</pre>
    <div className="fatal-actions"><button onClick={() => setFatal(false)}>Retry</button></div>
  </div></div>;
  return <main style={{ padding: 32 }}>
    <button onClick={() => setOpen(true)}>Open fixture</button>
    <button onClick={() => setThreads(true)}>Browse threads</button>
    <button onClick={() => setIssueOpen(true)}>Open issue</button>
    <button onClick={() => setFatal(true)}>Show fatal style</button>
    <button onClick={() => document.documentElement.dataset.theme = "dark"}>Dark theme</button>
    <Modal open={open} title="Outer dialog" onClose={() => setOpen(false)}>
      <p>Select this text without dismissing the dialog.</p>
      <input aria-label="First input" autoFocus />
      <button onClick={() => setNested(true)}>Open nested</button>
      <MessageMarkdown body="[Broken link](https://example.com)" />
      <MessageAttachments attachments={[image, { ...image, id: "file", original_name: "report.txt", mime_type: "text/plain", local_url: undefined }]} showImageThumbnails />
      <DraftAttachmentsPreview attachments={[draft]} onRemove={() => {}} />
      <button onClick={() => setNotice("Could not save changes")}>Trigger error</button>
      <button onClick={() => setOpen(false)}>Last button</button>
    </Modal>
    <Modal open={nested} title="Nested dialog" onClose={() => setNested(false)} closeOnEscape={!locked} closeOnBackdrop={!locked}>
      <input aria-label="Nested input" autoFocus />
      <button onClick={() => setLocked(!locked)}>{locked ? "Unlock dismissal" : "Lock dismissal"}</button>
    </Modal>
    <ThreadBrowserModal open={threads} channels={[]} threads={[]} activeThreadId={null} replyCounts={{}} unreadCounts={{}}
      onOpenThread={() => {}} onToggleFollow={() => {}} onClose={() => setThreads(false)} />
    <GithubIssueDrawer issue={issueOpen ? issue : null} detail={null} loading={false} error={null} agents={[]}
      onClose={() => setIssueOpen(false)} onOpenGithub={() => {}} onRetry={() => {}}
      onCreateTask={async () => { throw Error("unused"); }} onOpenThread={() => {}} />
    {notice && <AppToast message={notice} onDismiss={() => setNotice(null)} />}
  </main>;
}
createRoot(document.getElementById("root")!).render(<StrictMode><Fixture /></StrictMode>);
