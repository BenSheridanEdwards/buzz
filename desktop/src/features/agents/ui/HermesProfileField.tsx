import type { HermesProfile } from "@/shared/api/tauriHermesProfiles";
import { useHermesProfilesQuery } from "../useHermesProfiles";
import { PERSONA_LABEL_OPTIONAL_CLASS } from "./agentConfigOptions";
import { PersonaDropdownField } from "./PersonaDropdownField";
import {
  applyHermesProfileToDraft,
  envVarsWithoutHermesProfile,
  type HermesProfileDraft,
  hermesProfilePickerState,
  NO_HERMES_PROFILE_VALUE,
} from "./hermesProfileSelection";

export type HermesProfilesStatus = "loading" | "ready" | "error";

/**
 * Profile list plus the pick handler shared by the definition and instance
 * forms. `draft` is the form's current values; `onApply` receives the whole
 * seeded draft so one pick lands as one state update per field.
 */
export function useHermesProfilePicker({
  draft,
  enabled,
  onApply,
}: {
  draft: HermesProfileDraft;
  enabled: boolean;
  onApply: (next: HermesProfileDraft) => void;
}) {
  const query = useHermesProfilesQuery({ enabled });
  const profiles = query.data ?? [];
  const status: HermesProfilesStatus = query.isError
    ? "error"
    : query.data === undefined
      ? "loading"
      : "ready";

  function handleProfileChange(profile: HermesProfile | null) {
    if (profile === null) {
      onApply({
        ...draft,
        envVars: envVarsWithoutHermesProfile(draft.envVars),
      });
      return;
    }
    const previous = hermesProfilePickerState(
      profiles,
      draft.envVars,
    ).selectedProfile;
    onApply(applyHermesProfileToDraft(draft, profile, previous));
  }

  return { handleProfileChange, profiles, status };
}

/**
 * "Hermes profile" picker shown when the selected harness is Hermes Agent.
 *
 * The selection lives in the form's env vars (`HERMES_HOME`), so the field is
 * stateless: it derives its value from `envVars` and reports a pick as the
 * chosen profile (or `null` for "No profile"). The pinned directory is shown
 * read-only underneath so the user can see exactly what will run.
 */
export function HermesProfileField({
  disabled,
  envVars,
  id = "persona-hermes-profile",
  onProfileChange,
  profiles,
  status,
}: {
  disabled: boolean;
  envVars: Record<string, string>;
  id?: string;
  onProfileChange: (profile: HermesProfile | null) => void;
  profiles: readonly HermesProfile[];
  status: HermesProfilesStatus;
}) {
  const { options, selectedPath, value } = hermesProfilePickerState(
    profiles,
    envVars,
  );
  return (
    <div className="space-y-1.5">
      <label className="text-sm font-medium text-foreground" htmlFor={id}>
        Hermes profile
        <span className={PERSONA_LABEL_OPTIONAL_CLASS}>Optional</span>
      </label>
      <PersonaDropdownField
        disabled={disabled || status === "loading"}
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
        <p className="text-xs text-warning">
          Could not read Hermes profiles. You can still set HERMES_HOME under
          Advanced.
        </p>
      ) : null}
      {status === "ready" && profiles.length === 0 ? (
        <p className="text-xs text-muted-foreground">
          No profiles found in ~/.hermes/profiles.
        </p>
      ) : null}
      {selectedPath.length > 0 ? (
        <p
          className="break-all text-xs text-muted-foreground"
          data-testid="hermes-profile-path"
        >
          Profile directory:{" "}
          <span className="font-medium text-foreground">{selectedPath}</span>
        </p>
      ) : null}
      <p className="text-xs text-muted-foreground">
        Picking a profile fills the name, description and avatar from its SOUL,
        points HERMES_HOME at the profile, keeps its MCP servers on and sets
        parallelism to 1.
      </p>
    </div>
  );
}
