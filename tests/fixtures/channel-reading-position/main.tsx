import { useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { Conversation } from "../../../src/components/Conversation";
import { useVisibleChannelRead } from "../../../src/hooks/useVisibleChannelRead";
import type { ChannelReadLocation } from "../../../src/hooks/useChannelMessageScroll";
import { CHANNEL_READING_POSITION_KEY, captureMessageAnchor } from "../../../src/channel-reading-position";
import { streamingMessages } from "../../../src/streaming-message-store";
import type { Message } from "../../../src/types";
import "../../../src/styles.css";

const params = new URLSearchParams(location.search);
const id = (channel: number, seq: number) => `${channel}-${seq}`;
const message = (seq: number, channel = 0): Message => ({ id: id(channel, seq), seq,
  channel_id: `channel-${channel}`, thread_root_id: null, sender_agent_id: null, sender_name: "Agent", sender_role: "agent",
  body: `Message ${seq}\n\n` + "A paragraph of reading context.\n\n".repeat(seq % 4 + 1),
  is_task: false, thread_followed: false, delivery_state: "complete", stream_key: "", task_number: null, task_status: null,
  attachments: [], artifacts: [], created_at: seq <= 40 ? "2026-06-05T08:00:00Z" : "2026-09-08T00:00:00Z", updated_at: "2026-09-08T00:00:00Z" });
const channels = ["reading-test", "other"].map((name, n) => ({ id: `channel-${n}`, name, description: "Synthetic reading-position regression", kind: "channel" as const,
  dm_agent_id: null, unread_count: 1, github_unread_count: 0, github_review_synced_at: null }));
if (params.has("reset")) localStorage.removeItem(CHANNEL_READING_POSITION_KEY);
if (params.has("seed")) localStorage.setItem(CHANNEL_READING_POSITION_KEY, JSON.stringify([[channels[0].id,
  { anchor: { messageId: id(0, 5), seq: 5, offset: -12 }, atBottom: false, latestRootId: id(0, 120) }]]));
const receipts: {channelId: string; throughSeq: number}[] = [];
let streamBody = "Streaming start";
window.fetch = async (input, init) => {
  if (!String(input).endsWith("/api/mark_channel_read")) throw Error(`Unexpected fixture request: ${input}`);
  receipts.push(JSON.parse(String(init?.body)));
  return new Response(JSON.stringify({ ok: true }), { headers: { "Content-Type": "application/json" } });
};
const noop = () => {};
function Fixture() {
  const [channel, setChannel] = useState(0);
  const [ready, setReady] = useState(params.has("ready"));
  const [roots, setRoots] = useState([[...(params.has("context") ? [message(5)] : []), ...Array.from({ length: 80 }, (_, i) => message(i + 41))], Array.from({ length: 80 }, (_, i) => message(i + 1, 1))]);
  const [loading, setLoading] = useState(false), [pages, setPages] = useState(0);
  const [location, setLocation] = useState<ChannelReadLocation | null>(null);
  const [tab, setTab] = useState<"chat" | "tasks" | "github" | "wiki">("chat");
  const [draft, setDraft] = useState("");
  const [debug, setDebug] = useState("");
  const shown = channel === 0 && !ready ? [message(41)] : roots[channel];
  const hydrated = channel !== 0 || ready;
  useVisibleChannelRead({ channelId: channels[channel].id, latestRootId: shown.at(-1)?.id ?? null,
    throughSeq: shown.at(-1)?.seq ?? 0, unreadCount: 1, ready: hydrated, active: tab === "chat", location });
  useEffect(() => {
    const timer = setInterval(() => {
      const list = document.querySelector<HTMLDivElement>(".message-list");
      setDebug(JSON.stringify({ channel, ready: hydrated, pages, loading, receipts, location,
        anchor: list && captureMessageAnchor(list), restoring: list?.getAttribute("aria-busy"),
        distance: list ? list.scrollHeight - list.clientHeight - list.scrollTop : null,
        top: list?.scrollTop, rowCount: shown.length,
        saved: JSON.parse(localStorage.getItem(CHANNEL_READING_POSITION_KEY) || "[]") }, null, 2));
    }, 100);
    return () => clearInterval(timer);
  }, [channel, hydrated, shown, location, pages, loading]);
  async function older() {
    const targetChannel = channel;
    setPages(n => n + 1); setLoading(true);
    await new Promise(resolve => setTimeout(resolve, Number(params.get("delay") || 180)));
    setLoading(false);
    if (params.has("fail")) return false;
    if (targetChannel !== 0) return false;
    setRoots(before => before.map((items, c) => c !== 0 ? items : [...Array.from({ length: 40 }, (_, i) => message(i + 1))
      .filter(row => !params.has("deleted") || row.seq !== 5), ...items.filter(row => row.seq > 40)]));
    return true;
  }
  function scrollUp(toTop = false) {
    const list = document.querySelector<HTMLDivElement>(".message-list")!;
    list.dispatchEvent(new WheelEvent("wheel", { deltaY: -500, bubbles: true }));
    list.scrollTop = toTop ? 0 : list.scrollTop - 600;
  }
  function append(c = channel) { setRoots(before => before.map((items, index) => index === c ? [...items, message(items.at(-1)!.seq + 1, c)] : items)); }
  const shared = { channel: channels[channel], channels, agents: [], channelAgents: [], ownerProfile: { display_name: "Owner", avatar: "O", description: "" },
    agentActivities: [], agentRuns: [], agentWorkItems: [], messages: shown, activeRoot: null, taskTitleDrafts: {}, setTaskTitleDraft: noop, saveTaskTitle: noop,
    claimTask: noop, updateTaskStatus: noop, openAgentDetail: noop, openArtifact: noop, onReferenceMessageJump: noop, onReferenceThreadJump: noop,
    shareBaseUrl: null, savedMessageIds: new Set<string>(), focusedMessageId: null, showImageThumbnails: false, onToggleMessageSaved: noop };
  return <div style={{display: "grid", gridTemplateColumns: "minmax(0,1fr) 360px", height: "100vh"}}>
    <Conversation {...shared} activeTab={tab} setActiveTab={setTab} rootMessages={shown} threadReplyCounts={{}} threadUnreadCounts={{}} threadReplySummaries={{}}
      visibleTasks={[]} draft={draft} draftAttachments={[]} setActiveThreadId={noop} openMobileSidebar={noop} canNavigateBack={false} canNavigateForward={false}
      navigateBack={noop} navigateForward={noop} openChannelSettingsModal={noop} deleteChannel={noop} openChannelAgentsModal={noop} taskForMessage={() => null}
      openTask={noop} createGithubReviewTask={async () => {throw Error("unused");}} createGithubIssueTask={async () => {throw Error("unused");}}
      setDraft={setDraft} addDraftAttachments={noop} removeDraftAttachment={noop} sendRootMessage={() => append()} hasMoreRootMessages={channel === 0 && roots[0][0].seq > 1}
      historyBeforeSeq={channel === 0 ? (roots[0].some(row => row.seq === 1) ? 1 : 41) : undefined}
      isLoadingOlderRootMessages={loading} onLoadOlderRootMessages={older} isChannelReady={hydrated} onReadLocation={setLocation} />
    <aside style={{overflow: "auto", padding: 12, fontSize: 12}}>
      <h2>Reading regression controls</h2>
      <button onClick={() => setReady(true)}>Hydrate channel</button>
      <button onClick={() => {setChannel(1); setTab("chat");}}>Switch to other</button>
      <button onClick={() => {setChannel(0); setTab("chat");}}>Switch to channel</button>
      <button onClick={() => {document.querySelector<HTMLDivElement>(".message-list")!.scrollTop = 0; setChannel(1);}}>Queue old scroll and switch</button>
      <button onClick={() => scrollUp()}>Read middle</button>
      <button onClick={() => scrollUp(true)}>Read older history</button>
      <button onClick={() => append()}>Add new root</button>
      <button onClick={() => append(0)}>Add to channel while away</button>
      <button onClick={() => setRoots(before => before.map((items, c) => c === channel ? items.map((row, i) => i === 0 ? {...row, body: row.body + "Extra height above.\n\n".repeat(20)} : row) : items))}>Grow earlier row</button>
      <button onClick={() => {streamBody = "Streaming start"; setRoots(before => before.map((items, c) => c === channel ? [...items,
        {...message(items.at(-1)!.seq + 1, c), body: streamBody, delivery_state: "streaming"}] : items));}}>Start stream</button>
      <button onClick={() => {streamBody += "\n\nStreaming growth.\n\n".repeat(25); streamingMessages.publish(shown.at(-1)!.id,
        {body: streamBody, delivery_state: "streaming"});}}>Grow stream</button>
      <pre aria-label="Reading diagnostics" style={{whiteSpace: "pre-wrap", fontSize: 11}}>{debug}</pre>
    </aside>
  </div>;
}
const fixtureRoot = createRoot(document.getElementById("root")!);
fixtureRoot.render(<Fixture />);
import.meta.hot?.dispose(() => fixtureRoot.unmount());
