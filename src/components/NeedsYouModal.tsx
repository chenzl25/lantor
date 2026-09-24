import { CheckCircle2, ClipboardCheck, Hand, Hourglass, MessageSquare, X } from "lucide-react";
import type { ReactNode } from "react";
import { formatIdle, type NeedsYou, type NeedsYouTask } from "../decisions";
import type { Agent, Decision, Task } from "../types";
import { formatTime } from "../ui-utils";
import { AgentAvatar } from "./AgentAvatar";
import { DecisionCard } from "./DecisionCard";
import { DialogSurface } from "./DialogSurface";
import { PushNotificationsRow } from "./PushNotificationsRow";

type NeedsYouModalProps = {
  open: boolean;
  needsYou: NeedsYou;
  agents: Agent[];
  onOpenDecision: (decision: Decision) => void;
  onOpenTask: (task: Task) => void;
  onMarkTaskDone: (task: Task) => void;
  onClose: () => void;
};

function Section({ icon, title, hint, count, children }: {
  icon: ReactNode;
  title: string;
  hint: string;
  count: number;
  children: ReactNode;
}) {
  if (count === 0) return null;
  return (
    <section className="needs-you-section" aria-label={title}>
      <header className="needs-you-section-head">
        {icon}
        <h3>{title}</h3>
        <span className="needs-you-count">{count}</span>
        <small>{hint}</small>
      </header>
      {children}
    </section>
  );
}

function TaskRow({ item, agents, onOpen, onDone }: {
  item: NeedsYouTask;
  agents: Agent[];
  onOpen: (task: Task) => void;
  onDone?: (task: Task) => void;
}) {
  const { task } = item;
  const assignee = agents.find((agent) => agent.id === task.assignee_id) ?? null;
  return (
    <article className="needs-you-task">
      <button type="button" className="needs-you-task-main" onClick={() => onOpen(task)}
        aria-label={`Open task #${task.number}: ${task.title}`}>
        <span className="needs-you-task-avatar" aria-hidden="true">
          {assignee ? <AgentAvatar agent={assignee} size="sm" showStatus={false} /> : <ClipboardCheck size={16} />}
        </span>
        <span className="needs-you-task-text">
          <strong>#{task.number} {task.title}</strong>
          <small>
            #{task.channel_name}
            {" · "}{assignee ? `@${assignee.handle}` : task.assignee_name ?? "unassigned"}
            {" · "}{task.status.replace("_", " ")}
            {" · "}{formatIdle(item.idleMs)}
          </small>
        </span>
      </button>
      {onDone && (
        <button type="button" className="needs-you-task-done" onClick={() => onDone(task)}
          title="Mark done" aria-label={`Mark task #${task.number} done`}>
          <CheckCircle2 size={16} /> Done
        </button>
      )}
    </article>
  );
}

export function NeedsYouModal({
  open,
  needsYou,
  agents,
  onOpenDecision,
  onOpenTask,
  onMarkTaskDone,
  onClose,
}: NeedsYouModalProps) {
  if (!open) return null;
  const { decisions, reviews, stalled } = needsYou;
  const empty = decisions.length + reviews.length + stalled.length === 0;
  const summary = [
    decisions.length ? `${decisions.length} ${decisions.length === 1 ? "decision" : "decisions"}` : null,
    reviews.length ? `${reviews.length} in review` : null,
    stalled.length ? `${stalled.length} stalled` : null,
  ].filter(Boolean).join(" · ");

  return (
    <DialogSurface label="Needs you" backdropClassName="search-backdrop" className="activity-feed-panel needs-you-panel" onClose={onClose}>
      <header className="activity-feed-head">
        <div>
          <h2>Needs you</h2>
          <p>{empty ? "Nothing is waiting on you" : summary}</p>
        </div>
        <button className="activity-feed-back" onClick={onClose} aria-label="Close Needs you">
          <X size={18} />
        </button>
      </header>

      <div className="activity-feed-body needs-you-body">
        <PushNotificationsRow />
        {empty && (
          <div className="search-empty">
            <Hand size={34} />
            <h3>All clear</h3>
            <p>Agent decisions, tasks waiting for your review, and stalled tasks show up here.</p>
          </div>
        )}

        <Section icon={<Hand size={16} />} title="Decisions" hint="Agents are blocked until you choose" count={decisions.length}>
          {decisions.map((decision) => {
            const requester = agents.find((agent) => agent.id === decision.requester_agent_id) ?? null;
            return (
              <article key={decision.id} className="needs-you-decision">
                <button type="button" className="needs-you-decision-meta" onClick={() => onOpenDecision(decision)}
                  aria-label={`Open the conversation for ${decision.title}`}>
                  {requester && <AgentAvatar agent={requester} size="sm" showStatus={false} />}
                  <strong>{decision.requester_handle ? `@${decision.requester_handle}` : "Agent"}</strong>
                  <span>#{decision.channel_name}</span>
                  <time dateTime={decision.created_at}>{formatTime(decision.created_at)}</time>
                  <MessageSquare size={14} />
                </button>
                <DecisionCard decision={decision} compact />
              </article>
            );
          })}
        </Section>

        <Section icon={<ClipboardCheck size={16} />} title="Awaiting your review" hint="Agents marked these ready" count={reviews.length}>
          {reviews.map((item) => (
            <TaskRow key={item.task.id} item={item} agents={agents} onOpen={onOpenTask} onDone={onMarkTaskDone} />
          ))}
        </Section>

        <Section icon={<Hourglass size={16} />} title="Stalled" hint="Active tasks with no movement for 3+ days" count={stalled.length}>
          {stalled.map((item) => (
            <TaskRow key={item.task.id} item={item} agents={agents} onOpen={onOpenTask} />
          ))}
        </Section>
      </div>
    </DialogSurface>
  );
}
