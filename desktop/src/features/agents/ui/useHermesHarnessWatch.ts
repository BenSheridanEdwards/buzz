import * as React from "react";

import {
  type HarnessSelection,
  hermesDraftOnHarnessChange,
  type HermesProfileDraft,
} from "./hermesProfileSelection";

/**
 * Drop the Hermes profile when the form's harness stops being Hermes.
 *
 * Every route out of Hermes counts, not just the harness dropdown: a custom
 * command typed by hand can stop being a Hermes binary without any dropdown
 * event, so this watches the resolved harness rather than a single handler.
 *
 * That makes the trigger a *derived* value, and on the instance form it is
 * derived from two asynchronously settling queries (the runtime catalog and
 * the persona list) rather than from anything the user did. Until both have
 * settled, the same agent reads as cold Hermes, then as the app's default
 * runtime, then as Hermes again — three transitions the user never made, the
 * middle one of which would drop the pin that the last one cannot restore. So
 * `settled` gates the watch entirely and the baseline is seeded from the first
 * *settled* selection, never from the first render (rule 2: an async result
 * must prove it is the newest before it writes). While the inputs are
 * unsettled the baseline is discarded rather than kept, so a refetch that
 * un-settles them can only ever cost a comparison, never a spurious drop.
 *
 * `draft` and `onChange` are read through refs: an env edit must not re-run
 * the watch, only a harness change may.
 */
export function useHermesHarnessWatch({
  defaults,
  draft,
  enabled,
  harness,
  onChange,
  settled,
}: {
  /** Parallelism to restore when the profile that pinned 1 goes away. */
  defaults: { parallelism: string };
  draft: HermesProfileDraft;
  /** The form is open. A closed form has no selection to compare. */
  enabled: boolean;
  harness: HarnessSelection;
  onChange: (next: HermesProfileDraft) => void;
  /** Every query the harness selection is derived from has stopped loading. */
  settled: boolean;
}): void {
  const draftRef = React.useRef(draft);
  draftRef.current = draft;
  const onChangeRef = React.useRef(onChange);
  onChangeRef.current = onChange;
  const previousHarnessRef = React.useRef<HarnessSelection | null>(null);

  const { runtimeId } = harness;
  const command = harness.command ?? null;
  const defaultParallelism = defaults.parallelism;

  React.useEffect(() => {
    if (!enabled || !settled) {
      previousHarnessRef.current = null;
      return;
    }
    const previousHarness = previousHarnessRef.current;
    previousHarnessRef.current = { command, runtimeId };
    if (previousHarness === null) {
      // The first settled selection is the baseline, not a change.
      return;
    }
    const next = hermesDraftOnHarnessChange(
      draftRef.current,
      previousHarness,
      { command, runtimeId },
      { parallelism: defaultParallelism },
    );
    if (next === draftRef.current) {
      return;
    }
    onChangeRef.current(next);
  }, [command, defaultParallelism, enabled, runtimeId, settled]);
}
