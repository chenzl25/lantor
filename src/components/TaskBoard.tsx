import { ChevronDown, LayoutList, MessageSquare } from "lucide-react";
import { useMemo, useRef, useState, type ReactNode } from "react";
import { useAutoGrowTextarea } from "../hooks/useAutoGrowTextarea";
import { isImeComposing } from "../input-utils";
import { TASK_STATUSES, type Agent, type Task } from "../types";
import { formatTime } from "../ui-utils";
import { TaskAssigneePicker } from "./TaskAssigneePicker";

type Props = {
  tasks: Task[];
  agents: Agent[];
  assigneeOptions: Agent[];
  titleDrafts: Record<string, string>;
  onTitleChange: (task: Task, value: string) => void;
  onTitleSave: (task: Task) => void;
  onAssign: (task: Task, agentId: string) => void;
  onStatusChange: (task: Task, status: string) => void;
  onOpen: (task: Task) => void;
};

const statusLabel = (status: string) => ({ todo: "To do", in_progress: "In progress", in_review: "In review", done: "Done" })[status] ?? status.replace(/_/g, " ");
type Filter = "all" | "active" | "review" | "unassigned";

function TaskTitle({ task, value, onChange, onSave }: {
  task: Task; value: string; onChange: Props["onTitleChange"]; onSave: Props["onTitleSave"];
}) {
  const ref = useRef<HTMLTextAreaElement>(null);
  useAutoGrowTextarea(ref, value);
  return <textarea ref={ref} rows={1} className="task-row-title" aria-label={`Task #${task.number} title`}
    value={value} onChange={(event) => onChange(task, event.target.value.replace(/\n/g, " "))}
    onBlur={() => onSave(task)} onKeyDown={(event) => {
      if (event.key === "Enter" && !isImeComposing(event)) {
        event.preventDefault();
        event.currentTarget.blur();
      }
    }} />;
}

function TaskGroup({ status, count, children }: { status: string; count: number; children: ReactNode }) {
  const [expanded, setExpanded] = useState(status !== "done");
  return <section className="task-queue-section" aria-label={`${statusLabel(status)} tasks`}>
    <button type="button" className="task-group-toggle" aria-expanded={expanded} onClick={() => setExpanded(!expanded)}>
      <ChevronDown size={16} className={expanded ? "" : "collapsed"} aria-hidden="true" />
      <strong>{statusLabel(status)}</strong><span>{count}</span>
    </button>
    {expanded && <div className="task-list">{children}</div>}
  </section>;
}

export function TaskBoard({ tasks, agents, assigneeOptions, titleDrafts, onTitleChange, onTitleSave, onAssign, onStatusChange, onOpen }: Props) {
  const [filter, setFilter] = useState<Filter>("all");
  const agentsById = useMemo(() => new Map(agents.map((agent) => [agent.id, agent])), [agents]);
  const matches = (task: Task, value: Filter) => value === "all"
    || (value === "active" && task.status !== "done")
    || (value === "review" && task.status === "in_review")
    || (value === "unassigned" && task.status !== "done" && !task.assignee_id);
  const filters: Array<{ value: Filter; label: string }> = [
    { value: "all", label: "Total" }, { value: "active", label: "Active" },
    { value: "review", label: "In review" }, { value: "unassigned", label: "Unassigned" },
  ];
  const visible = tasks.filter((task) => matches(task, filter));
  const statuses = [...new Set(["in_review", "in_progress", "todo", "done", ...visible.map((task) => task.status)])];
  return <div className="task-board">
    <section className="task-board-summary" aria-label="Filter tasks">
      {filters.map(({ value, label }) => <button key={value} type="button" aria-pressed={filter === value}
        onClick={() => setFilter(value)} aria-label={`${label} tasks`}>
        <strong>{tasks.filter((task) => matches(task, value)).length}</strong><span>{label}</span>
      </button>)}
    </section>
    {visible.length === 0 ? <div className="empty-state">
      <LayoutList size={34} /><h2>{tasks.length ? "No matching tasks" : "No tasks in this channel"}</h2>
      <p>{tasks.length ? "Choose another filter to see more tasks." : "Create tracked work from chat by sending a message in Task mode."}</p>
    </div> : <div className="task-sections">
      {statuses.map((status) => {
        const group = visible.filter((task) => task.status === status);
        return group.length > 0 && <TaskGroup key={`${filter}:${status}`} status={status} count={group.length}>
          {group.map((task) => <article className="task-row" key={task.id} data-task-number={task.number}>
            <TaskTitle task={task} value={titleDrafts[task.id] ?? task.title} onChange={onTitleChange} onSave={onTitleSave} />
            <div className="task-row-meta">
              <button type="button" className="task-row-open" onClick={() => onOpen(task)} aria-label={`Open task #${task.number} thread`}>
                <MessageSquare size={14} aria-hidden="true" /> #{task.number}
              </button>
              <time>Updated {formatTime(task.updated_at)}</time>
              <div className="task-row-actions">
                <label className="task-status-pill" data-state={status}>
                  <select aria-label={`Task #${task.number} status`} value={status} onChange={(event) => onStatusChange(task, event.target.value)}>
                    {!TASK_STATUSES.includes(status as typeof TASK_STATUSES[number]) && <option value={status}>{statusLabel(status)}</option>}
                    {TASK_STATUSES.map((value) => <option key={value} value={value}>{statusLabel(value)}</option>)}
                  </select>
                  <ChevronDown size={14} aria-hidden="true" />
                </label>
                <TaskAssigneePicker agents={assigneeOptions} assignee={agentsById.get(task.assignee_id ?? "") ?? null}
                  disabled={status === "done"} done={status === "done"} compact
                  onChange={(agentId) => onAssign(task, agentId)} taskNumber={task.number} />
              </div>
            </div>
          </article>)}
        </TaskGroup>;
      })}
    </div>}
  </div>;
}
