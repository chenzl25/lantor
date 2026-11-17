import type { Artifact, Message } from "./types";

/**
 * The message that carries an artifact has the body "Created artifact: <title>",
 * followed by the summary, so previews, notifications and search have text.
 * In the conversation the artifact card shows the same title and summary.
 */
export function bodyOnlyAnnouncesArtifact(message: Pick<Message, "body" | "artifacts">) {
  if (!Array.isArray(message.artifacts)) return false;
  const body = message.body.trim();
  return message.artifacts.some((artifact) => {
    if (typeof artifact?.title !== "string") return false;
    const announcement = `Created artifact: ${artifact.title.trim()}`;
    const summary = typeof artifact.summary === "string" ? artifact.summary.trim() : "";
    return body === announcement || (summary !== "" && body === `${announcement}\n\n${summary}`);
  });
}

function comparableHeading(text: string) {
  return text.toLowerCase().replace(/[\s\p{P}\p{S}]+/gu, "");
}

/**
 * The reader shows the title above the content, and most reports open with a
 * "# heading" of their own. Drop that heading when it says nothing the title
 * does not: the same words, or the start of the title. A heading that differs
 * from the title stays.
 */
export function withoutRepeatedTitle(content: string, title: string) {
  const match = /^[ \t]*(?:\r?\n[ \t]*)*#[ \t]+([^\r\n]*)(?:\r?\n|$)/.exec(content);
  if (!match) return content;
  const heading = comparableHeading(match[1]);
  if (!heading || !comparableHeading(title).startsWith(heading)) return content;
  return content.slice(match[0].length).replace(/^(?:[ \t]*\r?\n)+/, "");
}

/** A download name for the report: its title without path or reserved characters. */
export function artifactFileName(artifact: Pick<Artifact, "title" | "kind">) {
  const base = artifact.title
    .replace(/[\\/:*?"<>|\p{Cc}]+/gu, " ")
    .replace(/\s+/g, " ")
    .slice(0, 80)
    .replace(/^[.\s]+|[.\s]+$/g, "");
  return `${base || "report"}.${artifact.kind === "markdown" ? "md" : "txt"}`;
}

/**
 * Conversations load artifacts without their content. A report read once stays
 * here, so opening it again shows it at once. An artifact whose updated_at
 * changed is read again.
 */
const CACHE_LIMIT = 24;
const cache = new Map<string, Artifact>();

export function cachedArtifact(artifact: Artifact): Artifact | null {
  if (artifact.content) return artifact;
  const hit = cache.get(artifact.id);
  return hit && hit.updated_at === artifact.updated_at ? hit : null;
}

export async function readArtifact(artifact: Artifact, read: (artifactId: string) => Promise<Artifact>) {
  const hit = cachedArtifact(artifact);
  if (hit) return hit;
  const full = await read(artifact.id);
  cache.delete(full.id);
  cache.set(full.id, full);
  while (cache.size > CACHE_LIMIT) {
    const oldest = cache.keys().next().value;
    if (oldest === undefined) break;
    cache.delete(oldest);
  }
  return full;
}
