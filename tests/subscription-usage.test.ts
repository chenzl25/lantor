import assert from "node:assert/strict";
import test from "node:test";

import { creditsSummary, formatSpend } from "../src/components/AgentSubscriptionUsage";

test("spend shows dollars and cents, and tiny amounts as under a cent", () => {
  assert.equal(formatSpend(0), "$0.00");
  assert.equal(formatSpend(4_000), "<$0.01");
  assert.equal(formatSpend(113_160_000), "$113.16");
  assert.equal(formatSpend(1_234_567_890), "$1,234.57");
});

test("an account without windows names its credits or per-use billing", () => {
  assert.deepEqual(
    creditsSummary({ status: "available", credits: { unlimited: true, balance: null } }),
    { label: "Credits", value: "Unlimited", note: "No usage windows" },
  );
  assert.deepEqual(
    creditsSummary({ status: "limited", credits: { unlimited: false, balance: "62500.0000000000" } }),
    { label: "Credits", value: "62,500 left", note: "Spend limit reached" },
  );
  assert.deepEqual(
    creditsSummary({ status: "available", credits: null }),
    { label: "Billing", value: "Pay as you go", note: "No usage windows" },
  );
});
