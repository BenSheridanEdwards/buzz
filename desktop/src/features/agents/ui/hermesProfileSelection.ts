import type { HermesProfile } from "@/shared/api/tauriHermesProfiles";
import type { PersonaDropdownOption } from "./agentConfigOptions";

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

const HERMES_COMMAND_IDENTITIES = new Set([
  "hermes",
  "hermes-acp",
  "hermes-agent",
]);

/**
 * Basename identity of a harness command: path prefix, Windows launcher
 * suffixes, case and `_`/space separators are ignored so `C:\...\HERMES_ACP.CMD`
 * and `/opt/hermes/bin/hermes-acp` compare equal. Mirrors the Rust
 * `normalize_command_identity`.
 */
export function normalizeHarnessCommandIdentity(command: string): string {
  const basename = command.trim().replace(/\\/g, "/").split("/").pop() ?? "";
  const lower = basename.toLowerCase().replace(/[ _]/g, "-");
  return lower.replace(/\.(exe|cmd|bat)$/, "");
}

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

/**
 * Find the stored spelling of an env key. Windows case-folds env names, so a
 * hand-typed `hermes_home` is the same variable as `HERMES_HOME`; reusing the
 * stored key keeps the raw editor from growing a duplicate row.
 */
function findEnvKey(
  envVars: Record<string, string>,
  name: string,
): string | undefined {
  const upper = name.toUpperCase();
  return Object.keys(envVars).find((key) => key.toUpperCase() === upper);
}

function withEnvVar(
  envVars: Record<string, string>,
  name: string,
  value: string,
): Record<string, string> {
  const key = findEnvKey(envVars, name) ?? name;
  return { ...envVars, [key]: value };
}

function withoutEnvVar(
  envVars: Record<string, string>,
  name: string,
): Record<string, string> {
  const upper = name.toUpperCase();
  return Object.fromEntries(
    Object.entries(envVars).filter(([key]) => key.toUpperCase() !== upper),
  );
}

/** The profile directory currently pinned through `HERMES_HOME`, if any. */
export function selectedHermesProfilePath(
  envVars: Record<string, string>,
): string {
  const key = findEnvKey(envVars, HERMES_HOME_ENV);
  return key === undefined ? "" : (envVars[key]?.trim() ?? "");
}

/** Path equality tolerant of separator style and a trailing separator. */
export function isSameProfilePath(a: string, b: string): boolean {
  const normalize = (path: string) =>
    path.trim().replace(/\\/g, "/").replace(/\/+$/, "");
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
  return withoutEnvVar(
    withoutEnvVar(envVars, HERMES_HOME_ENV),
    HERMES_SKIP_CONFIGURED_MCP_ENV,
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
 * Resolve the picker's state from the env pin and the discovered profiles.
 *
 * A pinned path that matches no discovered profile (edited by hand, or a
 * profile removed from disk since the agent was created) is still offered as
 * a "Custom" option so the pin stays visible and clearable.
 */
export function hermesProfilePickerState(
  profiles: readonly HermesProfile[],
  envVars: Record<string, string>,
): {
  options: PersonaDropdownOption[];
  selectedProfile: HermesProfile | null;
  selectedPath: string;
  value: string;
} {
  const selectedPath = selectedHermesProfilePath(envVars);
  const selectedProfile =
    profiles.find((profile) => isSameProfilePath(profile.path, selectedPath)) ??
    null;
  const options: PersonaDropdownOption[] = [
    { label: "No profile", value: NO_HERMES_PROFILE_VALUE },
    ...profiles.map((profile) => ({
      label: profile.name,
      value: profile.path,
    })),
  ];
  if (selectedPath.length > 0 && selectedProfile === null) {
    options.push({ label: `Custom: ${selectedPath}`, value: selectedPath });
  }
  return {
    options,
    selectedProfile,
    selectedPath,
    value: selectedProfile?.path ?? (selectedPath || NO_HERMES_PROFILE_VALUE),
  };
}
