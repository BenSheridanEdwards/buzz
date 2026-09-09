import type { EditAgentHermesFieldProps } from "./EditAgentRuntimeField";
import type { EnvVarsValue } from "./EnvVarsEditor";
import { useHermesProfilePicker } from "./HermesProfileField";
import { isHermesHarness } from "./hermesProfileSelection";
import { useHermesHarnessWatch } from "./useHermesHarnessWatch";

/**
 * Hermes profile wiring for the agent instance edit dialog.
 *
 * Owns the three pieces the instance form needs once its prospective harness
 * is Hermes: the profile picker (over the *effective* environment, definition
 * layer plus instance overrides), the watch that drops the pin on every route
 * out of Hermes, and the settling gate that keeps the watch from firing on a
 * selection the user never made. The dialog resolves the prospective harness
 * and owns the form state; everything Hermes-shaped lives here.
 *
 * `fieldProps` is `null` unless the prospective harness is Hermes, which is
 * exactly when the field is shown.
 */
export function useAgentInstanceHermesProfile({
  command,
  defaultParallelism,
  draft,
  inheritedEnvVars,
  onEditDefinition,
  open,
  ownsSystemPrompt,
  runtimeId,
  setters,
  settled,
}: {
  /** Command of the prospective harness, as the dialog resolved it. */
  command: string;
  /** Parallelism to restore when the profile that pinned 1 goes away. */
  defaultParallelism: string;
  draft: {
    envVars: EnvVarsValue;
    name: string;
    parallelism: string;
    systemPrompt: string;
  };
  /** The linked definition's env layer, beneath this instance's overrides. */
  inheritedEnvVars: Record<string, string>;
  /** Route into definition-edit, present only when that definition is editable. */
  onEditDefinition?: () => void;
  /** The dialog is open. A closed dialog has no selection to watch. */
  open: boolean;
  /** No linked definition owns the prompt, so the instance may write it. */
  ownsSystemPrompt: boolean;
  /** Prospective runtime id — the harness that is active after submit. */
  runtimeId: string;
  setters: {
    setEnvVars: (next: EnvVarsValue) => void;
    setName: (next: string) => void;
    setParallelism: (next: string) => void;
    setSystemPrompt: (next: string) => void;
  };
  /** Every query the harness selection is derived from has stopped loading. */
  settled: boolean;
}): {
  effectiveEnvVars: Record<string, string>;
  fieldProps: EditAgentHermesFieldProps | null;
} {
  const { envVars, name, parallelism, systemPrompt } = draft;
  const { setEnvVars, setName, setParallelism, setSystemPrompt } = setters;
  const harness = { runtimeId, command };
  const isHermesSelected = isHermesHarness(harness.runtimeId, harness.command);
  const picker = useHermesProfilePicker({
    defaultParallelism,
    // Avatar and description are definition-level identity here; the
    // instance prompt is only editable when no definition owns it.
    draft: {
      displayName: name,
      description: "",
      avatarUrl: "",
      systemPrompt: ownsSystemPrompt ? systemPrompt : "",
      envVars,
      parallelism,
    },
    enabled: open && isHermesSelected,
    // Instance env vars are an override layer over the definition's; the
    // picker must show what the process will actually see, not just the
    // overrides, or every agent created from a profile-backed definition
    // reports "No profile".
    inheritedEnvVars: inheritedEnvVars,
    onApply: (next) => {
      setName(next.displayName);
      if (ownsSystemPrompt) setSystemPrompt(next.systemPrompt);
      setEnvVars(next.envVars);
      setParallelism(next.parallelism);
    },
  });

  // Every route out of Hermes drops the pin, not just the harness dropdown: a
  // hand-typed custom command that stops being a Hermes binary hides the field
  // while `HERMES_HOME` would otherwise stay in the env. Watching the harness
  // itself keeps one owner for all of them, and reuses the same transform the
  // definition dialog applies.
  //
  // `harness` is derived from the runtime catalog and the persona list, so it
  // moves on its own while those queries load. The watch only compares
  // selections once both have stopped loading; see `useHermesHarnessWatch`.
  useHermesHarnessWatch({
    defaults: { parallelism: defaultParallelism },
    draft: {
      displayName: name,
      description: "",
      avatarUrl: "",
      systemPrompt,
      envVars,
      parallelism,
    },
    enabled: open,
    harness,
    onChange: (next) => {
      setEnvVars(next.envVars);
      setParallelism(next.parallelism);
      if (ownsSystemPrompt) {
        setSystemPrompt(next.systemPrompt);
      }
    },
    settled,
  });

  return {
    effectiveEnvVars: picker.effectiveEnvVars,
    fieldProps: isHermesSelected
      ? {
          inherited: {
            isInherited: picker.isInherited,
            onEditDefinition,
            path: picker.inheritedPath,
          },
          onProfileChange: picker.handleProfileChange,
          profiles: picker.profiles,
          status: picker.status,
        }
      : null,
  };
}
