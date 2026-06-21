import { ChevronDown, Shuffle } from "lucide-react";
import { useEffect, useRef } from "react";
import { Modal } from "./Modal";
import {
  AgentForm,
  CLAUDE_REASONING_EFFORTS,
  CODEX_REASONING_EFFORTS,
  CODEX_SERVICE_TIERS,
  RuntimeCheck,
  modelLabel,
  modelOptionsForRuntime,
} from "../types";
import { randomDylanAvatarSpec } from "../avatar-utils";
import { APP_DISPLAY_NAME } from "../branding";
import { AgentAvatar } from "./AgentAvatar";

type AgentFormModalProps = {
  open: boolean;
  title: string;
  form: AgentForm;
  runtimeChecks: Record<string, RuntimeCheck>;
  submitLabel: string;
  createMode?: boolean;
  showNotes?: boolean;
  onChange: (form: AgentForm) => void;
  onRuntimeChange: (runtime: string) => void;
  onCancel: () => void;
  onSubmit: () => void;
};

function RuntimePreflight({ check }: { check: RuntimeCheck | undefined }) {
  if (!check) {
    return <div className="runtime-preflight pending">Checking local CLI...</div>;
  }
  return (
    <div className={`runtime-preflight ${check.available ? "ok" : "missing"}`}>
      <strong>{check.available ? "Runtime ready" : "Runtime unavailable"}</strong>
      <span>{check.command || check.runtime}: {check.detail}</span>
    </div>
  );
}

function seedForAgentForm(form: AgentForm) {
  const seed = [form.handle, form.displayName]
    .map((value) => value.trim())
    .filter(Boolean)
    .join(":");
  return seed || "agent";
}

function shouldAutoFocusTextInput() {
  return window.matchMedia("(hover: hover) and (pointer: fine)").matches;
}

export function AgentFormModal({
  open,
  title,
  form,
  runtimeChecks,
  submitLabel,
  createMode = false,
  showNotes = false,
  onChange,
  onRuntimeChange,
  onCancel,
  onSubmit,
}: AgentFormModalProps) {
  const nameInputRef = useRef<HTMLInputElement | null>(null);
  const isCodex = form.runtime === "codex";
  const isClaude = form.runtime === "claude";
  const previewHandle = (form.handle || form.displayName || "agent").trim().replace(/^@/, "") || "agent";
  const previewName = form.displayName.trim() || form.handle.trim().replace(/^@/, "") || "New agent";
  const previewAgent = {
    id: "agent-form-preview",
    handle: previewHandle,
    display_name: previewName,
    status: "idle",
    runtime: form.runtime,
    model: form.model,
    role: form.role,
    avatar: form.avatar,
  };
  const canSubmit = createMode
    ? Boolean(form.displayName.trim() || form.handle.trim())
    : Boolean(form.handle.trim() || form.displayName.trim());
  const runtimeSelect = (
    <label className="agent-select-field">
      <span>Runtime</span>
      <div className="agent-select-control">
        <select value={form.runtime} onChange={(event) => onRuntimeChange(event.target.value)}>
          <option value="codex">Codex</option>
          <option value="claude">Claude</option>
        </select>
        <ChevronDown size={16} aria-hidden="true" />
      </div>
    </label>
  );
  const modelSelect = (
    <label className="agent-select-field">
      <span>Model</span>
      <div className="agent-select-control">
        <select
          value={form.model}
          onChange={(event) => onChange({ ...form, model: event.target.value })}
        >
          {modelOptionsForRuntime(form.runtime, form.model).map((model) => (
            <option key={model} value={model}>{modelLabel(model)}</option>
          ))}
        </select>
        <ChevronDown size={16} aria-hidden="true" />
      </div>
    </label>
  );
  const codexControls = isCodex && (
    <>
      <label className="agent-select-field">
        <span>Intelligence</span>
        <div className="agent-select-control">
          <select
            value={form.reasoningEffort}
            onChange={(event) => onChange({ ...form, reasoningEffort: event.target.value })}
          >
            {CODEX_REASONING_EFFORTS.map((effort) => (
              <option key={effort.value} value={effort.value}>{effort.label}</option>
            ))}
          </select>
          <ChevronDown size={16} aria-hidden="true" />
        </div>
      </label>
      <label className="agent-select-field">
        <span>Speed</span>
        <div className="agent-select-control">
          <select
            value={form.serviceTier}
            onChange={(event) => onChange({ ...form, serviceTier: event.target.value })}
          >
            {CODEX_SERVICE_TIERS.map((tier) => (
              <option key={tier.value || "default"} value={tier.value}>{tier.label}</option>
            ))}
          </select>
          <ChevronDown size={16} aria-hidden="true" />
        </div>
      </label>
    </>
  );
  const claudeControls = isClaude && (
    <label className="agent-select-field">
      <span>Effort</span>
      <div className="agent-select-control">
        <select
          value={form.reasoningEffort}
          onChange={(event) => onChange({ ...form, reasoningEffort: event.target.value })}
        >
          {CLAUDE_REASONING_EFFORTS.map((effort) => (
            <option key={effort.value || "default"} value={effort.value}>{effort.label}</option>
          ))}
        </select>
        <ChevronDown size={16} aria-hidden="true" />
      </div>
    </label>
  );
  const agentPreview = (
    <div className="agent-form-preview">
      <AgentAvatar agent={previewAgent} size="lg" showStatus={false} />
      <div>
        <strong>{previewName}</strong>
        <span>{modelLabel(form.model)}</span>
      </div>
      <button
        type="button"
        className="agent-form-preview-avatar-action"
        title="Random avatar"
        aria-label="Random avatar"
        onClick={() => onChange({ ...form, avatar: randomDylanAvatarSpec(seedForAgentForm(form)) })}
      >
        <Shuffle size={16} />
      </button>
    </div>
  );
  const environmentField = (
    <label>
      <span>Environment variables</span>
      <textarea
        className="agent-env-textarea"
        value={form.environmentVariables}
        onChange={(event) => onChange({ ...form, environmentVariables: event.target.value })}
        placeholder={"HTTP_PROXY=http://127.0.0.1:7897\nNO_PROXY=localhost,127.0.0.1"}
        spellCheck={false}
      />
    </label>
  );

  useEffect(() => {
    if (!open || !shouldAutoFocusTextInput()) return;
    nameInputRef.current?.focus();
  }, [open]);

  return (
    <Modal
      open={open}
      title={title}
      onClose={onCancel}
      width={700}
    >
      <div className="modal-form agent-modal-form">
        {createMode ? (
          <>
            {agentPreview}
            <div className="two-col">
              <label>
                <span>Name</span>
                <input
                  ref={nameInputRef}
                  value={form.displayName}
                  onChange={(event) => onChange({ ...form, displayName: event.target.value })}
                  placeholder="Agent name"
                />
              </label>
              {runtimeSelect}
            </div>
            <div className={isCodex ? "three-col" : "two-col"}>
              {modelSelect}
              {codexControls}
              {claudeControls}
            </div>
            <RuntimePreflight check={runtimeChecks[form.runtime]} />
            <details className="agent-advanced-settings">
              <summary>
                <span>Advanced</span>
                <ChevronDown size={16} aria-hidden="true" />
              </summary>
              <label>
                <span>Workspace directory</span>
                <input
                  value={form.workingDirectory}
                  onChange={(event) => onChange({ ...form, workingDirectory: event.target.value })}
                  placeholder="~/Library/Application Support/Lantor/agents/<handle>"
                />
              </label>
              {environmentField}
            </details>
          </>
        ) : (
          <>
            {agentPreview}
            <label>
              <span>Name</span>
              <input
                ref={nameInputRef}
                value={form.displayName}
                onChange={(event) => onChange({ ...form, displayName: event.target.value })}
                placeholder="Agent name"
              />
            </label>
            <div className="two-col">
              {runtimeSelect}
              {modelSelect}
            </div>
            {isCodex && <div className="two-col">{codexControls}</div>}
            {isClaude && <div className="two-col">{claudeControls}</div>}
            <RuntimePreflight check={runtimeChecks[form.runtime]} />
            {showNotes && (
              <label>
                <span>Notes</span>
                <textarea
                  value={form.description}
                  onChange={(event) => onChange({ ...form, description: event.target.value })}
                  placeholder="Agent notes"
                />
              </label>
            )}
            <label>
              <span>Workspace directory</span>
              <input
                value={form.workingDirectory}
                onChange={(event) => onChange({ ...form, workingDirectory: event.target.value })}
                placeholder="~/Library/Application Support/Lantor/agents/<handle>"
              />
              <small>{APP_DISPLAY_NAME} loads MEMORY.md from this directory as persistent context when the agent runs.</small>
            </label>
            {environmentField}
          </>
        )}
        <div className="modal-actions">
          <button onClick={onCancel}>Cancel</button>
          <button className="primary" disabled={!canSubmit} onClick={onSubmit}>{submitLabel}</button>
        </div>
      </div>
    </Modal>
  );
}
