import { Check, ChevronDown } from "lucide-react";
import type { PointerEvent as ReactPointerEvent } from "react";
import { useEffect, useRef, useState } from "react";
import { Agent } from "../types";
import { AgentAvatar } from "./AgentAvatar";

type TaskAssigneePickerProps = {
  agents: Agent[];
  assignee: Agent | null;
  allowUnassigned?: boolean;
  ariaLabel?: string;
  disabled?: boolean;
  done?: boolean;
  onChange: (agentId: string) => void;
  taskNumber?: number;
  compact?: boolean;
};

export function TaskAssigneePicker({
  agents,
  assignee,
  allowUnassigned = true,
  ariaLabel,
  disabled = false,
  done = false,
  onChange,
  taskNumber,
  compact = false,
}: TaskAssigneePickerProps) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!open) return;
    function handlePointerDown(event: PointerEvent) {
      if (!rootRef.current?.contains(event.target as Node)) setOpen(false);
    }
    function handleKeyDown(event: KeyboardEvent) {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      setOpen(false);
    }
    document.addEventListener("pointerdown", handlePointerDown);
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("pointerdown", handlePointerDown);
      document.removeEventListener("keydown", handleKeyDown);
    };
  }, [open]);

  function choose(agentId: string) {
    setOpen(false);
    if (disabled) return;
    if ((assignee?.id ?? "") === agentId) return;
    onChange(agentId);
  }

  const assigneeLabel = assignee?.display_name ?? "Unassigned";
  const triggerDetail = done
    ? "Done"
    : assignee
      ? `@${assignee.handle}`
      : "No agent";
  const triggerAriaLabel = ariaLabel
    ?? (taskNumber === undefined ? "Assign agent" : `Assign task #${taskNumber}`);
  const optionsAriaLabel = taskNumber === undefined || ariaLabel
    ? `${triggerAriaLabel} options`
    : `Agent options for task #${taskNumber}`;
  const stopPickerPointer = (event: ReactPointerEvent) => {
    event.stopPropagation();
  };

  return (
    <div className={`task-assignee-picker ${compact ? "compact" : ""}`} ref={rootRef} onPointerDown={stopPickerPointer}>
      <button
        type="button"
        className="task-assignee-trigger"
        aria-label={triggerAriaLabel}
        aria-expanded={open}
        disabled={disabled}
        title={compact ? `${assigneeLabel}${assignee ? ` (@${assignee.handle})` : ""}` : undefined}
        onClick={() => setOpen((current) => !current)}
      >
        {assignee ? (
          <AgentAvatar agent={assignee} size="sm" />
        ) : (
          <span className="task-unassigned-avatar" aria-hidden="true" />
        )}
        <span className="task-assignee-copy">
          <strong>{assigneeLabel}</strong>
          <span>{triggerDetail}</span>
        </span>
        <ChevronDown size={15} className="task-assignee-chevron" aria-hidden="true" />
      </button>
      {open && !disabled && (
        <div className="task-assignee-menu" role="listbox" aria-label={optionsAriaLabel}>
          {allowUnassigned && (
            <button
              type="button"
              className="task-assignee-option"
              role="option"
              aria-selected={!assignee}
              onClick={() => choose("")}
            >
              <span className="task-unassigned-avatar" aria-hidden="true" />
              <span className="task-assignee-option-copy">
                <strong>Unassigned</strong>
                <span>No agent</span>
              </span>
              <span className="task-assignee-check" aria-hidden="true">
                {!assignee && <Check size={14} />}
              </span>
            </button>
          )}
          {agents.map((agent) => (
            <button
              type="button"
              className="task-assignee-option"
              role="option"
              aria-selected={assignee?.id === agent.id}
              key={agent.id}
              onClick={() => choose(agent.id)}
            >
              <AgentAvatar agent={agent} size="sm" />
              <span className="task-assignee-option-copy">
                <strong>{agent.display_name}</strong>
                <span>
                  @{agent.handle}
                  <b aria-hidden="true">·</b>
                  {agent.model || agent.runtime}
                </span>
              </span>
              <span className="task-assignee-check" aria-hidden="true">
                {assignee?.id === agent.id && <Check size={14} />}
              </span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
