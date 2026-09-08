/**
 * Behavioral tests for the Hermes profile picker model.
 *
 * The picker stores its selection in the form's env vars (HERMES_HOME), so
 * these tests pin the env round-trip, the harness detection that gates the
 * field, and the seeding rules a pick applies to the draft. Each case is
 * load-bearing: dropping the case-fold in findEnvKey, the "previous seed"
 * check in applyHermesProfileToDraft, or the Custom option in
 * hermesProfilePickerState fails a specific test below.
 */

import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { fromRawHermesProfile } from "@/shared/api/tauriHermesProfiles.ts";
import {
  applyHermesProfileToDraft,
  clearHermesProfileFromDraft,
  envVarsWithoutHermesProfile,
  HERMES_PROFILE_DEFAULT_INSTRUCTIONS,
  hermesDraftOnHarnessChange,
  hermesInstanceEnvVarsForPick,
  hermesInstanceProfileState,
  hermesProfileEnvVars,
  hermesProfilePickerState,
  isHermesHarness,
  isSameProfilePath,
  NO_HERMES_PROFILE_VALUE,
  normalizeHarnessCommandIdentity,
  selectedHermesProfilePath,
} from "./hermesProfileSelection.ts";

const bond = {
  slug: "bond",
  name: "Bond",
  description: "Executor of the Fleet",
  path: "/Users/me/.hermes/profiles/bond",
  avatarDataUrl: "data:image/jpeg;base64,/9j/4A==",
};

const sky = {
  slug: "sky",
  name: "Sky",
  description: null,
  path: "/Users/me/.hermes/profiles/sky",
  avatarDataUrl: null,
};

function emptyDraft(overrides = {}) {
  return {
    displayName: "",
    description: "",
    avatarUrl: "",
    systemPrompt: "",
    envVars: {},
    parallelism: "",
    ...overrides,
  };
}

describe("isHermesHarness", () => {
  it("matches the hermes preset id regardless of command", () => {
    assert.equal(isHermesHarness("hermes", null), true);
    assert.equal(isHermesHarness(" Hermes ", undefined), true);
  });

  it("matches a custom harness whose command is a Hermes binary", () => {
    assert.equal(isHermesHarness("custom", "hermes-acp"), true);
    assert.equal(isHermesHarness("custom", "/opt/hermes/bin/hermes-acp"), true);
    assert.equal(
      isHermesHarness("my-harness", "C:\\Tools\\Hermes\\HERMES_ACP.CMD"),
      true,
    );
  });

  it("leaves every other harness alone", () => {
    assert.equal(isHermesHarness("claude", "claude-agent-acp"), false);
    assert.equal(isHermesHarness("custom", "/usr/bin/goose"), false);
    assert.equal(isHermesHarness("", ""), false);
    assert.equal(isHermesHarness("custom", null), false);
    // A binary that merely mentions hermes is not the Hermes harness.
    assert.equal(isHermesHarness("custom", "hermes-acp-wrapper"), false);
  });

  it("normalizes command identities like the Rust side", () => {
    assert.equal(
      normalizeHarnessCommandIdentity("C:\\x\\Hermes ACP.exe"),
      "hermes-acp",
    );
    assert.equal(
      normalizeHarnessCommandIdentity("/a/b/hermes_acp"),
      "hermes-acp",
    );
    assert.equal(normalizeHarnessCommandIdentity("  goose  "), "goose");
  });
});

describe("env var round-trip", () => {
  it("pins the profile without touching unrelated variables", () => {
    const next = hermesProfileEnvVars(
      { OPENAI_API_KEY: "sk", HERMES_ACP_SKIP_CONFIGURED_MCP: "1" },
      bond.path,
    );
    assert.deepEqual(next, {
      OPENAI_API_KEY: "sk",
      HERMES_HOME: bond.path,
      HERMES_ACP_SKIP_CONFIGURED_MCP: "0",
    });
  });

  it("rewrites a hand-typed lowercase key to the canonical spelling", () => {
    // POSIX env is case-sensitive and `merged_user_env` passes keys verbatim to
    // Command::env, so a pin left under `hermes_home` is a pin the agent never
    // sees while the picker happily shows it as selected.
    const next = hermesProfileEnvVars({ hermes_home: "/old" }, bond.path);
    assert.deepEqual(next, {
      HERMES_HOME: bond.path,
      HERMES_ACP_SKIP_CONFIGURED_MCP: "0",
    });
    assert.equal(next.hermes_home, undefined);
    assert.equal(selectedHermesProfilePath(next), bond.path);
  });

  it("collapses every case variant of the MCP flag onto one key", () => {
    const next = hermesProfileEnvVars(
      { Hermes_Acp_Skip_Configured_Mcp: "1" },
      bond.path,
    );
    assert.deepEqual(next, {
      HERMES_HOME: bond.path,
      HERMES_ACP_SKIP_CONFIGURED_MCP: "0",
    });
  });

  it("clears only the profile pin", () => {
    const cleared = envVarsWithoutHermesProfile({
      OPENAI_API_KEY: "sk",
      HERMES_HOME: bond.path,
      hermes_acp_skip_configured_mcp: "0",
    });
    assert.deepEqual(cleared, { OPENAI_API_KEY: "sk" });
    assert.equal(selectedHermesProfilePath(cleared), "");
  });

  it("does not invent variables when nothing is pinned", () => {
    assert.deepEqual(envVarsWithoutHermesProfile({ A: "1" }), { A: "1" });
    assert.equal(selectedHermesProfilePath({}), "");
  });
});

describe("isSameProfilePath", () => {
  it("tolerates separator style and trailing separators", () => {
    assert.equal(
      isSameProfilePath(
        "C:\\Users\\me\\.hermes\\profiles\\bond",
        "C:/Users/me/.hermes/profiles/bond/",
      ),
      true,
    );
    assert.equal(isSameProfilePath(bond.path, `${bond.path}/`), true);
    assert.equal(isSameProfilePath(bond.path, sky.path), false);
  });

  it("never treats two empty pins as the same profile", () => {
    assert.equal(isSameProfilePath("", ""), false);
    assert.equal(isSameProfilePath("  ", "/"), false);
  });

  it("case-folds Windows paths and only Windows paths", () => {
    // Windows filesystems case-fold, so a hand-typed pin matches the scanner's
    // spelling and no spurious "Custom:" entry appears.
    assert.equal(
      isSameProfilePath(
        "c:\\users\\me\\.hermes\\profiles\\bond",
        "C:/Users/me/.hermes/profiles/bond",
      ),
      true,
    );
    // A UNC path is Windows too.
    assert.equal(
      isSameProfilePath(
        "\\\\server\\share\\Profiles\\Bond",
        "\\\\SERVER\\share\\profiles\\bond",
      ),
      true,
    );
    // POSIX paths do not: /Users/me and /users/me can be two directories.
    assert.equal(
      isSameProfilePath("/users/me/.hermes/profiles/bond", bond.path),
      false,
    );
    // A literal backslash in a POSIX directory name does not make it a Windows
    // path: case-folding it would be wrong, and rewriting the backslash to a
    // separator would make two different directories compare equal.
    assert.equal(isSameProfilePath("/p/a\\b", "/p/a/b"), false);
    assert.equal(isSameProfilePath("/p/a\\b", "/p/A\\b"), false);
  });
});

describe("applyHermesProfileToDraft", () => {
  it("seeds every empty field and writes the run settings", () => {
    const next = applyHermesProfileToDraft(emptyDraft(), bond);
    assert.deepEqual(next, {
      displayName: "Bond",
      description: "Executor of the Fleet",
      avatarUrl: bond.avatarDataUrl,
      systemPrompt: HERMES_PROFILE_DEFAULT_INSTRUCTIONS,
      envVars: {
        HERMES_HOME: bond.path,
        HERMES_ACP_SKIP_CONFIGURED_MCP: "0",
      },
      parallelism: "1",
    });
  });

  it("keeps the user's own name, description, avatar and instructions", () => {
    const draft = emptyDraft({
      displayName: "My Bot",
      description: "Mine",
      avatarUrl: "https://relay.example/avatar.png",
      systemPrompt: "Be terse.",
      envVars: { OPENAI_API_KEY: "sk" },
      parallelism: "4",
    });
    const next = applyHermesProfileToDraft(draft, bond);
    assert.equal(next.displayName, "My Bot");
    assert.equal(next.description, "Mine");
    assert.equal(next.avatarUrl, "https://relay.example/avatar.png");
    assert.equal(next.systemPrompt, "Be terse.");
    // The pin and parallelism are what make the profile run: always written.
    assert.equal(next.envVars.OPENAI_API_KEY, "sk");
    assert.equal(next.envVars.HERMES_HOME, bond.path);
    assert.equal(next.envVars.HERMES_ACP_SKIP_CONFIGURED_MCP, "0");
    assert.equal(next.parallelism, "1");
  });

  it("re-seeds fields that still hold the previous profile's values", () => {
    const seeded = applyHermesProfileToDraft(emptyDraft(), bond);
    const next = applyHermesProfileToDraft(seeded, sky, bond);
    assert.equal(next.displayName, "Sky");
    // Sky has no title or avatar: Bond's must not carry over.
    assert.equal(next.description, "");
    assert.equal(next.avatarUrl, "");
    assert.equal(next.systemPrompt, HERMES_PROFILE_DEFAULT_INSTRUCTIONS);
    assert.equal(next.envVars.HERMES_HOME, sky.path);
  });

  it("does not re-seed a field the user edited after the first pick", () => {
    const seeded = applyHermesProfileToDraft(emptyDraft(), bond);
    const edited = { ...seeded, displayName: "Bond (staging)" };
    const next = applyHermesProfileToDraft(edited, sky, bond);
    assert.equal(next.displayName, "Bond (staging)");
    assert.equal(next.description, "");
  });
});

describe("clearHermesProfileFromDraft", () => {
  it("drops the pin and everything the pick seeded but the user never owned", () => {
    const seeded = applyHermesProfileToDraft(emptyDraft(), bond);
    const cleared = clearHermesProfileFromDraft(seeded, { parallelism: "" });
    assert.deepEqual(cleared.envVars, {});
    // Instructions that still say "your Hermes profile" and a parallelism of 1
    // that only existed for a Hermes engine must not outlive the profile.
    assert.equal(cleared.systemPrompt, "");
    assert.equal(cleared.parallelism, "");
    // Identity is the agent's now, not the profile's.
    assert.equal(cleared.displayName, "Bond");
    assert.equal(cleared.description, "Executor of the Fleet");
    assert.equal(cleared.avatarUrl, bond.avatarDataUrl);
  });

  it("keeps instructions and parallelism the user chose", () => {
    const draft = emptyDraft({
      systemPrompt: "Be terse.",
      envVars: { HERMES_HOME: bond.path, OPENAI_API_KEY: "sk" },
      parallelism: "4",
    });
    const cleared = clearHermesProfileFromDraft(draft, { parallelism: "" });
    assert.deepEqual(cleared.envVars, { OPENAI_API_KEY: "sk" });
    assert.equal(cleared.systemPrompt, "Be terse.");
    assert.equal(cleared.parallelism, "4");
  });

  it("restores the form's own parallelism default", () => {
    const seeded = applyHermesProfileToDraft(emptyDraft(), bond);
    assert.equal(
      clearHermesProfileFromDraft(seeded, { parallelism: "3" }).parallelism,
      "3",
    );
  });
});

describe("hermesDraftOnHarnessChange", () => {
  const seeded = () => applyHermesProfileToDraft(emptyDraft(), bond);
  const hermesPreset = { runtimeId: "hermes", command: null };
  const hermesCustom = { runtimeId: "custom", command: "/opt/bin/hermes-acp" };
  const otherPreset = { runtimeId: "claude", command: "claude-agent-acp" };
  const otherCustom = { runtimeId: "custom", command: "/usr/bin/goose" };
  const addedHarness = { runtimeId: "my-harness", command: "/usr/bin/aider" };

  const cases = [
    ["preset to preset", hermesPreset, otherPreset, true],
    [
      "preset to a custom command that is not Hermes",
      hermesPreset,
      otherCustom,
      true,
    ],
    [
      "custom Hermes command to another preset",
      hermesCustom,
      otherPreset,
      true,
    ],
    [
      "custom Hermes command to a freshly added harness",
      hermesCustom,
      addedHarness,
      true,
    ],
    [
      "preset to a custom command that is still Hermes",
      hermesPreset,
      hermesCustom,
      false,
    ],
    [
      "custom Hermes command to the Hermes preset",
      hermesCustom,
      hermesPreset,
      false,
    ],
    ["a harness that was never Hermes", otherPreset, otherCustom, false],
  ];

  for (const [name, previous, next, drops] of cases) {
    it(`${drops ? "drops" : "keeps"} the pin: ${name}`, () => {
      const result = hermesDraftOnHarnessChange(seeded(), previous, next, {
        parallelism: "",
      });
      assert.equal(
        selectedHermesProfilePath(result.envVars),
        drops ? "" : bond.path,
      );
      assert.equal(
        result.systemPrompt,
        drops ? "" : HERMES_PROFILE_DEFAULT_INSTRUCTIONS,
      );
      assert.equal(result.parallelism, drops ? "" : "1");
    });
  }

  it("returns the draft untouched when nothing is dropped", () => {
    const draft = seeded();
    assert.equal(
      hermesDraftOnHarnessChange(draft, hermesPreset, hermesCustom, {
        parallelism: "",
      }),
      draft,
    );
  });
});

describe("hermes instance override layer", () => {
  const inherited = {
    HERMES_HOME: bond.path,
    HERMES_ACP_SKIP_CONFIGURED_MCP: "0",
    OPENAI_API_KEY: "definition-key",
  };

  it("reports a definition pin as inherited when nothing overrides it", () => {
    const state = hermesInstanceProfileState({ FOO: "1" }, inherited);
    assert.equal(state.isInherited, true);
    assert.equal(
      selectedHermesProfilePath(state.effectiveEnvVars),
      bond.path,
      "an instance created from a definition must not report No profile",
    );
    assert.equal(state.effectiveEnvVars.FOO, "1");
  });

  it("reports an instance pin as its own, not inherited", () => {
    const state = hermesInstanceProfileState(
      { HERMES_HOME: sky.path },
      inherited,
    );
    assert.equal(state.isInherited, false);
    assert.equal(selectedHermesProfilePath(state.effectiveEnvVars), sky.path);
  });

  it("writes no override when the pick is what the definition already gives", () => {
    const nextEffective = hermesProfileEnvVars({ ...inherited }, bond.path);
    assert.deepEqual(
      hermesInstanceEnvVarsForPick({ FOO: "1" }, inherited, nextEffective),
      { FOO: "1" },
    );
  });

  it("writes an override only for a genuine change", () => {
    const nextEffective = hermesProfileEnvVars({ ...inherited }, sky.path);
    assert.deepEqual(
      hermesInstanceEnvVarsForPick({ FOO: "1" }, inherited, nextEffective),
      { FOO: "1", HERMES_HOME: sky.path },
    );
  });

  it("removes an override that returns to the inherited profile", () => {
    const nextEffective = hermesProfileEnvVars({ ...inherited }, bond.path);
    assert.deepEqual(
      hermesInstanceEnvVarsForPick(
        { HERMES_HOME: sky.path, HERMES_ACP_SKIP_CONFIGURED_MCP: "1" },
        inherited,
        nextEffective,
      ),
      {},
    );
  });

  it("keeps the whole pin on an instance with no definition underneath", () => {
    const nextEffective = hermesProfileEnvVars({}, bond.path);
    assert.deepEqual(hermesInstanceEnvVarsForPick({}, {}, nextEffective), {
      HERMES_HOME: bond.path,
      HERMES_ACP_SKIP_CONFIGURED_MCP: "0",
    });
  });

  it("never copies unrelated definition env vars into the override layer", () => {
    const nextEffective = hermesProfileEnvVars({ ...inherited }, sky.path);
    const override = hermesInstanceEnvVarsForPick({}, inherited, nextEffective);
    assert.equal(override.OPENAI_API_KEY, undefined);
  });
});

describe("hermesProfilePickerState", () => {
  it("offers No profile plus every discovered profile", () => {
    const state = hermesProfilePickerState([bond, sky], {});
    assert.deepEqual(
      state.options.map((option) => option.value),
      [NO_HERMES_PROFILE_VALUE, bond.path, sky.path],
    );
    assert.equal(state.value, NO_HERMES_PROFILE_VALUE);
    assert.equal(state.selectedProfile, null);
    assert.equal(state.selectedPath, "");
  });

  it("re-selects the pinned profile from HERMES_HOME on edit", () => {
    const state = hermesProfilePickerState([bond, sky], {
      HERMES_HOME: `${sky.path}/`,
    });
    assert.equal(state.selectedProfile, sky);
    assert.equal(state.value, sky.path);
    assert.equal(state.selectedPath, `${sky.path}/`);
  });

  it("keeps a pin that matches no discovered profile visible and clearable", () => {
    const state = hermesProfilePickerState([bond], {
      HERMES_HOME: "/gone/profile",
    });
    assert.equal(state.selectedProfile, null);
    assert.equal(state.value, "/gone/profile");
    assert.deepEqual(state.options.at(-1), {
      label: "Custom: /gone/profile",
      value: "/gone/profile",
    });
  });

  it("does not flash a Custom entry while the scan is still running", () => {
    const loading = hermesProfilePickerState(
      [],
      { HERMES_HOME: bond.path },
      {
        status: "loading",
      },
    );
    assert.ok(
      !loading.options.some((option) => option.label.startsWith("Custom:")),
    );
    // No option to point at yet: the placeholder shows instead of claiming the
    // agent has no profile.
    assert.equal(loading.value, "");

    const ready = hermesProfilePickerState(
      [bond],
      { HERMES_HOME: bond.path },
      {
        status: "ready",
      },
    );
    assert.equal(ready.value, bond.path);
  });

  it("still offers the Custom entry when the scan failed", () => {
    const state = hermesProfilePickerState(
      [],
      { HERMES_HOME: "/gone" },
      {
        status: "error",
      },
    );
    assert.equal(state.value, "/gone");
  });

  it("names the definition's profile on the fall-back option", () => {
    const state = hermesProfilePickerState(
      [bond, sky],
      { HERMES_HOME: sky.path },
      { inheritedPath: bond.path },
    );
    assert.deepEqual(state.options[0], {
      label: "Definition default: Bond",
      value: NO_HERMES_PROFILE_VALUE,
    });
  });
});

describe("fromRawHermesProfile", () => {
  it("maps the snake_case IPC shape and fills absent optionals with null", () => {
    assert.deepEqual(
      fromRawHermesProfile({ slug: "s", name: "N", path: "/p" }),
      {
        slug: "s",
        name: "N",
        description: null,
        path: "/p",
        avatarDataUrl: null,
      },
    );
    assert.deepEqual(
      fromRawHermesProfile({
        slug: "s",
        name: "N",
        description: "D",
        path: "/p",
        avatar_data_url: "data:image/png;base64,iVBORw==",
      }),
      {
        slug: "s",
        name: "N",
        description: "D",
        path: "/p",
        avatarDataUrl: "data:image/png;base64,iVBORw==",
      },
    );
  });
});
