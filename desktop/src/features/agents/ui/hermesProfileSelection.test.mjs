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
  envVarsWithoutHermesProfile,
  HERMES_PROFILE_DEFAULT_INSTRUCTIONS,
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

  it("reuses a hand-typed lowercase key instead of adding a duplicate row", () => {
    const next = hermesProfileEnvVars({ hermes_home: "/old" }, bond.path);
    assert.deepEqual(next, {
      hermes_home: bond.path,
      HERMES_ACP_SKIP_CONFIGURED_MCP: "0",
    });
    assert.equal(selectedHermesProfilePath(next), bond.path);
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
