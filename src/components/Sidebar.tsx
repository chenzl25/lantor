import {
  ArrowDownAZ,
  ChevronDown,
  Circle,
  Clock3,
  Hash,
  Inbox,
  Bookmark,
  Plus,
  Search,
  Settings,
  UserRound,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import {
  Agent,
  Bootstrap,
  Channel,
} from "../types";
import { APP_DISPLAY_NAME } from "../branding";
import { ownerAsAvatarAgent } from "../ui-utils";
import { AgentAvatar } from "./AgentAvatar";
import { UnreadBadge } from "./UnreadBadge";

const SIDEBAR_CHANNEL_SORT_STORAGE_KEY = "lantor.sidebarChannelSort";
const CHANNEL_NAME_COLLATOR = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

type ChannelSortMode = "name" | "recent";

type SidebarProps = {
  data: Bootstrap;
  channel: Channel | null;
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

function loadSidebarChannelSortMode(): ChannelSortMode {
  return window.localStorage.getItem(SIDEBAR_CHANNEL_SORT_STORAGE_KEY) === "recent" ? "recent" : "name";
}

function timestampValue(value: string | null | undefined) {
  if (!value) return 0;
  const timestamp = new Date(value).getTime();
  return Number.isFinite(timestamp) ? timestamp : 0;
}

function compareChannelsByName(left: Channel, right: Channel) {
  return CHANNEL_NAME_COLLATOR.compare(left.name, right.name)
    || CHANNEL_NAME_COLLATOR.compare(left.id, right.id);
}

export function Sidebar({
  data,
  channel,
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
  const [channelSortMode, setChannelSortMode] = useState<ChannelSortMode>(() => loadSidebarChannelSortMode());
  const dmListRef = useRef<HTMLElement | null>(null);
  const normalChannels = useMemo(() => {
    const latestActivityByChannel = new Map<string, number>();
    for (const message of data.messages) {
      const current = latestActivityByChannel.get(message.channel_id) ?? 0;
      latestActivityByChannel.set(message.channel_id, Math.max(current, timestampValue(message.created_at)));
    }
    for (const activity of data.thread_activities) {
      const current = latestActivityByChannel.get(activity.channel_id) ?? 0;
      latestActivityByChannel.set(activity.channel_id, Math.max(current, timestampValue(activity.latest_activity_at)));
    }

    return data.channels
      .filter((item) => item.kind !== "dm")
      .sort((left, right) => {
        if (channelSortMode === "recent") {
          const latestDelta = (latestActivityByChannel.get(right.id) ?? 0) - (latestActivityByChannel.get(left.id) ?? 0);
          if (latestDelta !== 0) return latestDelta;
        }
        return compareChannelsByName(left, right);
      });
  }, [channelSortMode, data.channels, data.messages, data.thread_activities]);
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
  useEffect(() => {
    window.localStorage.setItem(SIDEBAR_CHANNEL_SORT_STORAGE_KEY, channelSortMode);
  }, [channelSortMode]);

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
        <button type="button" className="channel quick-action-row" onClick={openSearch}>
          <Search size={17} />
          <span className="channel-name">Search</span>
        </button>
        <button
          type="button"
          className={`channel quick-action-row ${activityFeedUnreadCount ? "has-unread" : ""}`}
          onClick={openActivityFeed}
        >
          <Inbox size={17} />
          <span className="channel-name">Activity</span>
          {activityFeedUnreadCount > 0 && <UnreadBadge value={activityFeedUnreadCount} />}
        </button>
        <button
          type="button"
          className={`channel quick-action-row ${savedUnreadCount ? "has-unread" : ""}`}
          onClick={openSaved}
        >
          <Bookmark size={17} />
          <span className="channel-name">Saved</span>
          {savedUnreadCount > 0 && <UnreadBadge value={savedUnreadCount} />}
        </button>
      </section>

      <div className={`sidebar-sections ${collapsedSections.channels ? "channels-collapsed" : ""} ${collapsedSections.dms ? "dms-collapsed" : ""}`}>
        <section className={`channel-block ${collapsedSections.channels ? "collapsed" : ""}`}>
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
            <div className="section-actions channel-section-actions">
              <div className="channel-sort-control" role="group" aria-label="Sort channels">
                <button
                  type="button"
                  className={channelSortMode === "name" ? "active" : ""}
                  title="Sort channels by name"
                  aria-label="Sort channels by name"
                  aria-pressed={channelSortMode === "name"}
                  onClick={() => setChannelSortMode("name")}
                >
                  <ArrowDownAZ size={15} />
                </button>
                <button
                  type="button"
                  className={channelSortMode === "recent" ? "active" : ""}
                  title="Sort channels by recent messages"
                  aria-label="Sort channels by recent messages"
                  aria-pressed={channelSortMode === "recent"}
                  onClick={() => setChannelSortMode("recent")}
                >
                  <Clock3 size={15} />
                </button>
              </div>
              <button onClick={openCreateChannelModal} title="Create channel"><Plus size={18} /></button>
            </div>
          </div>
          <div className="sidebar-section-scroll">
            {!collapsedSections.channels && normalChannels.map((item) => {
              const badge = item.unread_count > 0 ? String(item.unread_count) : "";
              return (
                <button
                  key={item.id}
                  className={`channel ${item.id === channel?.id ? "selected" : ""} ${badge ? "has-unread" : ""}`}
                  onClick={() => selectChannel(item.id)}
                >
                  <Hash size={17} />
                  <span className="channel-name">{item.name}</span>
                  {badge && <UnreadBadge value={badge} />}
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
              const badge = item && item.unread_count > 0 ? String(item.unread_count) : "";
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
                    {badge && <UnreadBadge value={badge} />}
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
