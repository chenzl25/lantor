import assert from "node:assert/strict";
import test from "node:test";

import { CliVersionStore } from "../src/cli-versions";
import { cliCanUpdate, cliUpdateRequest } from "../src/components/AgentCliVersion";
import type { CliVersionStatus } from "../src/types";

const behind: CliVersionStatus = {
  runtime: "codex",
  command: "codex",
  installed: "0.167.0",
  latest: "0.168.0",
  update_available: true,
  problem: null,
  checked_at: "2026-01-02T03:04:06.000Z",
};

test("the update request tells the agent what to update and how to report back", () => {
  assert.ok(cliCanUpdate(behind));
  assert.equal(
    cliUpdateRequest(behind),
    [
      "Codex 0.168.0 is out; this Mac has 0.167.0.",
      "",
      "Please update Codex to the latest release from your shell. `codex update` usually works, since it knows how Codex was installed here. If it fails, find out why and fix it.",
      "",
      "Every Codex agent on this Mac uses this install. Your own process keeps its current version until it next starts. When you're done, run `codex --version` and tell me the version and anything you changed.",
    ].join("\n"),
  );
});

test("an update is offered only when both versions are known and a later one exists", () => {
  assert.equal(cliCanUpdate(null), false);
  assert.equal(cliCanUpdate({ ...behind, update_available: false, installed: "0.168.0" }), false);
  assert.equal(cliCanUpdate({ ...behind, update_available: false, installed: null, problem: "codex not found on this Mac" }), false);
  assert.equal(cliCanUpdate({ ...behind, update_available: false, latest: null, problem: "Couldn't reach the npm registry" }), false);
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
  const pending: ReturnType<typeof deferred<CliVersionStatus>>[] = [];
  const store = new CliVersionStore({
    load: () => {
      const answer = deferred<CliVersionStatus>();
      pending.push(answer);
      return answer.promise;
    },
  }, () => clock);
  return {
    store,
    pending,
    advance: (ms: number) => { clock += ms; },
    settled: () => new Promise((done) => setImmediate(done)),
  };
}

test("hover cards reuse a recent status instead of reading the CLI again", async () => {
  const { store, pending, advance, settled } = fixtureStore();
  store.refresh("codex", 60_000);
  store.refresh("codex", 60_000);
  assert.equal(pending.length, 1, "a read on its way is shared");
  pending[0].resolve(behind);
  await settled();
  assert.equal(store.get("codex").status, behind);

  advance(30_000);
  store.refresh("codex", 60_000);
  assert.equal(pending.length, 1, "a status read 30 s ago is reused");
  store.refresh("codex", 0);
  assert.equal(pending.length, 2, "the profile reads it again");
  pending[1].resolve(behind);
  await settled();

  advance(61_000);
  store.refresh("codex", 60_000);
  assert.equal(pending.length, 3, "an older status is read again");
});

test("after an agent is asked to update, the next hover reads the version again", async () => {
  const { store, pending, settled } = fixtureStore();
  store.refresh("codex", 60_000);
  pending[0].resolve(behind);
  await settled();
  store.invalidate("codex");
  assert.equal(store.get("codex").status, behind, "the card keeps showing what it knows meanwhile");
  store.refresh("codex", 60_000);
  assert.equal(pending.length, 2);
  pending[1].resolve({ ...behind, installed: "0.168.0", update_available: false });
  await settled();
  assert.equal(store.get("codex").status?.installed, "0.168.0");
});

test("a failed read is tried again on the next hover", async () => {
  const { store, pending, settled } = fixtureStore();
  store.refresh("claude", 60_000);
  pending[0].reject(new Error("offline"));
  await settled();
  assert.equal(store.get("claude").loadFailed, true);
  store.refresh("claude", 60_000);
  assert.equal(pending.length, 2);
});
