import assert from "node:assert/strict";
import test from "node:test";

import {
  EMPTY_AGENT_FORM,
  RUNTIME_PRESETS,
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
    ["low", "medium", "high", "xhigh", "max", "ultra"],
  );
  assert.equal(normalizeCodexReasoningEffortForModel("gpt-6-astra", "ultra"), "ultra");
});

test("GPT-6 Sol and Luna presets follow the Codex app-server effort ranges", () => {
  assert.equal(RUNTIME_PRESETS.codex.defaultModel, "gpt-6-sol");
  assert.equal(EMPTY_AGENT_FORM.model, "gpt-6-sol");
  assert.ok(modelOptionsForRuntime("codex").includes("gpt-6-sol"));
  assert.ok(modelOptionsForRuntime("codex").includes("gpt-6-luna"));
  assert.equal(modelLabel("gpt-6-sol"), "GPT-6 Sol");
  assert.equal(modelLabel("gpt-6-luna"), "GPT-6 Luna");
  assert.deepEqual(
    codexReasoningEffortsForModel("gpt-6-sol").map((effort) => effort.value),
    ["low", "medium", "high", "xhigh", "max", "ultra"],
  );
  assert.deepEqual(
    codexReasoningEffortsForModel("gpt-6-luna").map((effort) => effort.value),
    ["low", "medium", "high", "xhigh", "max"],
  );
  assert.equal(normalizeCodexReasoningEffortForModel("gpt-6-luna", "ultra"), "max");
});

test("Claude Opus 5.5 is selectable alongside the rolling opus alias", () => {
  const models = modelOptionsForRuntime("claude");
  assert.ok(models.includes("claude-opus-5-5"));
  assert.ok(models.includes("claude-opus-5-5[1m]"));
  assert.ok(models.includes("opus"));
  assert.equal(modelLabel("claude-opus-5-5"), "Claude Opus 5.5");
  assert.equal(modelLabel("claude-opus-5-5[1m]"), "Claude Opus 5.5 (1M context)");
  assert.equal(modelLabel("opus"), "Claude Opus (latest)");
});

test("Claude Fable preset targets the current 5.1 model", () => {
  assert.ok(modelOptionsForRuntime("claude").includes("fable"));
  assert.equal(modelLabel("fable"), "Claude Fable 5.1");
  assert.equal(modelLabel("claude-fable-5-1"), "Claude Fable 5.1");
});
