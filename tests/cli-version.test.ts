import assert from "node:assert/strict";
import test from "node:test";

import { cliFixRequest } from "../src/components/AgentCliVersion";
import type { CliVersionStatus } from "../src/types";

const failed: CliVersionStatus = {
  runtime: "codex",
  command: "codex",
  installed: "0.167.0",
  latest: "0.168.0",
  update_available: true,
  updating: false,
  problem: null,
  last_update: {
    ok: false,
    command: "codex update",
    from: "0.167.0",
    to: "0.167.0",
    output: "npm ERR! code EACCES\nnpm ERR! permission denied",
    finished_at: "2026-01-02T03:04:05.000Z",
  },
  checked_at: "2026-01-02T03:04:06.000Z",
};

test("a failed update becomes a request the agent can act on", () => {
  assert.equal(
    cliFixRequest(failed),
    [
      "`codex update` failed on this Mac. Codex is 0.167.0; the latest release is 0.168.0.",
      "",
      "Please find out why, fix it, and update Codex to the latest release. Every Codex agent here uses this install. Tell me what you changed.",
      "",
      "Output:",
      "```text",
      "npm ERR! code EACCES",
      "npm ERR! permission denied",
      "```",
    ].join("\n"),
  );
});

test("output that contains a code fence gets a longer fence", () => {
  const body = cliFixRequest({
    ...failed,
    runtime: "claude",
    command: "claude",
    installed: null,
    last_update: { ...failed.last_update!, command: "claude update", output: "see ```log```" },
  });
  assert.match(body, /^`claude update` failed on this Mac\. Claude Code is not found;/);
  assert.ok(body.endsWith("````text\nsee ```log```\n````"), body);
});
