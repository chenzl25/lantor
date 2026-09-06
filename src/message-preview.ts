export const DESKTOP_MESSAGE_PREVIEW_LINES = 48;
export const DESKTOP_MESSAGE_PREVIEW_CHARS = 8000;

export function shouldCollapseMessage(body: string) {
  const text = body.trim();
  if (text.length > DESKTOP_MESSAGE_PREVIEW_CHARS) return true;
  let lines = 1;
  for (let index = 0; index < text.length; index += 1) {
    if (text.charCodeAt(index) === 10 && ++lines > DESKTOP_MESSAGE_PREVIEW_LINES) return true;
  }
  return false;
}
