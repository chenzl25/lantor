import { apiInvoke, isTauriRuntime } from "./apiClient";

// A crashed phone or remote browser has no visible console, so every fatal
// render error (and uncaught window error) is posted to the backend, where it
// lands in the activity log with the stack, URL, user agent and shell version.
const MAX_REPORTS_PER_SESSION = 5;
let reported = 0;

export function reportClientCrash(
  error: unknown,
  componentStack: string | null | undefined,
  source: "render" | "window",
) {
  if (reported >= MAX_REPORTS_PER_SESSION) return;
  reported += 1;
  const failure = error instanceof Error ? error : new Error(String(error ?? "Unknown error"));
  try {
    void apiInvoke("report_client_crash", {
      message: failure.message || failure.name || "Unknown error",
      stack: failure.stack ?? null,
      componentStack: componentStack ?? null,
      source,
      runtime: isTauriRuntime() ? "desktop" : "web",
      url: window.location.href,
      userAgent: navigator.userAgent,
      shellVersion: document.querySelector<HTMLMetaElement>('meta[name="lantor-shell-version"]')?.content ?? null,
      viewport: `${window.innerWidth}x${window.innerHeight}`,
    }).catch(() => {});
  } catch {
    // Reporting must never throw from inside an error boundary.
  }
}

let windowErrorsWatched = false;

export function watchWindowErrors() {
  if (windowErrorsWatched || typeof window === "undefined") return;
  windowErrorsWatched = true;
  window.addEventListener("error", (event) => {
    // Resource load failures also fire "error" on window but carry no Error.
    if (!event.error && !event.message) return;
    reportClientCrash(event.error ?? event.message, null, "window");
  });
}
