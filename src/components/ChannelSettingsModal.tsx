import { Channel } from "../types";
import { Modal } from "./Modal";

type ChannelSettingsModalProps = {
  open: boolean;
  channel: Channel | null;
  nameDraft: string;
  descriptionDraft: string;
  onNameChange: (value: string) => void;
  onDescriptionChange: (value: string) => void;
  onCancel: () => void;
  onSave: () => void;
};

export function ChannelSettingsModal({
  open,
  channel,
  nameDraft,
  descriptionDraft,
  onNameChange,
  onDescriptionChange,
  onCancel,
  onSave,
}: ChannelSettingsModalProps) {
  return (
    <Modal
      open={open}
      title={channel ? `#${channel.name} Settings` : "Channel Settings"}
      onClose={onCancel}
      width={560}
    >
      {channel && (
        <div className="modal-form">
          <label>
            <span>Channel name</span>
            <input
              value={nameDraft}
              onChange={(event) => onNameChange(event.target.value)}
              placeholder="channel-name"
            />
          </label>
          <label>
            <span>Description</span>
            <textarea
              value={descriptionDraft}
              onChange={(event) => onDescriptionChange(event.target.value)}
              placeholder="Channel description"
            />
          </label>
          <div className="modal-actions">
            <button type="button" className="primary" disabled={!nameDraft.trim()} onClick={onSave}>Save</button>
          </div>
        </div>
      )}
    </Modal>
  );
}
