export const UI_ERROR_EVENT = "lantor-ui-error";

export function reportUiError(message: string, error: unknown) {
  const detail = error instanceof Error ? error.message : String(error || "Unknown error");
  window.dispatchEvent(new CustomEvent<string>(UI_ERROR_EVENT, { detail: `${message}: ${detail}` }));
}
