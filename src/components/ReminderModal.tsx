import { Bell, Check, Clock3, Repeat2, TimerReset, X } from "lucide-react";
import type { Channel, Reminder } from "../types";
import { APP_DISPLAY_NAME } from "../branding";
import { formatTime } from "../ui-utils";
import { Modal } from "./Modal";

type ReminderModalProps = {
  open: boolean;
  reminders: Reminder[];
  channels: Channel[];
  currentChannel: Channel | null;
  title: string;
  note: string;
  dueAt: string;
  recurrence: string;
  includeThread: boolean;
  activeThreadId: string | null;
  onTitleChange: (value: string) => void;
  onNoteChange: (value: string) => void;
  onDueAtChange: (value: string) => void;
  onRecurrenceChange: (value: string) => void;
  onIncludeThreadChange: (value: boolean) => void;
  onCreate: () => void;
  onSnooze: (reminder: Reminder, minutes: number) => void;
  onComplete: (reminder: Reminder) => void;
  onCancelReminder: (reminder: Reminder) => void;
  onClose: () => void;
};

function channelLabel(channels: Channel[], reminder: Reminder) {
  if (reminder.channel_name) return `#${reminder.channel_name}`;
  const channel = channels.find((item) => item.id === reminder.channel_id);
  if (!channel) return "No channel";
  return channel.kind === "dm" ? "Direct message" : `#${channel.name}`;
}

function dueLabel(reminder: Reminder) {
  const due = new Date(reminder.due_at);
  if (Number.isNaN(due.getTime())) return formatTime(reminder.due_at);
  const delta = due.getTime() - Date.now();
  const abs = Math.abs(delta);
  const minutes = Math.max(1, Math.round(abs / 60_000));
  const suffix = delta < 0 ? "ago" : "from now";
  if (minutes < 60) return `${minutes}m ${suffix}`;
  const hours = Math.round(minutes / 60);
  if (hours < 48) return `${hours}h ${suffix}`;
  return due.toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

function datetimeLocalValue(date: Date) {
  const offset = date.getTimezoneOffset();
  const local = new Date(date.getTime() - offset * 60_000);
  return local.toISOString().slice(0, 16);
}

export function ReminderModal({
  open,
  reminders,
  channels,
  currentChannel,
  title,
  note,
  dueAt,
  recurrence,
  includeThread,
  activeThreadId,
  onTitleChange,
  onNoteChange,
  onDueAtChange,
  onRecurrenceChange,
  onIncludeThreadChange,
  onCreate,
  onSnooze,
  onComplete,
  onCancelReminder,
  onClose,
}: ReminderModalProps) {
  const scheduled = reminders.filter((reminder) => reminder.status === "scheduled");
  const fired = reminders.filter((reminder) => reminder.status === "fired");
  const canCreate = title.trim() && dueAt.trim();
  const nextReminder = scheduled[0] ?? null;
  const quickDue = (minutes: number) => {
    const date = new Date(Date.now() + minutes * 60_000);
    date.setSeconds(0, 0);
    onDueAtChange(datetimeLocalValue(date));
  };

  return (
    <Modal
      open={open}
      title="Reminders"
      onClose={onClose}
      width={880}
    >
      <div className="reminder-modal">
        <section className="reminder-create">
          <div className="reminder-hero">
            <div className="reminder-icon"><Bell size={18} /></div>
            <div>
              <strong>New reminder</strong>
              <span>{currentChannel ? `Anchored to ${currentChannel.kind === "dm" ? "this DM" : `#${currentChannel.name}`}` : "No active channel selected"}</span>
            </div>
          </div>
          <div className="reminder-quick-grid" aria-label="Quick reminder times">
            <button type="button" onClick={() => quickDue(30)}>30m</button>
            <button type="button" onClick={() => quickDue(60)}>1h</button>
            <button type="button" onClick={() => quickDue(24 * 60)}>Tomorrow</button>
          </div>
          <input
            value={title}
            onChange={(event) => onTitleChange(event.target.value)}
            placeholder={`What should ${APP_DISPLAY_NAME} remind you about?`}
          />
          <textarea
            value={note}
            onChange={(event) => onNoteChange(event.target.value)}
            placeholder="Optional note"
          />
          <div className="reminder-create-grid">
            <label>
              Due
              <input type="datetime-local" value={dueAt} onChange={(event) => onDueAtChange(event.target.value)} />
            </label>
            <label>
              Repeat
              <select value={recurrence} onChange={(event) => onRecurrenceChange(event.target.value)}>
                <option value="none">No repeat</option>
                <option value="daily">Daily</option>
                <option value="weekly">Weekly</option>
              </select>
            </label>
          </div>
          <label className={`reminder-thread-toggle ${!activeThreadId ? "disabled" : ""}`}>
            <input
              type="checkbox"
              checked={includeThread && Boolean(activeThreadId)}
              disabled={!activeThreadId}
              onChange={(event) => onIncludeThreadChange(event.target.checked)}
            />
            <span>Anchor to current thread</span>
          </label>
          <button className="primary" disabled={!canCreate} onClick={onCreate}>
            Create reminder
          </button>
        </section>

        <section className="reminder-list">
          <div className="reminder-summary">
            <div>
              <span>Due now</span>
              <strong>{fired.length}</strong>
            </div>
            <div>
              <span>Scheduled</span>
              <strong>{scheduled.length}</strong>
            </div>
            <div>
              <span>Next</span>
              <strong>{nextReminder ? dueLabel(nextReminder) : "None"}</strong>
            </div>
          </div>
          {fired.length > 0 && (
            <div className="reminder-group">
              <div className="reminder-group-head">
                <h4>Due now</h4>
                <span>{fired.length}</span>
              </div>
              {fired.map((reminder) => (
                <article key={reminder.id} className="reminder-row fired">
                  <div className="reminder-row-icon"><TimerReset size={15} /></div>
                  <div className="reminder-row-body">
                    <div className="reminder-row-title">
                      <strong>{reminder.title}</strong>
                      <span className="reminder-status due">Due</span>
                    </div>
                    <span>{channelLabel(channels, reminder)} · {dueLabel(reminder)}{reminder.creator_agent_handle ? ` · by @${reminder.creator_agent_handle}` : ""}</span>
                    {reminder.note && <p>{reminder.note}</p>}
                  </div>
                  <div className="reminder-row-actions">
                    <button onClick={() => onSnooze(reminder, 10)}>Snooze 10m</button>
                    <button className="icon" onClick={() => onComplete(reminder)} title="Complete"><Check size={15} /></button>
                  </div>
                </article>
              ))}
            </div>
          )}

          <div className="reminder-group">
            <div className="reminder-group-head">
              <h4>Scheduled</h4>
              <span>{scheduled.length}</span>
            </div>
            {scheduled.length === 0 && <p className="empty-mini">No scheduled reminders.</p>}
            {scheduled.map((reminder) => (
              <article key={reminder.id} className="reminder-row">
                <div className="reminder-row-icon">{reminder.recurrence !== "none" ? <Repeat2 size={15} /> : <Clock3 size={15} />}</div>
                <div className="reminder-row-body">
                  <div className="reminder-row-title">
                    <strong>{reminder.title}</strong>
                    {reminder.recurrence !== "none" && <span className="reminder-status">{reminder.recurrence}</span>}
                  </div>
                  <span>{channelLabel(channels, reminder)} · {dueLabel(reminder)}{reminder.creator_agent_handle ? ` · by @${reminder.creator_agent_handle}` : ""}</span>
                  {reminder.note && <p>{reminder.note}</p>}
                </div>
                <div className="reminder-row-actions">
                  <button onClick={() => onSnooze(reminder, 60)}>+1h</button>
                  <button className="icon danger" onClick={() => onCancelReminder(reminder)} title="Cancel"><X size={15} /></button>
                </div>
              </article>
            ))}
          </div>
        </section>
      </div>
    </Modal>
  );
}
