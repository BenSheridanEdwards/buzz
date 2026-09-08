import type { HermesProfile } from "@/shared/api/tauriHermesProfiles";
import type { PersonaDropdownOption } from "./agentConfigOptions";
import { envVarsWithoutKeyCaseInsensitive } from "./providerEnvVarUpdates";

/** Preset harness id for Hermes Agent (`hermes-acp`). */
export const HERMES_RUNTIME_ID = "hermes";

/** Env var that selects the Hermes profile directory for a spawned agent. */
export const HERMES_HOME_ENV = "HERMES_HOME";

/**
 * buzz-acp sets this to `1` for Hermes engines so a bare Hermes skips its
 * profile-configured MCP servers at startup. A profile-backed agent wants
 * those servers (they are the profile's own tools), so the picker pins `0`.
 */
export const HERMES_SKIP_CONFIGURED_MCP_ENV = "HERMES_ACP_SKIP_CONFIGURED_MCP";

/** Every Hermes engine is a full process loading MCP servers: one at a time. */
export const HERMES_PROFILE_PARALLELISM = "1";

/** Agent instructions seeded when a profile is picked into an empty prompt. */
export const HERMES_PROFILE_DEFAULT_INSTRUCTIONS =
  "Your SOUL, memory, skills and tools come from your Hermes profile. Follow them.";

/** Dropdown value for "no profile selected". */
export const NO_HERMES_PROFILE_VALUE = "__no-hermes-profile__";

/** Lifecycle of the profile scan behind the picker. */
export type HermesProfilesStatus = "loading" | "ready" | "error";

/** The env vars a profile pick owns; everything else belongs to the user. */
const HERMES_PROFILE_ENV_KEYS = [
  HERMES_HOME_ENV,
  HERMES_SKIP_CONFIGURED_MCP_ENV,
] as const;

const HERMES_COMMAND_IDENTITIES = new Set([
  "hermes",
  "hermes-acp",
  "hermes-agent",
]);

/**
 * Basename identity of a harness command: path prefix, Windows launcher
 * suffixes, case and `_`/space separators are ignored so `C:\...\HERMES_ACP.CMD`
 * and `/opt/hermes/bin/hermes-acp` compare equal. Mirrors the Rust
 * `normalize_agent_command_identity` in `crates/buzz-acp/src/config.rs`, which
 * is the one that also strips `.cmd` and `.bat` (`normalize_command_identity`
 * in `discovery.rs` strips `.exe` alone).
 */
export function normalizeHarnessCommandIdentity(command: string): string {
  const basename = command.trim().replace(/\\/g, "/").split("/").pop() ?? "";
  const lower = basename.toLowerCase().replace(/[ _]/g, "-");
  return lower.replace(/\.(exe|cmd|bat)$/, "");
}

/** A harness as the forms hold it: a catalog id plus its resolved command. */
export type HarnessSelection = {
  runtimeId: string;
  command?: string | null;
};

/**
 * True when the selected harness is Hermes: the preset id, or any catalog
 * entry (custom harness included) whose command resolves to a Hermes binary.
 */
export function isHermesHarness(
  runtimeId: string,
  command?: string | null,
): boolean {
  if (runtimeId.trim().toLowerCase() === HERMES_RUNTIME_ID) return true;
  if (!command) return false;
  return HERMES_COMMAND_IDENTITIES.has(
    normalizeHarnessCommandIdentity(command),
  );
}

function isHermesSelection(harness: HarnessSelection): boolean {
  return isHermesHarness(harness.runtimeId, harness.command);
}

/**
 * Write `name` with its canonical spelling, dropping any other-case variant.
 *
 * Windows case-folds env names but POSIX does not: a hand-typed `hermes_home`
 * is a *different* variable to the `HERMES_HOME` the child process reads
 * (`merged_user_env` passes keys verbatim to `Command::env`). Reusing the
 * stored spelling would show the profile as pinned while Hermes ignored it, so
 * the canonical key always wins and the variant is removed rather than left
 * behind as a second row in the raw editor.
 */
function withEnvVar(
  envVars: Record<string, string>,
  name: string,
  value: string,
): Record<string, string> {
  return { ...envVarsWithoutKeyCaseInsensitive(envVars, name), [name]: value };
}

/** The profile directory currently pinned through `HERMES_HOME`, if any. */
export function selectedHermesProfilePath(
  envVars: Record<string, string>,
): string {
  const upper = HERMES_HOME_ENV.toUpperCase();
  const key = Object.keys(envVars).find((name) => name.toUpperCase() === upper);
  return key === undefined ? "" : (envVars[key]?.trim() ?? "");
}

/**
 * A path whose shape says it came from Windows, where the filesystem case-folds
 * and a hand-typed `c:\users\...` names the same directory the scanner reported
 * as `C:\Users\...`.
 */
function looksLikeWindowsPath(path: string): boolean {
  return /^[a-z]:[\\/]/i.test(path.trim()) || path.includes("\\");
}

/**
 * Path equality tolerant of separator style, a trailing separator, and — for
 * Windows paths only — case. POSIX paths stay case-sensitive because there
 * `/Users/me` and `/users/me` really can be two directories.
 */
export function isSameProfilePath(a: string, b: string): boolean {
  const caseFold = looksLikeWindowsPath(a) && looksLikeWindowsPath(b);
  const normalize = (path: string) => {
    const trimmed = path.trim().replace(/\\/g, "/").replace(/\/+$/, "");
    return caseFold ? trimmed.toLowerCase() : trimmed;
  };
  const left = normalize(a);
  return left.length > 0 && left === normalize(b);
}

/** Env vars with the profile pinned; every unrelated key is left untouched. */
export function hermesProfileEnvVars(
  envVars: Record<string, string>,
  profilePath: string,
): Record<string, string> {
  return withEnvVar(
    withEnvVar(envVars, HERMES_HOME_ENV, profilePath),
    HERMES_SKIP_CONFIGURED_MCP_ENV,
    "0",
  );
}

/** Env vars with the profile pin removed; every unrelated key is kept. */
export function envVarsWithoutHermesProfile(
  envVars: Record<string, string>,
): Record<string, string> {
  return HERMES_PROFILE_ENV_KEYS.reduce<Record<string, string>>(
    (current, key) => envVarsWithoutKeyCaseInsensitive(current, key),
    envVars,
  );
}

/** The form fields a profile pick can seed. */
export type HermesProfileDraft = {
  displayName: string;
  description: string;
  avatarUrl: string;
  systemPrompt: string;
  envVars: Record<string, string>;
  /** Raw parallelism text as the forms hold it. */
  parallelism: string;
};

function isSeedable(
  current: string,
  previousSeed: string | null | undefined,
): boolean {
  const trimmed = current.trim();
  return trimmed.length === 0 || trimmed === (previousSeed ?? "").trim();
}

/**
 * Apply a profile pick to a form draft.
 *
 * Identity fields are seeded only when they are empty or still hold what the
 * previously picked profile seeded, so a user's own edits survive switching
 * profiles while a pure profile-to-profile switch fully re-seeds. The env pin
 * and parallelism are always written: they are what makes the profile run.
 */
export function applyHermesProfileToDraft(
  draft: HermesProfileDraft,
  profile: HermesProfile,
  previousProfile: HermesProfile | null = null,
): HermesProfileDraft {
  const description = profile.description ?? "";
  const avatarUrl = profile.avatarDataUrl ?? "";
  return {
    displayName: isSeedable(draft.displayName, previousProfile?.name)
      ? profile.name
      : draft.displayName,
    description: isSeedable(draft.description, previousProfile?.description)
      ? description
      : draft.description,
    // A profile without an avatar clears one seeded by the previous profile;
    // the old profile's face must not carry over to the new identity.
    avatarUrl: isSeedable(draft.avatarUrl, previousProfile?.avatarDataUrl)
      ? avatarUrl
      : draft.avatarUrl,
    systemPrompt: isSeedable(
      draft.systemPrompt,
      HERMES_PROFILE_DEFAULT_INSTRUCTIONS,
    )
      ? HERMES_PROFILE_DEFAULT_INSTRUCTIONS
      : draft.systemPrompt,
    envVars: hermesProfileEnvVars(draft.envVars, profile.path),
    parallelism: HERMES_PROFILE_PARALLELISM,
  };
}

/**
 * Drop a profile from a draft: the pin goes, and so does everything the pick
 * seeded that the user never made their own.
 *
 * Identity (name, description, avatar) is deliberately kept — it is the agent's
 * now, not the profile's — but instructions that still read "your Hermes
 * profile" and a parallelism of 1 that only exists because a Hermes engine
 * needed it would otherwise outlive the profile that explained them.
 */
export function clearHermesProfileFromDraft(
  draft: HermesProfileDraft,
  defaults: { parallelism: string },
): HermesProfileDraft {
  return {
    ...draft,
    envVars: envVarsWithoutHermesProfile(draft.envVars),
    systemPrompt: isSeedable(
      draft.systemPrompt,
      HERMES_PROFILE_DEFAULT_INSTRUCTIONS,
    )
      ? ""
      : draft.systemPrompt,
    parallelism:
      draft.parallelism.trim() === HERMES_PROFILE_PARALLELISM
        ? defaults.parallelism
        : draft.parallelism,
  };
}

/**
 * The draft after the harness changed.
 *
 * Leaving Hermes drops the profile: `HERMES_HOME` means nothing to another
 * harness, and a pin the user cannot see is a pin they cannot remove. Every
 * route out counts — the preset dropdown, a custom harness whose command is not
 * a Hermes binary, a freshly added harness, a hand-typed custom command — so
 * this takes the two harness selections rather than a single dropdown event.
 */
export function hermesDraftOnHarnessChange(
  draft: HermesProfileDraft,
  previous: HarnessSelection,
  next: HarnessSelection,
  defaults: { parallelism: string },
): HermesProfileDraft {
  if (!isHermesSelection(previous) || isHermesSelection(next)) {
    return draft;
  }
  return clearHermesProfileFromDraft(draft, defaults);
}

/**
 * Resolve the picker's state from the env pin and the discovered profiles.
 *
 * A pinned path that matches no discovered profile (edited by hand, or a
 * profile removed from disk since the agent was created) is still offered as
 * a "Custom" option so the pin stays visible and clearable — but only once the
 * scan has finished, otherwise every pinned agent flashes "Custom: <path>"
 * before its profile name arrives.
 */
export function hermesProfilePickerState(
  profiles: readonly HermesProfile[],
  envVars: Record<string, string>,
  options: {
    status?: HermesProfilesStatus;
    /**
     * The pin a linked definition supplies underneath this form's env vars.
     * Clearing an instance override falls back to it rather than to nothing, so
     * the "none" option says so.
     */
    inheritedPath?: string;
  } = {},
): {
  options: PersonaDropdownOption[];
  selectedProfile: HermesProfile | null;
  selectedPath: string;
  value: string;
} {
  const status = options.status ?? "ready";
  const inheritedPath = options.inheritedPath ?? "";
  const selectedPath = selectedHermesProfilePath(envVars);
  const selectedProfile =
    profiles.find((profile) => isSameProfilePath(profile.path, selectedPath)) ??
    null;
  const inheritedProfile =
    inheritedPath.length > 0
      ? (profiles.find((profile) =>
          isSameProfilePath(profile.path, inheritedPath),
        ) ?? null)
      : null;
  const noneLabel =
    inheritedPath.length > 0
      ? `Definition default${inheritedProfile ? `: ${inheritedProfile.name}` : ""}`
      : "No profile";
  const dropdownOptions: PersonaDropdownOption[] = [
    { label: noneLabel, value: NO_HERMES_PROFILE_VALUE },
    ...profiles.map((profile) => ({
      label: profile.name,
      value: profile.path,
    })),
  ];
  const showCustom =
    status !== "loading" && selectedPath.length > 0 && selectedProfile === null;
  if (showCustom) {
    dropdownOptions.push({
      label: `Custom: ${selectedPath}`,
      value: selectedPath,
    });
  }
  return {
    options: dropdownOptions,
    selectedProfile,
    selectedPath,
    // While the scan is still running, an unmatched pin has no option to point
    // at; an empty value shows the "Loading profiles..." placeholder instead of
    // flashing "Custom: <path>" and then the profile's real name.
    value:
      selectedProfile?.path ??
      (showCustom
        ? selectedPath
        : selectedPath.length > 0 && status === "loading"
          ? ""
          : NO_HERMES_PROFILE_VALUE),
  };
}

/**
 * How an instance form must present a pin that its linked definition may own.
 *
 * Instance `envVars` are an override layer: spawn merges the definition's env
 * underneath them (`merged_user_env`), and instances are never seeded from the
 * definition. Reading the instance layer alone therefore reports "No profile"
 * for every agent created from a profile-backed definition, and writing to it
 * on every pick manufactures a silent override that the definition dialog then
 * contradicts. Both dialogs must agree, so the instance form displays the
 * *effective* env and only writes an override for a genuine change.
 */
export function hermesInstanceProfileState(
  instanceEnvVars: Record<string, string>,
  inheritedEnvVars: Record<string, string>,
): {
  /** What the spawned process will actually see. */
  effectiveEnvVars: Record<string, string>;
  /** True when the effective pin comes from the definition, not an override. */
  isInherited: boolean;
} {
  const inheritedPath = selectedHermesProfilePath(inheritedEnvVars);
  const instancePath = selectedHermesProfilePath(instanceEnvVars);
  return {
    effectiveEnvVars: { ...inheritedEnvVars, ...instanceEnvVars },
    isInherited: inheritedPath.length > 0 && instancePath.length === 0,
  };
}

/**
 * The instance override layer after a pick, given what the definition provides.
 *
 * Only the profile's own keys are touched, and only when the pick differs from
 * what is inherited: picking the definition's own profile back removes the
 * override instead of freezing a copy of it.
 */
export function hermesInstanceEnvVarsForPick(
  instanceEnvVars: Record<string, string>,
  inheritedEnvVars: Record<string, string>,
  nextEffectiveEnvVars: Record<string, string>,
): Record<string, string> {
  return HERMES_PROFILE_ENV_KEYS.reduce<Record<string, string>>(
    (current, key) => {
      const next = readEnvVar(nextEffectiveEnvVars, key);
      const inherited = readEnvVar(inheritedEnvVars, key);
      if (next === undefined || next === inherited) {
        return envVarsWithoutKeyCaseInsensitive(current, key);
      }
      return withEnvVar(current, key, next);
    },
    instanceEnvVars,
  );
}

function readEnvVar(
  envVars: Record<string, string>,
  name: string,
): string | undefined {
  const upper = name.toUpperCase();
  const key = Object.keys(envVars).find((each) => each.toUpperCase() === upper);
  return key === undefined ? undefined : envVars[key];
}
