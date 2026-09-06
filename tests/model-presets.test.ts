import assert from "node:assert/strict";
import test from "node:test";

import {
  codexReasoningEffortsForModel,
  modelLabel,
  modelOptionsForRuntime,
  normalizeCodexReasoningEffortForModel,
} from "../src/types";

test("GPT-6 Astra preset uses the current model id and effort range", () => {
  assert.ok(modelOptionsForRuntime("codex").includes("gpt-6-astra"));
  assert.equal(modelLabel("gpt-6-astra"), "GPT-6 Astra");
  assert.deepEqual(
    codexReasoningEffortsForModel("gpt-6-astra").map((effort) => effort.value),
    ["low", "medium", "high", "xhigh", "max"],
  );
  assert.equal(normalizeCodexReasoningEffortForModel("gpt-6-astra", "ultra"), "max");
});

test("Claude Fable preset targets the current 5.1 model", () => {
  assert.ok(modelOptionsForRuntime("claude").includes("fable"));
  assert.equal(modelLabel("fable"), "Claude Fable 5.1");
  assert.equal(modelLabel("claude-fable-5-1"), "Claude Fable 5.1");
});
