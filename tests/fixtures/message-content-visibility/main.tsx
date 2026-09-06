import { Profiler, useLayoutEffect, useState } from "react";
import { flushSync } from "react-dom";
import { streamingMessages } from "../../../src/streaming-message-store";
import { createRoot } from "react-dom/client";
import { Conversation } from "../../../src/components/Conversation";
import { ThreadPanel } from "../../../src/components/ThreadPanel";
import type { Agent, AgentActivity, AgentRun, AgentWorkItem, Message, ThreadReplySummary } from "../../../src/types";
import "../../../src/styles.css";

export const id = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const channelId = id(10000);
const params = new URLSearchParams(location.search);
const threadMode = params.has("thread");
const count = Number(params.get("count") || 2200);
if (params.has("baseline")) {
  const style = document.createElement("style");
  style.textContent = ".message-render-boundary { content-visibility: visible !important; contain-intrinsic-block-size: none !important; }";
  document.head.append(style);
}
const agent: Agent = { id: id(20000), handle: "Hancock", display_name: "Hancock", role: "agent", status: "idle", runtime: "test", model: "", reasoning_effort: "", service_tier: "", avatar: "H", description: "Test profile", launch_command: "", environment_variables: "", working_directory: "", workspace_exists: false, workspace_memory_path: "", workspace_memory_exists: false, workspace_entries: [], daily_budget_micros: 0, subscription_status: null };
const message = (n: number, thread: string | null = null): Message => ({
  id: id(n), seq: n, channel_id: channelId, thread_root_id: thread, sender_agent_id: n % 2 ? agent.id : null,
  sender_name: n % 2 ? "Hancock" : "Owner", sender_role: n % 2 ? "agent" : "owner",
  body: n % 4 === 0 ? `Message ${n}, @Hancock.\n\n| Value | Result |\n|---|---|\n| ${n} | **Ready** |`
    : n % 4 === 1 ? `Message ${n}: short note.`
    : n % 4 === 2 ? `Message ${n}:\n\n` + "Paragraph with **bold**, `inline code`, and a [link](https://example.com).\n\n".repeat(5)
    : `Message ${n}:\n\n- First item\n- Second item\n\n\x60\x60\x60ts\nconst value = ${n};\n\x60\x60\x60` ,
  is_task: false, thread_followed: false, delivery_state: "complete", stream_key: "", task_number: null, task_status: null,
  attachments: [], artifacts: [], created_at: `2026-09-06T12:${String(n % 60).padStart(2, "0")}:00Z`, updated_at: `2026-09-06T12:${String(n % 60).padStart(2, "0")}:00Z`,
});
const initial = {
  messages: [...(threadMode ? [message(1)] : []), ...Array.from({ length: count }, (_, i) => message(201 + i, threadMode ? id(1) : null))],
  agents: [agent], channels: [{ id: channelId, name: "render-test", description: "", kind: "channel" as const, dm_agent_id: null, unread_count: 0, github_unread_count: 0, github_review_synced_at: null }],
  owner: { display_name: "Owner", avatar: "O", description: "" },
  activities: [] as AgentActivity[], runs: [] as AgentRun[], workItems: [] as AgentWorkItem[],
};
initial.messages.find((m) => m.id === id(222))!.attachments = [{
  id: id(30000), message_id: id(222), original_name: "sample.svg", mime_type: "image/svg+xml", size_bytes: 100,
  storage_path: "", local_url: "data:image/svg+xml," + encodeURIComponent('<svg xmlns="http://www.w3.org/2000/svg" width="120" height="60"><rect width="120" height="60" fill="royalblue"/></svg>'),
  created_at: "2026-09-06T12:00:00Z",
}];
initial.messages.at(-1)!.body += `\n\n[[message:${id(220)}]]`;
const noop = () => {};
const probe = (window as any).__rowProbe;
function Fixture() {
  const [data, setData] = useState(initial);
  const [generation, setGeneration] = useState(0);
  const [draft, setDraft] = useState("");
  const [replyDraft, setReplyDraft] = useState("");
  const [saved, setSaved] = useState(new Set<string>());
  const [activeId, setActiveId] = useState<string | null>(threadMode ? id(1) : null);
  const [focusedId, setFocusedId] = useState<string | null>(null);
  const [pages, setPages] = useState(0);
  async function older() {
    const list = document.querySelector(".message-list")!;
    const anchor = list.querySelector("article[data-message-id]") as HTMLElement;
    probe.prependAnchor = { id: anchor.dataset.messageId, offset: anchor.getBoundingClientRect().top - list.getBoundingClientRect().top };
    if (probe.loadDelay) await new Promise((resolve) => setTimeout(resolve, probe.loadDelay));
    const first = 201 - (pages + 1) * 40;
    flushSync(() => {
      setData((before) => ({ ...before, messages: [...Array.from({ length: 40 }, (_, i) => message(first + i)), ...before.messages] }));
      setPages((n) => n + 1);
    });
  }
  const roots = data.messages.filter((m) => !m.thread_root_id);
  const activeRoot = data.messages.find((m) => m.id === activeId) ?? null;
  const replies = data.messages.filter((m) => m.thread_root_id === activeId);
  const summaries: Record<string, ThreadReplySummary> = {};
  const counts: Record<string, number> = {};
  function send() {
    setData((before) => { const next = structuredClone(before); next.messages.push(message(9000)); next.channels[0].unread_count += 1; return next; });
    setGeneration((n) => n + 1);
  }
  useLayoutEffect(() => {
    probe.pages = pages;
    probe.jump = (number: number) => setFocusedId(id(number));
    probe.clearFocus = () => setFocusedId(null);
    probe.stream = () => setData((before) => ({ ...before, messages: [...before.messages, { ...message(9001, threadMode ? id(1) : null), body: "Streaming start", delivery_state: "streaming" }] }));
    probe.chunk = (body: string) => streamingMessages.publish(id(9001), { body, delivery_state: "streaming" });
    probe.generation = generation;
    probe.draft = draft;
    probe.replyDraft = replyDraft;

  });
  const shared = {
    channel: data.channels[0], channels: data.channels, agents: data.agents, channelAgents: data.agents, ownerProfile: data.owner,
    agentActivities: data.activities, agentRuns: data.runs, agentWorkItems: data.workItems, messages: data.messages,
    activeRoot, taskTitleDrafts: {}, setTaskTitleDraft: noop, saveTaskTitle: noop, claimTask: noop, updateTaskStatus: noop,
    openAgentDetail: (a: Agent) => probe.events.push({ kind: "agent", generation, description: a.description }),
    openArtifact: (artifact: unknown) => probe.events.push({ kind: "artifact", generation, artifact }),
    onReferenceMessageJump: (_source: string, target: string) => setFocusedId(target),
    onReferenceThreadJump: (source: string, target: string) => probe.events.push({ kind: "thread-reference", generation, source, target }),
    shareBaseUrl: null, savedMessageIds: saved, focusedMessageId: focusedId, showImageThumbnails: true,
    onToggleMessageSaved: (m: Message, state: boolean) => setSaved((before) => { const next = new Set(before); state ? next.add(m.id) : next.delete(m.id); return next; }),
  };
  return <Profiler id="fixture" onRender={(_id, phase, actualDuration, baseDuration, startTime, commitTime) => { probe.parentCommits += 1; probe.commits.push({phase, actualDuration, baseDuration, startTime, commitTime}); }}>
    <div style={{ display: "grid", gridTemplateColumns: "1fr", height: "100vh" }}>
      {!threadMode && <Conversation {...shared} activeTab="chat" rootMessages={roots} threadReplyCounts={counts} threadUnreadCounts={{}}
        threadReplySummaries={summaries} visibleTasks={[]} draft={draft} draftAttachments={[]}
        setActiveTab={noop} setActiveThreadId={setActiveId} openMobileSidebar={noop} canNavigateBack={false} canNavigateForward={false}
        navigateBack={noop} navigateForward={noop} openChannelSettingsModal={noop} deleteChannel={noop} openChannelAgentsModal={noop}
        taskForMessage={() => null} openTask={noop} createGithubReviewTask={async () => { throw Error("unused"); }} createGithubIssueTask={async () => { throw Error("unused"); }}
        setDraft={setDraft} addDraftAttachments={noop} removeDraftAttachment={noop} sendRootMessage={send}
        hasMoreRootMessages={pages < 5} isLoadingOlderRootMessages={false} onLoadOlderRootMessages={older} />}
      {threadMode && <ThreadPanel {...shared} activeTask={null} replies={replies} unreadCount={0} replyDraft={replyDraft} replyAttachments={[]}
        onClose={noop} setReplyDraft={setReplyDraft} addReplyAttachments={noop} removeReplyAttachment={noop} sendReply={noop} onLocateRoot={noop} onResizeStart={noop} />}
    </div>
  </Profiler>;
}
createRoot(document.getElementById("root")!).render(<Fixture />);
