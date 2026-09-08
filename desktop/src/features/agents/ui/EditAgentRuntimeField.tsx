import type { AcpRuntimeCatalogEntry } from "@/shared/api/types";
import type { HermesProfile } from "@/shared/api/tauriHermesProfiles";
import type { PersonaDropdownOption } from "./agentConfigOptions";
import { AddCustomHarnessDialog } from "./AddCustomHarnessDialog";
import {
  HermesProfileField,
  type HermesProfilesStatus,
} from "./HermesProfileField";
import { PersonaDropdownField } from "./PersonaDropdownField";

/**
 * Runtime (harness) picker for the instance edit dialog, with the inline
 * "Add custom harness" dialog and, for Hermes, the profile picker beneath it.
 */
export function EditAgentRuntimeField({
  disabled,
  envVars,
  hermes,
  isAddHarnessOpen,
  onAddHarnessOpenChange,
  onHarnessSaved,
  onValueChange,
  options,
  selectedRuntime,
  value,
}: {
  disabled: boolean;
  envVars: Record<string, string>;
  /** Present only while the prospective harness is Hermes. */
  hermes: {
    onProfileChange: (profile: HermesProfile | null) => void;
    profiles: readonly HermesProfile[];
    status: HermesProfilesStatus;
  } | null;
  isAddHarnessOpen: boolean;
  onAddHarnessOpenChange: (open: boolean) => void;
  onHarnessSaved: (id: string) => void;
  onValueChange: (value: string) => void;
  options: PersonaDropdownOption[];
  selectedRuntime: AcpRuntimeCatalogEntry | undefined;
  value: string;
}) {
  return (
    <>
      <div className="space-y-1.5">
        <label
          className="text-sm font-medium text-foreground"
          htmlFor="edit-agent-runtime"
        >
          Provider
        </label>
        <PersonaDropdownField
          disabled={disabled}
          id="edit-agent-runtime"
          onValueChange={onValueChange}
          options={options}
          placeholder="Choose a provider"
          value={value}
        />
        {selectedRuntime ? (
          <p className="text-xs text-muted-foreground">
            Detected at{" "}
            <span className="font-medium">
              {selectedRuntime.binaryPath ??
                selectedRuntime.command ??
                selectedRuntime.id}
            </span>
          </p>
        ) : null}
        <AddCustomHarnessDialog
          onOpenChange={onAddHarnessOpenChange}
          onSaved={onHarnessSaved}
          open={isAddHarnessOpen}
        />
      </div>
      {hermes ? (
        <HermesProfileField
          disabled={disabled}
          envVars={envVars}
          id="edit-agent-hermes-profile"
          onProfileChange={hermes.onProfileChange}
          profiles={hermes.profiles}
          status={hermes.status}
        />
      ) : null}
    </>
  );
}
