import { Monitor, Moon, Sun, Type } from "lucide-react";
import { Modal } from "./Modal";

export type ThemePreference = "auto" | "light" | "dark";
export type ChatTextSize = "compact" | "default" | "large" | "xlarge";

type SettingsModalProps = {
  open: boolean;
  themePreference: ThemePreference;
  chatTextSize: ChatTextSize;
  onThemePreferenceChange: (value: ThemePreference) => void;
  onChatTextSizeChange: (value: ChatTextSize) => void;
  onClose: () => void;
};

const THEME_OPTIONS: Array<{
  value: ThemePreference;
  label: string;
  detail: string;
  icon: typeof Monitor;
}> = [
  { value: "auto", label: "Auto", detail: "Follow system", icon: Monitor },
  { value: "light", label: "Light", detail: "Bright surfaces", icon: Sun },
  { value: "dark", label: "Dark", detail: "Dim surfaces", icon: Moon },
];

const CHAT_TEXT_SIZE_OPTIONS: Array<{
  value: ChatTextSize;
  label: string;
  detail: string;
}> = [
  { value: "compact", label: "Small", detail: "Compact UI" },
  { value: "default", label: "Default", detail: "Current scale" },
  { value: "large", label: "Large", detail: "More readable" },
  { value: "xlarge", label: "Extra", detail: "Largest" },
];

export function SettingsModal({
  open,
  themePreference,
  chatTextSize,
  onThemePreferenceChange,
  onChatTextSizeChange,
  onClose,
}: SettingsModalProps) {
  return (
    <Modal open={open} title="Settings" onClose={onClose} width={560}>
      <section className="settings-panel">
        <div className="settings-section-head">
          <h4>Appearance</h4>
          <p>Device-local preferences for this Lantor app.</p>
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
                  <span>
                    <strong>{option.label}</strong>
                    <small>{option.detail}</small>
                  </span>
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
                <span>
                  <strong>{option.label}</strong>
                  <small>{option.detail}</small>
                </span>
              </button>
            ))}
          </div>
          <p className="settings-hint">Applies across messages, inputs, panels, and modals. Use Command +/- or Ctrl +/- to adjust without opening Settings. Command/Ctrl+0 resets.</p>
        </fieldset>
      </section>
    </Modal>
  );
}
