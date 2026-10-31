import { Check, UserPlus } from "lucide-react";
import { Agent, Channel } from "../types";
import { AgentAvatar } from "./AgentAvatar";
import { Modal } from "./Modal";

type ChannelAgentsModalProps = {
  open: boolean;
  channel: Channel | null;
  agents: Agent[];
  channelMemberIds: Set<string>;
  onSetMember: (agentId: string, member: boolean) => void;
  onCreateAgent: () => void;
  onClose: () => void;
};

export function ChannelAgentsModal({
  open,
  channel,
  agents,
  channelMemberIds,
  onSetMember,
  onCreateAgent,
  onClose,
}: ChannelAgentsModalProps) {
  const selectedCount = agents.filter((agent) => channelMemberIds.has(agent.id)).length;

  return (
    <Modal
      open={open}
      title={channel ? `Add agents to #${channel.name}` : "Add agents"}
      onClose={onClose}
      width={560}
    >
      <div className="modal-form">
        <div className="channel-agent-modal-intro">
          <span className="channel-agent-modal-icon" aria-hidden="true">
            <UserPlus size={18} />
          </span>
          <div>
            <strong>{selectedCount > 0 ? `${selectedCount} ${selectedCount === 1 ? "agent" : "agents"} added` : "Agents in this channel"}</strong>
            <p>{agents.length > 0 ? "Pick who works here." : "Create an agent first."}</p>
          </div>
        </div>
        <div className="member-editor modal-member-editor channel-agent-picker">
          {agents.length === 0 && <span>No agents yet.</span>}
          {agents.map((agent) => {
            const isMember = channelMemberIds.has(agent.id);
            return (
              <label key={agent.id} className={`channel-agent-option ${isMember ? "selected" : ""}`}>
                <input
                  className="channel-agent-checkbox"
                  type="checkbox"
                  checked={isMember}
                  onChange={(event) => onSetMember(agent.id, event.target.checked)}
                  aria-label={`${isMember ? "Remove" : "Add"} @${agent.handle}`}
                />
                <div className="channel-agent-profile">
                  <AgentAvatar agent={agent} size="sm" title={`@${agent.handle}`} />
                  <div className="agent-pick-row">
                    <strong>{agent.display_name}</strong>
                    <small>@{agent.handle}</small>
                  </div>
                </div>
                <span className={`channel-agent-option-state ${isMember ? "selected" : ""}`}>
                  {isMember ? <Check size={14} /> : <UserPlus size={14} />}
                  {isMember ? "Added" : "Add"}
                </span>
              </label>
            );
          })}
        </div>
        <div className="modal-actions split">
          <button onClick={onCreateAgent}>New agent</button>
          <button className="primary" onClick={onClose}>Done</button>
        </div>
      </div>
    </Modal>
  );
}
