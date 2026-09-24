import { Bell, BellOff, BellRing, Smartphone } from "lucide-react";
import { useEffect, useState, type ReactNode } from "react";
import { disablePush, enablePush, readPushState, sendTestPush, type PushState } from "../web-push";

const COPY: Record<Exclude<PushState, "unsupported">, { icon: ReactNode; title: string; hint: string }> = {
  install: {
    icon: <Smartphone size={18} />,
    title: "Get notified on this phone",
    hint: "Add Lantor to your Home Screen (Share → Add to Home Screen), then open it from there.",
  },
  denied: {
    icon: <BellOff size={18} />,
    title: "Notifications are blocked",
    hint: "Allow notifications for Lantor in system settings to get alerts here.",
  },
  off: {
    icon: <Bell size={18} />,
    title: "Notify this device",
    hint: "Get an alert when an agent needs your call or a task is ready for review.",
  },
  on: {
    icon: <BellRing size={18} />,
    title: "Notifications are on",
    hint: "This device gets an alert for new decisions and reviews.",
  },
};

/** Web Push opt-in for the current browser; hidden where push cannot work. */
export function PushNotificationsRow() {
  const [state, setState] = useState<PushState | null>(null);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    readPushState()
      .then((next) => { if (!cancelled) setState(next); })
      .catch(() => { if (!cancelled) setState("unsupported"); });
    return () => { cancelled = true; };
  }, []);

  if (!state || state === "unsupported") return null;
  const copy = COPY[state];

  async function run(action: () => Promise<PushState | string>) {
    setBusy(true);
    setNote(null);
    try {
      const result = await action();
      if (result === "unsupported" || result === "install" || result === "denied" || result === "off" || result === "on") {
        setState(result);
      } else {
        setNote(result);
      }
    } catch (error) {
      setNote(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  }

  async function test() {
    const result = await sendTestPush();
    return result.delivered ? "Test sent. It should arrive in a few seconds." : result.error ?? "The test was not delivered.";
  }

  return (
    <section className={`needs-you-push is-${state}`} aria-label="Notifications on this device">
      <span className="needs-you-push-icon" aria-hidden="true">{copy.icon}</span>
      <span className="needs-you-push-text">
        <strong>{copy.title}</strong>
        <small>{note ?? copy.hint}</small>
      </span>
      {state === "off" && (
        <button type="button" className="needs-you-push-action is-primary" disabled={busy} onClick={() => void run(enablePush)}>
          Turn on
        </button>
      )}
      {state === "on" && (
        <span className="needs-you-push-actions">
          <button type="button" className="needs-you-push-action" disabled={busy} onClick={() => void run(test)}>
            Test
          </button>
          <button type="button" className="needs-you-push-action" disabled={busy} onClick={() => void run(disablePush)}>
            Turn off
          </button>
        </span>
      )}
    </section>
  );
}
