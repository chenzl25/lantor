import { Image, MessageSquare, Monitor, Moon, Sun, Type } from "lucide-react";
import { setAgentReplyMode, useAgentReplyMode } from "../hooks/useAgentReplyMode";
import { Modal } from "./Modal";

export type ThemePreference = "auto" | "light" | "dark";
export type ChatTextSize = "compact" | "default" | "large" | "xlarge";
export type FontPreset = "space-grotesk" | "system";

type SettingsModalProps = {
  open: boolean;
  themePreference: ThemePreference;
  chatTextSize: ChatTextSize;
  fontPreset: FontPreset;
  showImageThumbnails: boolean;
  onThemePreferenceChange: (value: ThemePreference) => void;
  onChatTextSizeChange: (value: ChatTextSize) => void;
  onFontPresetChange: (value: FontPreset) => void;
  onShowImageThumbnailsChange: (value: boolean) => void;
  onClose: () => void;
};

const THEME_OPTIONS: Array<{
  value: ThemePreference;
  label: string;
  icon: typeof Monitor;
}> = [
  { value: "auto", label: "Auto", icon: Monitor },
  { value: "light", label: "Light", icon: Sun },
  { value: "dark", label: "Dark", icon: Moon },
];

const CHAT_TEXT_SIZE_OPTIONS: Array<{
  value: ChatTextSize;
  label: string;
}> = [
  { value: "compact", label: "Small" },
  { value: "default", label: "Default" },
  { value: "large", label: "Large" },
  { value: "xlarge", label: "Extra" },
];

const FONT_PRESET_OPTIONS: Array<{
  value: FontPreset;
  label: string;
}> = [
  { value: "system", label: "System" },
  { value: "space-grotesk", label: "Space Grotesk" },
];

export function SettingsModal({
  open,
  themePreference,
  chatTextSize,
  fontPreset,
  showImageThumbnails,
  onThemePreferenceChange,
  onChatTextSizeChange,
  onFontPresetChange,
  onShowImageThumbnailsChange,
  onClose,
}: SettingsModalProps) {
  const agentReplyMode = useAgentReplyMode();
  return (
    <Modal open={open} title="Settings" onClose={onClose} width={560}>
      <section className="settings-panel">
        <div className="settings-section-head">
          <h4>Appearance</h4>
        </div>
        <fieldset className="settings-fieldset">
          <legend>Theme</legend>
          <div className="theme-choice-grid">
            {THEME_OPTIONS.map((option) => {
              const Icon = option.icon;
              return (
                <button
                  type="button"
                  key={option.value}
                  className={themePreference === option.value ? "selected" : ""}
                  aria-pressed={themePreference === option.value}
                  onClick={() => onThemePreferenceChange(option.value)}
                >
                  <Icon size={18} />
                  <strong>{option.label}</strong>
                </button>
              );
            })}
          </div>
        </fieldset>
        <fieldset className="settings-fieldset">
          <legend>Text size</legend>
          <div className="chat-text-size-grid">
            {CHAT_TEXT_SIZE_OPTIONS.map((option) => (
              <button
                type="button"
                key={option.value}
                className={chatTextSize === option.value ? "selected" : ""}
                aria-pressed={chatTextSize === option.value}
                onClick={() => onChatTextSizeChange(option.value)}
              >
                <Type size={17} />
                <strong>{option.label}</strong>
              </button>
            ))}
          </div>
        </fieldset>
        <fieldset className="settings-fieldset">
          <legend>Font</legend>
          <div className="theme-choice-grid font-preset-grid">
            {FONT_PRESET_OPTIONS.map((option) => (
              <button
                type="button"
                key={option.value}
                className={fontPreset === option.value ? "selected" : ""}
                aria-pressed={fontPreset === option.value}
                onClick={() => onFontPresetChange(option.value)}
              >
                <Type size={17} />
                <strong>{option.label}</strong>
              </button>
            ))}
          </div>
        </fieldset>
        <fieldset className="settings-fieldset">
          <legend>Agent replies</legend>
          <div className="theme-choice-grid font-preset-grid">
            {([ ["streaming", "Live streaming"], ["final", "Final result only"] ] as const).map(([value, label]) => (
              <button type="button" key={value} className={agentReplyMode === value ? "selected" : ""}
                aria-pressed={agentReplyMode === value} onClick={() => setAgentReplyMode(value)}>
                <MessageSquare size={17} />
                <strong>{label}</strong>
              </button>
            ))}
          </div>
        </fieldset>
        <fieldset className="settings-fieldset settings-attachments-fieldset">
          <legend>Attachments</legend>
          <label className="settings-toggle-row">
            <span className="settings-toggle-copy">
              <Image size={17} />
              <strong>Show image thumbnails</strong>
            </span>
            <input
              type="checkbox"
              checked={showImageThumbnails}
              onChange={(event) => onShowImageThumbnailsChange(event.currentTarget.checked)}
            />
          </label>
        </fieldset>
      </section>
    </Modal>
  );
}
