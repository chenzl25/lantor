import assert from "node:assert/strict";
import test from "node:test";

import {
  EMPTY_AGENT_FORM,
  RUNTIME_PRESETS,
  codexReasoningEffortsForModel,
  isRetiredCodexModel,
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

test("GPT-6 Sol, Terra and Luna presets follow the Codex app-server effort ranges", () => {
  assert.equal(RUNTIME_PRESETS.codex.defaultModel, "gpt-6-sol");
  assert.equal(EMPTY_AGENT_FORM.model, "gpt-6-sol");
  assert.ok(modelOptionsForRuntime("codex").includes("gpt-6.1-sol"));
  assert.ok(modelOptionsForRuntime("codex").includes("gpt-6.1-terra"));
  assert.ok(modelOptionsForRuntime("codex").includes("gpt-6-sol"));
  assert.ok(modelOptionsForRuntime("codex").includes("gpt-6-luna"));
  assert.equal(modelLabel("gpt-6.1-sol"), "GPT-6.1 Sol");
  assert.equal(modelLabel("gpt-6.1-terra"), "GPT-6.1 Terra");
  assert.equal(modelLabel("gpt-6-sol"), "GPT-6 Sol");
  assert.equal(modelLabel("gpt-6-luna"), "GPT-6 Luna");
  assert.deepEqual(
    codexReasoningEffortsForModel("gpt-6.1-sol").map((effort) => effort.value),
    ["low", "medium", "high", "xhigh", "max", "ultra"],
  );
  assert.equal(normalizeCodexReasoningEffortForModel("gpt-6.1-sol", "ultra"), "ultra");
  assert.deepEqual(
    codexReasoningEffortsForModel("gpt-6.1-terra").map((effort) => effort.value),
    ["low", "medium", "high", "xhigh", "max", "ultra"],
  );
  assert.equal(normalizeCodexReasoningEffortForModel("gpt-6.1-terra", "ultra"), "ultra");
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

test("Codex offers only GPT-6 presets and marks older saved models as retired", () => {
  const models = modelOptionsForRuntime("codex");
  assert.deepEqual(models, ["gpt-6-astra", "gpt-6.1-sol", "gpt-6.1-terra", "gpt-6-sol", "gpt-6-luna"]);
  for (const model of ["gpt-4o", "gpt-5.3-codex", "gpt-5.4-mini", "gpt-5.5", " GPT-5.6-SOL ", "gpt-5.6-terra", "gpt-5.6-luna"]) {
    assert.equal(isRetiredCodexModel(model), true, `${model} must be retired`);
    assert.ok(!models.includes(model));
  }
  for (const model of [...models, "gpt-7", "custom-model"]) {
    assert.equal(isRetiredCodexModel(model), false, `${model} must not be retired`);
  }
  assert.deepEqual(modelOptionsForRuntime("codex", "gpt-5.5"), ["gpt-5.5", ...models]);
});

test("Claude picker exposes only rolling aliases; opus tracks the latest Opus release", () => {
  const models = modelOptionsForRuntime("claude");
  assert.deepEqual(models, ["fable", "opus", "sonnet", "haiku"]);
  assert.ok(!models.some((model) => model.startsWith("claude-opus-")));
  assert.equal(modelLabel("opus"), "Claude Opus (latest)");
});

test("Claude Fable preset tracks the latest Fable release", () => {
  const models = modelOptionsForRuntime("claude");
  assert.ok(models.includes("fable"));
  assert.ok(!models.some((model) => model.startsWith("claude-fable-")));
  assert.equal(modelLabel("fable"), "Claude Fable (latest)");
  assert.equal(modelLabel("claude-fable-5-2"), "Claude Fable 5.2");
  assert.equal(modelLabel("claude-fable-5-1"), "Claude Fable 5.1");
});
