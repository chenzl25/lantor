import {
  ChevronDown,
  Circle,
  Hash,
  Inbox,
  Bookmark,
  Plus,
  Search,
  Settings,
  UserRound,
} from "lucide-react";
import { useEffect, useRef, useState, type CSSProperties, type PointerEvent as ReactPointerEvent } from "react";
import {
  Agent,
  Bootstrap,
  Channel,
} from "../types";
import { APP_DISPLAY_NAME } from "../branding";
import { ownerAsAvatarAgent } from "../ui-utils";
import { AgentAvatar } from "./AgentAvatar";

const SIDEBAR_CHANNELS_HEIGHT_STORAGE_KEY = "lantor.sidebarChannelsHeight";
const DEFAULT_SIDEBAR_CHANNELS_HEIGHT = 260;
const MIN_SIDEBAR_SECTION_HEIGHT = 104;
const SIDEBAR_SECTION_HANDLE_SPACE = 14;

type SidebarProps = {
  data: Bootstrap;
  channel: Channel | null;
  channelAlertIds: Set<string>;
  activityFeedUnreadCount: number;
  savedUnreadCount: number;
  openSearch: () => void;
  openActivityFeed: () => void;
  openSaved: () => void;
  mobileFocus: "home" | "dms";
  openCreateChannelModal: () => void;
  selectChannel: (channelId: string) => void;
  openCreateAgentModal: () => void;
  openDmWithAgent: (agent: Agent) => void;
  openAgentDetail: (agent: Agent) => void;
  openOwnerProfileModal: () => void;
  openSettingsModal: () => void;
  onResizeStart: (event: ReactPointerEvent<HTMLButtonElement>) => void;
};

function loadSidebarChannelsHeight() {
  const stored = window.localStorage.getItem(SIDEBAR_CHANNELS_HEIGHT_STORAGE_KEY);
  const parsed = stored ? Number(stored) : Number.NaN;
  return Number.isFinite(parsed)
    ? Math.max(MIN_SIDEBAR_SECTION_HEIGHT, parsed)
    : DEFAULT_SIDEBAR_CHANNELS_HEIGHT;
}

function clampSidebarChannelsHeight(height: number, containerHeight: number) {
  const maxHeight = Math.max(
    MIN_SIDEBAR_SECTION_HEIGHT,
    containerHeight - MIN_SIDEBAR_SECTION_HEIGHT - SIDEBAR_SECTION_HANDLE_SPACE,
  );
  return Math.min(maxHeight, Math.max(MIN_SIDEBAR_SECTION_HEIGHT, height));
}

export function Sidebar({
  data,
  channel,
  channelAlertIds,
  activityFeedUnreadCount,
  savedUnreadCount,
  openSearch,
  openActivityFeed,
  openSaved,
  mobileFocus,
  openCreateChannelModal,
  selectChannel,
  openCreateAgentModal,
  openDmWithAgent,
  openAgentDetail,
  openOwnerProfileModal,
  openSettingsModal,
  onResizeStart,
}: SidebarProps) {
  const [collapsedSections, setCollapsedSections] = useState({ channels: false, dms: false });
  const [channelSectionHeight, setChannelSectionHeight] = useState(() => loadSidebarChannelsHeight());
  const sidebarSectionsRef = useRef<HTMLDivElement | null>(null);
  const channelBlockRef = useRef<HTMLElement | null>(null);
  const dmListRef = useRef<HTMLElement | null>(null);
  const normalChannels = data.channels.filter((item) => item.kind !== "dm");
  const dmChannels = data.channels.filter((item) => item.kind === "dm");
  const showDmConversations = mobileFocus === "dms" && window.matchMedia("(max-width: 760px)").matches;
  const dmRows = showDmConversations
    ? dmChannels
      .map((item) => {
        const agent = item.dm_agent_id ? data.agents.find((candidate) => candidate.id === item.dm_agent_id) ?? null : null;
        return agent ? { agent, item } : null;
      })
      .filter((row): row is { agent: Agent; item: Channel } => Boolean(row))
    : data.agents.map((agent) => ({
      agent,
      item: dmChannels.find((candidate) => candidate.dm_agent_id === agent.id) ?? null,
    }));
  const toggleSection = (section: "channels" | "dms") => {
    setCollapsedSections((current) => ({ ...current, [section]: !current[section] }));
  };
  const openAgentDmTarget = (agent: Agent, dmChannel: Channel | null) => {
    if (dmChannel) selectChannel(dmChannel.id);
    else openDmWithAgent(agent);
  };
  const showSectionResizeHandle = !collapsedSections.channels && !collapsedSections.dms;

  const setClampedChannelSectionHeight = (nextHeight: number) => {
    const containerHeight = sidebarSectionsRef.current?.clientHeight ?? 0;
    setChannelSectionHeight(containerHeight > 0
      ? clampSidebarChannelsHeight(nextHeight, containerHeight)
      : Math.max(MIN_SIDEBAR_SECTION_HEIGHT, nextHeight));
  };

  const nudgeChannelSectionHeight = (delta: number) => {
    setClampedChannelSectionHeight(channelSectionHeight + delta);
  };

  function startSectionResize(event: ReactPointerEvent<HTMLButtonElement>) {
    event.preventDefault();
    const containerHeight = sidebarSectionsRef.current?.clientHeight ?? 0;
    if (containerHeight <= 0) return;
    const startY = event.clientY;
    const startHeight = channelBlockRef.current?.getBoundingClientRect().height ?? channelSectionHeight;

    function onPointerMove(moveEvent: PointerEvent) {
      const delta = moveEvent.clientY - startY;
      setChannelSectionHeight(clampSidebarChannelsHeight(startHeight + delta, containerHeight));
    }

    function stopResize() {
      window.removeEventListener("pointermove", onPointerMove);
      window.removeEventListener("pointerup", stopResize);
      window.removeEventListener("pointercancel", stopResize);
      document.body.classList.remove("resizing-row");
    }

    document.body.classList.add("resizing-row");
    window.addEventListener("pointermove", onPointerMove);
    window.addEventListener("pointerup", stopResize);
    window.addEventListener("pointercancel", stopResize);
  }

  useEffect(() => {
    window.localStorage.setItem(SIDEBAR_CHANNELS_HEIGHT_STORAGE_KEY, String(Math.round(channelSectionHeight)));
  }, [channelSectionHeight]);

  useEffect(() => {
    if (mobileFocus !== "dms") return;
    setCollapsedSections((current) => current.dms ? { ...current, dms: false } : current);
    window.requestAnimationFrame(() => {
      dmListRef.current?.scrollIntoView({ block: "start", behavior: "smooth" });
    });
  }, [mobileFocus]);

  return (
    <aside className="sidebar">
      <button
        className="sidebar-resize-handle"
        aria-label="Resize sidebar"
        onPointerDown={onResizeStart}
      />
      <section className="workspace">
        <div className="workspace-switch" aria-label={APP_DISPLAY_NAME}>
          <img className="workspace-switch-logo" src="/lantor-icon.png" alt="" aria-hidden="true" />
          <strong>{APP_DISPLAY_NAME}</strong>
        </div>
      </section>

      <section className="quick-actions">
        <button className="search-trigger" onClick={openSearch}>
          <Search size={18} />
          <span>Search</span>
          <kbd>⌘K</kbd>
        </button>
        <button
          className={`sidebar-nav-trigger ${activityFeedUnreadCount ? "has-unread" : ""}`}
          onClick={openActivityFeed}
        >
          <Inbox size={18} />
          <span>Activity</span>
          {activityFeedUnreadCount > 0 && <strong>{activityFeedUnreadCount}</strong>}
        </button>
        <button
          className={`sidebar-nav-trigger ${savedUnreadCount ? "has-unread" : ""}`}
          onClick={openSaved}
        >
          <Bookmark size={18} />
          <span>Saved</span>
          {savedUnreadCount > 0 && <strong>{savedUnreadCount}</strong>}
        </button>
      </section>

      <div
        ref={sidebarSectionsRef}
        className={`sidebar-sections ${collapsedSections.channels ? "channels-collapsed" : ""} ${collapsedSections.dms ? "dms-collapsed" : ""}`}
        style={{ "--sidebar-channels-height": `${channelSectionHeight}px` } as CSSProperties}
      >
        <section ref={channelBlockRef} className={`channel-block ${collapsedSections.channels ? "collapsed" : ""}`}>
          <div className="section-title">
            <div className="section-label">
              <button
                className={`section-collapse ${collapsedSections.channels ? "collapsed" : ""}`}
                onClick={() => toggleSection("channels")}
                aria-expanded={!collapsedSections.channels}
                title={collapsedSections.channels ? "Show channels" : "Hide channels"}
              >
                <ChevronDown size={14} />
              </button>
              <span>Channels</span>
            </div>
            <div className="section-actions">
              <button onClick={openCreateChannelModal} title="Create channel"><Plus size={18} /></button>
            </div>
          </div>
          <div className="sidebar-section-scroll">
            {!collapsedSections.channels && normalChannels.map((item) => {
              const badge = item.unread_count > 0 ? String(item.unread_count) : channelAlertIds.has(item.id) ? "1" : "";
              return (
                <button
                  key={item.id}
                  className={`channel ${item.id === channel?.id ? "selected" : ""} ${badge ? "has-unread" : ""}`}
                  onClick={() => selectChannel(item.id)}
                >
                  <Hash size={17} />
                  <span className="channel-name">{item.name}</span>
                  {badge && <strong>{badge}</strong>}
                </button>
              );
            })}
            {!collapsedSections.channels && normalChannels.length === 0 && (
              <div className="sidebar-empty-cta">
                <p>No channels yet.</p>
                <button type="button" onClick={openCreateChannelModal}>
                  <Plus size={16} />
                  <span>Create first channel</span>
                </button>
              </div>
            )}
          </div>
        </section>

        {showSectionResizeHandle && (
          <button
            type="button"
            className="sidebar-section-resize-handle"
            aria-label="Resize Channels and Agents"
            aria-orientation="horizontal"
            title="Resize Channels and Agents"
            onPointerDown={startSectionResize}
            onKeyDown={(event) => {
              if (event.key === "ArrowUp") {
                event.preventDefault();
                nudgeChannelSectionHeight(-24);
              } else if (event.key === "ArrowDown") {
                event.preventDefault();
                nudgeChannelSectionHeight(24);
              }
            }}
          />
        )}

        <section ref={dmListRef} className={`dm-list ${collapsedSections.dms ? "collapsed" : ""}`}>
          <div className="section-title">
            <div className="section-label">
              <button
                className={`section-collapse ${collapsedSections.dms ? "collapsed" : ""}`}
                onClick={() => toggleSection("dms")}
                aria-expanded={!collapsedSections.dms}
                title={collapsedSections.dms ? (showDmConversations ? "Show DMs" : "Show agents") : (showDmConversations ? "Hide DMs" : "Hide agents")}
              >
                <ChevronDown size={14} />
              </button>
              <span>{showDmConversations ? "DMs" : "Agents"}</span>
            </div>
            {!showDmConversations && <button onClick={openCreateAgentModal} title="Add agent"><Plus size={18} /></button>}
          </div>
          <div className="sidebar-section-scroll">
            {!collapsedSections.dms && dmRows.map(({ agent, item }) => {
              const badge = item
                ? item.unread_count > 0
                  ? String(item.unread_count)
                  : channelAlertIds.has(item.id)
                    ? "1"
                    : ""
                : "";
              return (
                <div
                  key={agent.id}
                  className={`dm-row ${item?.id === channel?.id ? "selected" : ""} ${badge ? "has-unread" : ""}`}
                >
                  <button
                    type="button"
                    className="dm-avatar-shell"
                    aria-label={`Open DM with @${agent.handle}`}
                    onClick={() => openAgentDmTarget(agent, item)}
                  >
                    <AgentAvatar agent={agent} size="sm" />
                  </button>
                  <button
                    type="button"
                    className="dm"
                    onClick={() => openAgentDmTarget(agent, item)}
                  >
                    <div>
                      <strong>{agent.display_name}</strong>
                      <span>@{agent.handle}</span>
                    </div>
                    {badge && <strong className="dm-badge">{badge}</strong>}
                  </button>
                  <div className="dm-row-actions">
                    <Circle className={`dot ${agent.status}`} size={10} />
                    <button
                      type="button"
                      className="dm-detail-trigger"
                      title={`View @${agent.handle} details`}
                      aria-label={`View @${agent.handle} details`}
                      onClick={() => openAgentDetail(agent)}
                    >
                      <UserRound size={15} />
                    </button>
                  </div>
                </div>
              );
            })}
            {!collapsedSections.dms && dmRows.length === 0 && (
              showDmConversations ? (
                <div className="empty-mini">No direct messages yet.</div>
              ) : (
                <div className="sidebar-empty-cta">
                  <p>No agents yet.</p>
                  <button type="button" onClick={openCreateAgentModal}>
                    <UserRound size={16} />
                    <span>Create first agent</span>
                  </button>
                </div>
              )
            )}
          </div>
        </section>
      </div>

      <div className="profile">
        <button type="button" className="profile-main" onClick={openOwnerProfileModal}>
          <AgentAvatar agent={ownerAsAvatarAgent(data.owner_profile)} size="md" showStatus={false} />
          <div>
            <strong>{data.owner_profile.display_name}</strong>
            <span>{data.owner_profile.description || "local owner"}</span>
          </div>
        </button>
        <button
          type="button"
          className="profile-settings"
          aria-label="Open settings"
          title="Settings"
          onClick={openSettingsModal}
        >
          <Settings size={17} />
        </button>
      </div>
    </aside>
  );
}
