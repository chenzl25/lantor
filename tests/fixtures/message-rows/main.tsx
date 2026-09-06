import { Profiler, useLayoutEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { Conversation } from "../../../src/components/Conversation";
import { ThreadPanel } from "../../../src/components/ThreadPanel";
import type { Agent, AgentActivity, AgentRun, AgentWorkItem, Message, ThreadReplySummary } from "../../../src/types";
import "../../../src/styles.css";

export const id = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const channelId = id(1000);
const agent: Agent = { id: id(2000), handle: "Hancock", display_name: "Hancock", role: "agent", status: "idle", runtime: "test", model: "", reasoning_effort: "", service_tier: "", avatar: "H", description: "Test profile", launch_command: "", environment_variables: "", working_directory: "", workspace_exists: false, workspace_memory_path: "", workspace_memory_exists: false, workspace_entries: [], daily_budget_micros: 0, subscription_status: null };
const message = (n: number, thread: string | null = null): Message => ({
  id: id(n), seq: n, channel_id: channelId, thread_root_id: thread, sender_agent_id: n % 2 ? agent.id : null,
  sender_name: n % 2 ? "Hancock" : "Owner", sender_role: n % 2 ? "agent" : "owner",
  body: `Message ${n}, @Hancock.\n\n| Value | Result |\n|---|---|\n| ${n} | **Ready** |`,
  is_task: false, thread_followed: false, delivery_state: "complete", stream_key: "", task_number: null, task_status: null,
  attachments: [], artifacts: [], created_at: `2026-09-06T12:${String(n % 60).padStart(2, "0")}:00Z`, updated_at: `2026-09-06T12:${String(n % 60).padStart(2, "0")}:00Z`,
});
const initial = {
  messages: [...Array.from({ length: 30 }, (_, i) => message(i + 1)), ...Array.from({ length: 10 }, (_, i) => message(101 + i, id(1)))],
  agents: [agent], channels: [{ id: channelId, name: "render-test", description: "", kind: "channel" as const, dm_agent_id: null, unread_count: 0, github_unread_count: 0, github_review_synced_at: null }],
  owner: { display_name: "Owner", avatar: "O", description: "" },
  activities: [] as AgentActivity[], runs: [] as AgentRun[], workItems: [] as AgentWorkItem[],
};
initial.messages[1].body = `References [[message:${id(3)}]] and [[thread:${id(1)}]] and [[message:${id(999)}]]. @Hancock`;
initial.messages[3].body = "Long message\n\n" + "Paragraph content for expansion.\n\n".repeat(80);
initial.messages[4].body = "Math: $$x^2$$";
initial.messages[5].sender_role = "system";
for (const root of [id(1), id(7), id(9)]) {
  const runId = id(3000 + initial.runs.length);
  initial.runs.push({ id: runId, agent_id: agent.id, agent_handle: agent.handle, work_item_id: null, command: "", working_directory: "", status: "running", pid: null, exit_code: null, log: "", input_tokens: 0, output_tokens: 0, cost_micros: 0, started_at: "2026-09-06T12:00:00Z", stopped_at: null });
  initial.workItems.push({ id: id(4000 + initial.workItems.length), agent_id: agent.id, agent_handle: agent.handle, channel_id: channelId, channel_name: "render-test", thread_root_id: root, source_message_id: root, task_id: null, task_number: null, source_kind: "mention", title: "Test", context: "", status: "running", run_id: runId, created_at: "2026-09-06T12:00:00Z", updated_at: "2026-09-06T12:00:00Z", completed_at: null });
  initial.activities.push({ id: id(5000 + initial.activities.length), agent_id: agent.id, agent_handle: agent.handle, run_id: runId, kind: "thinking", phase: "thinking", status: "running", title: "Reading source", summary: "Reading source", detail: "Inspecting changes", metadata: {}, created_at: "2026-09-06T12:01:00Z" });
}
const noop = () => {};
const probe = (window as any).__rowProbe;
function Fixture() {
  const [data, setData] = useState(initial);
  const [generation, setGeneration] = useState(0);
  const [draft, setDraft] = useState("");
  const [replyDraft, setReplyDraft] = useState("");
  const [saved, setSaved] = useState(new Set<string>());
  const [activeId, setActiveId] = useState<string | null>(id(1));
  const roots = data.messages.filter((m) => !m.thread_root_id);
  const activeRoot = data.messages.find((m) => m.id === activeId) ?? null;
  const replies = data.messages.filter((m) => m.thread_root_id === activeId);
  const summaries: Record<string, ThreadReplySummary> = {};
  const counts: Record<string, number> = {};
  for (const root of roots) {
    const items = data.messages.filter((m) => m.thread_root_id === root.id);
    if (items.length) { counts[root.id] = items.length; summaries[root.id] = { count: items.length, latest: items.at(-1)!, participants: items.slice(0, 3) }; }
  }
  function send() {
    setData((before) => { const next = structuredClone(before); next.messages.push(message(31)); next.channels[0].unread_count += 1; return next; });
    setGeneration((n) => n + 1);
  }
  useLayoutEffect(() => {
    probe.generation = generation;
    probe.draft = draft;
    probe.replyDraft = replyDraft;
    probe.refresh = () => { setData((before) => structuredClone(before)); setGeneration((n) => n + 1); };
    probe.send = send;
    probe.edit = (number: number, patch: Partial<Message>) => setData((before) => ({ ...before, messages: before.messages.map((m) => m.id === id(number) ? { ...m, ...patch } : m) }));
    probe.add = (number: number, thread: number | null = null) => setData((before) => ({ ...before, messages: [...before.messages, message(number, thread === null ? null : id(thread))] }));
    probe.rename = () => setData((before) => ({ ...before, channels: before.channels.map((c) => ({ ...c, name: "renamed-channel" })) }));
    probe.activity = () => setData((before) => ({ ...before, activities: before.activities.map((a, i) => i === 1 ? { ...a, summary: "Updated progress" } : a) }));
    probe.owner = () => setData((before) => ({ ...before, owner: { ...before.owner, avatar: "Z" } }));
    probe.profile = () => setData((before) => ({ ...before, agents: before.agents.map((a) => ({ ...a, description: "Updated profile" })) }));
    probe.removeAgent = () => setData((before) => ({ ...before, agents: [], messages: before.messages.map((m) => ({ ...m, sender_agent_id: null })) }));
  });
  const shared = {
    channel: data.channels[0], channels: data.channels, agents: data.agents, channelAgents: data.agents, ownerProfile: data.owner,
    agentActivities: data.activities, agentRuns: data.runs, agentWorkItems: data.workItems, messages: data.messages,
    activeRoot, taskTitleDrafts: {}, setTaskTitleDraft: noop, saveTaskTitle: noop, claimTask: noop, updateTaskStatus: noop,
    openAgentDetail: (a: Agent) => probe.events.push({ kind: "agent", generation, description: a.description }),
    openArtifact: (artifact: unknown) => probe.events.push({ kind: "artifact", generation, artifact }),
    onReferenceMessageJump: (source: string, target: string) => probe.events.push({ kind: "reference", generation, source, target }),
    onReferenceThreadJump: (source: string, target: string) => probe.events.push({ kind: "thread-reference", generation, source, target }),
    shareBaseUrl: null, savedMessageIds: saved, focusedMessageId: null, showImageThumbnails: true,
    onToggleMessageSaved: (m: Message, state: boolean) => setSaved((before) => { const next = new Set(before); state ? next.add(m.id) : next.delete(m.id); return next; }),
  };
  return <Profiler id="fixture" onRender={() => { probe.parentCommits += 1; }}>
    <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", height: "100vh" }}>
      <Conversation {...shared} activeTab="chat" rootMessages={roots} threadReplyCounts={counts} threadUnreadCounts={{}}
        threadReplySummaries={summaries} visibleTasks={[]} draft={draft} draftAttachments={[]}
        setActiveTab={noop} setActiveThreadId={setActiveId} openMobileSidebar={noop} canNavigateBack={false} canNavigateForward={false}
        navigateBack={noop} navigateForward={noop} openChannelSettingsModal={noop} deleteChannel={noop} openChannelAgentsModal={noop}
        taskForMessage={() => null} openTask={noop} createGithubReviewTask={async () => { throw Error("unused"); }} createGithubIssueTask={async () => { throw Error("unused"); }}
        setDraft={setDraft} addDraftAttachments={noop} removeDraftAttachment={noop} sendRootMessage={send}
        hasMoreRootMessages={false} isLoadingOlderRootMessages={false} onLoadOlderRootMessages={async () => {}} />
      <ThreadPanel {...shared} activeTask={null} replies={replies} unreadCount={0} replyDraft={replyDraft} replyAttachments={[]}
        onClose={noop} setReplyDraft={setReplyDraft} addReplyAttachments={noop} removeReplyAttachment={noop} sendReply={noop} onLocateRoot={noop} onResizeStart={noop} />
    </div>
  </Profiler>;
}
createRoot(document.getElementById("root")!).render(<Fixture />);
