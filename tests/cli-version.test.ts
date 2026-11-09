import assert from "node:assert/strict";
import test from "node:test";

import { CliVersionStore } from "../src/cli-versions";
import { cliFixRequest, cliVersionView } from "../src/components/AgentCliVersion";
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

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}

/** A store over a fake API whose answers the test releases one by one. */
function fixtureStore() {
  let clock = 1_000;
  const pending: { kind: "load" | "update"; answer: ReturnType<typeof deferred<CliVersionStatus>> }[] = [];
  const call = (kind: "load" | "update") => {
    const answer = deferred<CliVersionStatus>();
    pending.push({ kind, answer });
    return answer.promise;
  };
  const store = new CliVersionStore({ load: () => call("load"), update: () => call("update") }, () => clock);
  return {
    store,
    pending,
    advance: (ms: number) => { clock += ms; },
    settled: () => new Promise((done) => setImmediate(done)),
  };
}

const current: CliVersionStatus = { ...failed, update_available: true, last_update: null };

test("hover cards reuse a recent status instead of reading the CLI again", async () => {
  const { store, pending, advance, settled } = fixtureStore();
  store.refresh("codex", 60_000);
  store.refresh("codex", 60_000);
  assert.equal(pending.length, 1, "a read on its way is shared");
  pending[0].answer.resolve(current);
  await settled();
  assert.equal(store.get("codex").status, current);

  advance(30_000);
  store.refresh("codex", 60_000);
  assert.equal(pending.length, 1, "a status read 30 s ago is reused");
  store.refresh("codex", 0);
  assert.equal(pending.length, 2, "the profile reads it again");
  pending[1].answer.resolve(current);
  await settled();

  advance(61_000);
  store.refresh("codex", 60_000);
  assert.equal(pending.length, 3, "an older status is read again");
});

test("a running update is always read again, and a late read cannot undo its answer", async () => {
  const { store, pending, settled } = fixtureStore();
  store.refresh("codex", 60_000);
  const update = store.update("codex");
  assert.equal(store.get("codex").starting, true);
  pending[1].answer.resolve({ ...current, updating: true });
  await update;
  pending[0].answer.resolve(current);
  await settled();
  assert.equal(store.get("codex").status?.updating, true, "the read sent before the update is dropped");
  assert.equal(store.get("codex").starting, false);

  store.refresh("codex", 60_000);
  assert.equal(pending.length, 3, "a status that says updating is never reused");
  pending[2].answer.resolve({ ...current, installed: "0.168.0", update_available: false });
  await settled();
  assert.equal(store.get("codex").status?.installed, "0.168.0");
});

test("a failed read is tried again on the next hover", async () => {
  const { store, pending, settled } = fixtureStore();
  store.refresh("claude", 60_000);
  pending[0].answer.reject(new Error("offline"));
  await settled();
  assert.equal(store.get("claude").loadFailed, true);
  store.refresh("claude", 60_000);
  assert.equal(pending.length, 2);
});

test("the hover card shows a line only when there is something to do or see", () => {
  const view = (status: CliVersionStatus | null, starting = false) => cliVersionView({ status, loadFailed: false, starting });
  assert.equal(view({ ...current, update_available: false, installed: "0.168.0" }).canUpdate, false);
  assert.equal(view(current).canUpdate, true);
  assert.deepEqual([view(current, true).updating, view(current, true).canUpdate], [true, false]);
  assert.deepEqual([view(failed).failed, view(failed).canUpdate], [true, false], "Try again replaces Update after a failure");
  assert.equal(view({ ...failed, update_available: false, installed: "0.168.0" }).failed, false, "a failure fixed elsewhere is not shown");
});
