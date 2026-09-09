import assert from "node:assert/strict";
import test from "node:test";

import {
  AGENT_PARALLELISM_HELP,
  AGENT_PARALLELISM_PLACEHOLDER,
  DEFAULT_AGENT_PARALLELISM,
  agentParallelismHelp,
  agentParallelismPlaceholder,
  harnessParallelismDefault,
  resolveAgentParallelism,
  parallelismCapHint,
} from "./agentParallelism.ts";

// ── Blank-field copy: harness default vs app default ─────────────────────────

test("blank-field copy falls back to the app default with no runtime selected", () => {
  assert.equal(harnessParallelismDefault(undefined), null);
  assert.equal(
    agentParallelismPlaceholder(undefined),
    AGENT_PARALLELISM_PLACEHOLDER,
  );
  assert.equal(agentParallelismHelp(undefined), AGENT_PARALLELISM_HELP);
});

test("blank-field copy stays on the app default when the harness matches it", () => {
  const goose = {
    label: "Goose",
    defaultParallelism: DEFAULT_AGENT_PARALLELISM,
  };
  assert.equal(harnessParallelismDefault(goose), null);
  assert.equal(
    agentParallelismPlaceholder(goose),
    AGENT_PARALLELISM_PLACEHOLDER,
  );
  assert.equal(agentParallelismHelp(goose), AGENT_PARALLELISM_HELP);
});

test("blank-field copy names the harness whose default differs from the app default", () => {
  const hermes = { label: "Hermes", defaultParallelism: 1 };
  assert.deepEqual(harnessParallelismDefault(hermes), {
    label: "Hermes",
    value: 1,
  });
  const placeholder = agentParallelismPlaceholder(hermes);
  assert.equal(placeholder, "Hermes default (1)");
  assert.notEqual(placeholder, AGENT_PARALLELISM_PLACEHOLDER);
  const help = agentParallelismHelp(hermes);
  assert.ok(help.includes("Hermes"), "help must name the harness");
  assert.ok(
    help.includes("(currently 1)"),
    "help must carry the harness default",
  );
  assert.ok(
    !help.includes(`(currently ${DEFAULT_AGENT_PARALLELISM})`),
    "help must not still advertise the app default",
  );
  assert.ok(help.includes("1–32"), "range guidance is unchanged");
});

test("parallelism uses the app default only when input and definition omit it", () => {
  assert.equal(
    resolveAgentParallelism(undefined, undefined),
    DEFAULT_AGENT_PARALLELISM,
  );
  assert.equal(
    resolveAgentParallelism(undefined, null),
    DEFAULT_AGENT_PARALLELISM,
  );
  assert.equal(resolveAgentParallelism(undefined, 4), 4);
  assert.equal(resolveAgentParallelism(2, 4), 2);
});

// ── parallelismCapHint: persona/instance hint data path ───────────────────────

test("parallelismCapHint returns null when requested is at or below the cap", () => {
  assert.equal(parallelismCapHint("OpenClaw", 5, 5), null);
  assert.equal(parallelismCapHint("OpenClaw", 5, 3), null);
  assert.equal(parallelismCapHint("OpenClaw", 5, 1), null);
});

test("parallelismCapHint returns hint string when requested exceeds cap", () => {
  const hint = parallelismCapHint("OpenClaw", 5, 10);
  assert.ok(hint !== null, "hint must be non-null when 10 > 5");
  assert.ok(hint.includes("OpenClaw"), "hint must include the harness label");
  assert.ok(hint.includes("5"), "hint must include the cap value");
});

test("parallelismCapHint uses singular form for cap of 1", () => {
  const hint = parallelismCapHint("SomeHarness", 1, 5);
  assert.ok(hint !== null);
  assert.ok(
    hint.includes("conversation") && !hint.includes("conversations"),
    "cap=1 must use singular 'conversation'",
  );
});

// Stored-10 OpenClaw → Goose: hint clears when the harness has no cap.
// The UI derives: if selectedRuntime.maxParallelism is undefined, hint is null.
// This test validates the helper contract that makes that work.
test("parallelismCapHint returns null when cap equals or exceeds any common parallelism value", () => {
  // Simulates an uncapped harness: the caller passes a very large cap
  // OR simply doesn't call parallelismCapHint at all (guarded by maxParallelism check).
  // When stored=10 and harness switches to goose (no cap), no hint is shown.
  assert.equal(parallelismCapHint("Goose", 32, 10), null);
  assert.equal(parallelismCapHint("Goose", 32, 32), null);
});
