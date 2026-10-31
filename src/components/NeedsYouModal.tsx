import { CheckCircle2, ClipboardCheck, Hand, Hourglass, MessageSquare } from "lucide-react";
import { useState, type ReactNode } from "react";
import { formatIdle, type NeedsYou, type NeedsYouTask } from "../decisions";
import type { Agent, Decision, Task } from "../types";
import { formatTime } from "../ui-utils";
import { AgentAvatar } from "./AgentAvatar";
import { DecisionCard } from "./DecisionCard";
import { DialogHeader } from "./DialogHeader";
import { DialogSurface } from "./DialogSurface";
import { PushNotificationsRow } from "./PushNotificationsRow";

type NeedsYouModalProps = {
  open: boolean;
  mobileNavigation?: ReactNode;
  needsYou: NeedsYou;
  agents: Agent[];
  onOpenDecision: (decision: Decision) => void;
  onOpenTask: (task: Task) => void;
  onMarkTaskDone: (task: Task) => Promise<void>;
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
  mobileNavigation,
  needsYou,
  agents,
  onOpenDecision,
  onOpenTask,
  onMarkTaskDone,
  onClose,
}: NeedsYouModalProps) {
  // Hide a row as soon as Done is clicked; the row returns if the update fails.
  const [completing, setCompleting] = useState<ReadonlySet<string>>(() => new Set());
  if (!open) return null;
  const markDone = (task: Task) => {
    setCompleting((current) => new Set(current).add(task.id));
    void onMarkTaskDone(task).catch(() => undefined).finally(() => {
      setCompleting((current) => {
        const next = new Set(current);
        next.delete(task.id);
        return next;
      });
    });
  };
  const { decisions, stalled } = needsYou;
  const reviews = needsYou.reviews.filter((item) => !completing.has(item.task.id));
  const empty = decisions.length + reviews.length + stalled.length === 0;
  const summary = [
    decisions.length ? `${decisions.length} ${decisions.length === 1 ? "decision" : "decisions"}` : null,
    reviews.length ? `${reviews.length} to review` : null,
    stalled.length ? `${stalled.length} stuck` : null,
  ].filter(Boolean).join(" · ");

  return (
    <DialogSurface label="Needs you" backdropClassName="search-backdrop" className="activity-feed-panel needs-you-panel" onClose={onClose}>
      <DialogHeader title="Needs you" subtitle={empty ? "Nothing is waiting on you" : summary}
        closeLabel="Close Needs you" onClose={onClose} />

      <div className="activity-feed-body needs-you-body">
        <PushNotificationsRow />
        {empty && (
          <div className="search-empty">
            <Hand size={34} />
            <h3>All clear</h3>
            <p>Decisions and reviews for you show up here.</p>
          </div>
        )}

        <Section icon={<Hand size={16} />} title="Decisions" hint="Agents wait for your answer" count={decisions.length}>
          {decisions.map((decision) => {
            const requester = agents.find((agent) => agent.id === decision.requester_agent_id) ?? null;
            return (
              <article key={decision.id} className="needs-you-decision">
                <button type="button" className="needs-you-decision-meta" onClick={() => onOpenDecision(decision)}
                  aria-label={`Open the conversation for ${decision.title}`}>
                  {requester && <AgentAvatar agent={requester} size="sm" showStatus={false} />}
                  <strong>{decision.requester_handle ? `@${decision.requester_handle}` : decision.requester_agent_id ? "Agent" : "Lantor"}</strong>
                  <span>#{decision.channel_name}</span>
                  <time dateTime={decision.created_at}>{formatTime(decision.created_at)}</time>
                  <MessageSquare size={14} />
                </button>
                <DecisionCard decision={decision} compact />
              </article>
            );
          })}
        </Section>

        <Section icon={<ClipboardCheck size={16} />} title="To review" hint="Agents say these are done" count={reviews.length}>
          {reviews.map((item) => (
            <TaskRow key={item.task.id} item={item} agents={agents} onOpen={onOpenTask} onDone={markDone} />
          ))}
        </Section>

        <Section icon={<Hourglass size={16} />} title="Stuck" hint="No progress in 3+ days" count={stalled.length}>
          {stalled.map((item) => (
            <TaskRow key={item.task.id} item={item} agents={agents} onOpen={onOpenTask} />
          ))}
        </Section>
      </div>
      {mobileNavigation}
    </DialogSurface>
  );
}
