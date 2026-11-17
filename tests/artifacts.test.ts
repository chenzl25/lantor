import assert from "node:assert/strict";
import test from "node:test";
import { artifactFileName, bodyOnlyAnnouncesArtifact, cachedArtifact, readArtifact, withoutRepeatedTitle } from "../src/artifacts";
import type { Artifact } from "../src/types";

const artifact = (extra: Partial<Artifact> = {}): Artifact => ({
  id: "00000000-0000-4000-8000-000000000001", message_id: "m1", channel_id: "c1", thread_root_id: null,
  creator_agent_id: null, creator_agent_handle: "arch", kind: "markdown", title: "Deploy review",
  summary: "Two risks, one blocker.", content: "", metadata: {}, created_at: "2026-11-17T03:00:00Z",
  updated_at: "2026-11-17T03:00:00Z", ...extra,
});

test("a body that only announces the artifact is left to the card", () => {
  const report = artifact();
  assert.equal(bodyOnlyAnnouncesArtifact({ body: "Created artifact: Deploy review\n\nTwo risks, one blocker.", artifacts: [report] }), true);
  assert.equal(bodyOnlyAnnouncesArtifact({ body: "Created artifact: Deploy review", artifacts: [artifact({ summary: "" })] }), true);
  // Anything the agent wrote besides the announcement stays visible.
  assert.equal(bodyOnlyAnnouncesArtifact({ body: "Created artifact: Deploy review\n\nTwo risks, one blocker.\n\nMerge after CI.", artifacts: [report] }), false);
  assert.equal(bodyOnlyAnnouncesArtifact({ body: "Created artifact: Deploy review", artifacts: [] }), false);
  assert.equal(bodyOnlyAnnouncesArtifact({ body: "Created artifact: Other report", artifacts: [report] }), false);
});

test("the reader drops a leading heading that only repeats the title", () => {
  const body = "Intro paragraph.\n\n## Details\n";
  assert.equal(withoutRepeatedTitle(`# Deploy review\n\n${body}`, "Deploy review"), body);
  assert.equal(withoutRepeatedTitle(`\n# Deploy  Review #\n${body}`, "Deploy review"), body);
  // Punctuation and spacing differ between half- and full-width forms.
  assert.equal(withoutRepeatedTitle(`# 设计:控制事件\n\n${body}`, "设计：控制事件 Freshness Hold"), body);
  // A heading that says more than the title is part of the report.
  const longer = `# Deploy review (staging, 11-17)\n\n${body}`;
  assert.equal(withoutRepeatedTitle(longer, "Deploy review"), longer);
  const other = `# Rollout plan\n\n${body}`;
  assert.equal(withoutRepeatedTitle(other, "Deploy review"), other);
  // Only a first-level heading at the very start counts.
  const second = `## Deploy review\n\n${body}`;
  assert.equal(withoutRepeatedTitle(second, "Deploy review"), second);
  const later = `Intro.\n\n# Deploy review\n`;
  assert.equal(withoutRepeatedTitle(later, "Deploy review"), later);
});

test("report downloads are named after the title", () => {
  assert.equal(artifactFileName(artifact({ title: "rw#27960 逐文件说明" })), "rw#27960 逐文件说明.md");
  assert.equal(artifactFileName(artifact({ title: "a/b: c?\n" })), "a b c.md");
  assert.equal(artifactFileName(artifact({ title: "../.." })), "report.md");
  assert.equal(artifactFileName(artifact({ title: "Page", kind: "html" })), "Page.txt");
});

test("a report read once opens again without another read until it changes", async () => {
  const summary = artifact({ id: "00000000-0000-4000-8000-000000000002" });
  const full = { ...summary, content: "# Deploy review\n\nBody" };
  const unavailable = async (): Promise<Artifact> => { throw new Error("offline"); };
  assert.equal(cachedArtifact(summary), null);
  assert.deepEqual(await readArtifact(summary, async () => full), full);
  assert.deepEqual(cachedArtifact(summary), full);
  assert.deepEqual(await readArtifact(summary, unavailable), full);
  const changed = { ...summary, updated_at: "2026-11-17T04:00:00Z" };
  assert.equal(cachedArtifact(changed), null);
  await assert.rejects(readArtifact(changed, unavailable), /offline/);
  // An artifact that arrives with its content needs no read at all.
  assert.equal(cachedArtifact(full), full);
});
