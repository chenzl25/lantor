import assert from "node:assert/strict";
import test from "node:test";

import { supervisorStatusLines } from "../src/supervisor-status";
import type { SupervisorStatus } from "../src/types";
import { formatTime } from "../src/ui-utils";

const startedAt = "2026-11-13T05:20:00.000Z";

function status(overrides: Partial<SupervisorStatus>): SupervisorStatus {
  return { pid: 42, status: "running", updated_at: startedAt, started_at: startedAt, ...overrides };
}

test("a running supervisor says since when", () => {
  assert.deepEqual(supervisorStatusLines(status({})), [`Running since ${formatTime(startedAt)}.`]);
  assert.deepEqual(supervisorStatusLines(status({ started_at: null })), ["Running."]);
});

test("a newer build waiting for idle agents is mentioned", () => {
  assert.deepEqual(supervisorStatusLines(status({ update_waiting_since: startedAt })), [
    `Running since ${formatTime(startedAt)}.`,
    "A newer build is ready. It takes over when no agent is working.",
  ]);
  assert.deepEqual(supervisorStatusLines(status({ status: "restarting", update_waiting_since: startedAt })), [
    "Restarting with a newer build.",
  ]);
});

test("a build that could not start says why", () => {
  assert.deepEqual(supervisorStatusLines(status({ update_error: "Permission denied (os error 13)" })), [
    `Running since ${formatTime(startedAt)}.`,
    "The newer build could not start: Permission denied (os error 13)",
  ]);
});

test("a supervisor that is gone or silent says so", () => {
  assert.deepEqual(supervisorStatusLines(undefined), ["Not running."]);
  assert.deepEqual(supervisorStatusLines(status({ pid: null, status: "offline" })), ["Not running."]);
  assert.deepEqual(supervisorStatusLines(status({ status: "stale" })), [`Not responding since ${formatTime(startedAt)}.`]);
});
