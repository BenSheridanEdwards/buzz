import type { HermesProfile } from "@/shared/api/tauriHermesProfiles";
import { useHermesProfilesQuery } from "../useHermesProfiles";
import { PERSONA_LABEL_OPTIONAL_CLASS } from "./agentConfigOptions";
import { PersonaDropdownField } from "./PersonaDropdownField";
import {
  applyHermesProfileToDraft,
  clearHermesProfileFromDraft,
  type HermesProfileDraft,
  hermesInstanceEnvVarsForPick,
  hermesInstanceProfileState,
  hermesProfilePickerState,
  type HermesProfilesStatus,
  NO_HERMES_PROFILE_VALUE,
  selectedHermesProfilePath,
} from "./hermesProfileSelection";

export type { HermesProfilesStatus };

/**
 * Profile list plus the pick handler shared by the definition and instance
 * forms. `draft` is the form's current values, with `draft.envVars` holding the
 * layer this form owns; `inheritedEnvVars` is the definition layer beneath it
 * on an instance form. `onApply` receives the whole seeded draft — its
 * `envVars` are already back in the form's own layer — so one pick lands as one
 * state update per field.
 */
export function useHermesProfilePicker({
  defaultParallelism,
  draft,
  enabled,
  inheritedEnvVars,
  onApply,
}: {
  /** Parallelism to restore when the profile that pinned 1 is removed. */
  defaultParallelism: string;
  draft: HermesProfileDraft;
  enabled: boolean;
  inheritedEnvVars?: Record<string, string>;
  onApply: (next: HermesProfileDraft) => void;
}) {
  const query = useHermesProfilesQuery({ enabled });
  const profiles = query.data ?? [];
  const status: HermesProfilesStatus = query.isError
    ? "error"
    : query.data === undefined
      ? "loading"
      : "ready";

  const inherited = inheritedEnvVars ?? {};
  const { effectiveEnvVars, isInherited } = hermesInstanceProfileState(
    draft.envVars,
    inherited,
  );
  const inheritedPath = selectedHermesProfilePath(inherited);

  /** Put a computed effective env back into the layer this form writes. */
  function toFormLayer(nextEffective: Record<string, string>) {
    return inheritedEnvVars === undefined
      ? nextEffective
      : hermesInstanceEnvVarsForPick(draft.envVars, inherited, nextEffective);
  }

  function handleProfileChange(profile: HermesProfile | null) {
    const effectiveDraft = { ...draft, envVars: effectiveEnvVars };
    if (profile === null) {
      if (inheritedPath.length > 0) {
        // Not "no profile": dropping the override falls back to the
        // definition's profile, which still explains the seeded instructions
        // and the parallelism of 1, so neither is cleared.
        onApply({
          ...draft,
          envVars: hermesInstanceEnvVarsForPick(draft.envVars, inherited, {}),
        });
        return;
      }
      const cleared = clearHermesProfileFromDraft(effectiveDraft, {
        parallelism: defaultParallelism,
      });
      onApply({ ...cleared, envVars: toFormLayer(cleared.envVars) });
      return;
    }
    const previous = hermesProfilePickerState(
      profiles,
      effectiveEnvVars,
    ).selectedProfile;
    const next = applyHermesProfileToDraft(effectiveDraft, profile, previous);
    onApply({ ...next, envVars: toFormLayer(next.envVars) });
  }

  return {
    effectiveEnvVars,
    handleProfileChange,
    inheritedPath,
    isInherited,
    profiles,
    status,
  };
}

/**
 * "Hermes profile" picker shown when the selected harness is Hermes Agent.
 *
 * The selection lives in the form's env vars (`HERMES_HOME`), so the field is
 * stateless: it derives its value from `envVars` and reports a pick as the
 * chosen profile (or `null` for "no profile of this form's own"). The pinned
 * directory is shown read-only underneath so the user can see exactly what will
 * run.
 *
 * On an instance form `envVars` is the *effective* env — the definition's
 * layer plus the instance's overrides. A pin that comes from the definition
 * alone is read-only here: an instance override cannot unset an inherited
 * variable, so the honest recovery is to edit the definition, and the field
 * says so rather than offering a control that would do nothing.
 */
export function HermesProfileField({
  disabled,
  envVars,
  id = "persona-hermes-profile",
  inherited,
  onProfileChange,
  profiles,
  status,
}: {
  disabled: boolean;
  envVars: Record<string, string>;
  id?: string;
  /** Present on an instance form whose linked definition owns the pin. */
  inherited?: {
    /** The definition's pin, "" when it has none. */
    path: string;
    /** True while that pin is in effect with no instance override. */
    isInherited: boolean;
    onEditDefinition?: () => void;
  } | null;
  onProfileChange: (profile: HermesProfile | null) => void;
  profiles: readonly HermesProfile[];
  status: HermesProfilesStatus;
}) {
  const isInherited = inherited?.isInherited === true;
  const { options, selectedPath, value } = hermesProfilePickerState(
    profiles,
    envVars,
    { inheritedPath: inherited?.path ?? "", status },
  );
  const helpId = `${id}-help`;
  const errorId = `${id}-error`;
  const pathId = `${id}-path`;
  const inheritedId = `${id}-inherited`;
  const describedBy = [
    status === "error" ? errorId : null,
    isInherited ? inheritedId : null,
    selectedPath.length > 0 ? pathId : null,
    helpId,
  ]
    .filter((each) => each !== null)
    .join(" ");
  return (
    <div className="space-y-1.5">
      <label className="text-sm font-medium text-foreground" htmlFor={id}>
        Hermes profile
        <span className={PERSONA_LABEL_OPTIONAL_CLASS}>Optional</span>
      </label>
      <PersonaDropdownField
        describedBy={describedBy}
        disabled={disabled || isInherited || status === "loading"}
        id={id}
        onValueChange={(nextValue) => {
          if (nextValue === NO_HERMES_PROFILE_VALUE) {
            onProfileChange(null);
            return;
          }
          onProfileChange(
            profiles.find((profile) => profile.path === nextValue) ?? null,
          );
        }}
        options={options}
        placeholder={
          status === "loading" ? "Loading profiles..." : "Choose a profile"
        }
        value={value}
      />
      {status === "error" ? (
        <p className="text-xs text-warning" id={errorId} role="alert">
          Could not read Hermes profiles. You can still set HERMES_HOME under
          Advanced.
        </p>
      ) : null}
      {status === "ready" && profiles.length === 0 ? (
        <p className="text-xs text-muted-foreground" id={`${id}-empty`}>
          No profiles found in ~/.hermes/profiles.
        </p>
      ) : null}
      {isInherited ? (
        <p className="text-xs text-muted-foreground" id={inheritedId}>
          Set by this agent's definition.{" "}
          {inherited?.onEditDefinition ? (
            <button
              className="underline underline-offset-2 hover:text-foreground"
              onClick={inherited.onEditDefinition}
              type="button"
            >
              Edit definition
            </button>
          ) : (
            "Change it there to use a different profile."
          )}
        </p>
      ) : null}
      {selectedPath.length > 0 ? (
        <p
          className="break-all text-xs text-muted-foreground"
          data-testid="hermes-profile-path"
          id={pathId}
        >
          Profile directory:{" "}
          <span className="font-medium text-foreground">{selectedPath}</span>
        </p>
      ) : null}
      <p className="text-xs text-muted-foreground" id={helpId}>
        Picking a profile fills the name, description and avatar from its SOUL,
        points HERMES_HOME at the profile, keeps its MCP servers on and sets
        parallelism to 1.
      </p>
    </div>
  );
}
