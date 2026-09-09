import { useQuery } from "@tanstack/react-query";

import { listHermesProfiles } from "@/shared/api/tauriHermesProfiles";

export const hermesProfilesQueryKey = ["hermes-profiles"] as const;

/**
 * Profiles change when the user edits `~/.hermes` by hand, which does not
 * happen while an agent form is open. A scan reads and base64-encodes one
 * avatar per profile, so refetching on every window focus would re-do
 * megabytes of work for a list that has not moved; the dialog fetches once and
 * a later open past `staleTime` picks up any change.
 */
export const hermesProfilesFocusRefetchPolicy = {
  refetchOnWindowFocus: false,
  staleTime: 30_000,
} as const;

/**
 * Hermes profiles installed under `~/.hermes/profiles`. Only enabled while a
 * form shows the Hermes harness, so non-Hermes flows never touch the disk.
 */
export function useHermesProfilesQuery(options?: { enabled?: boolean }) {
  return useQuery({
    ...hermesProfilesFocusRefetchPolicy,
    enabled: options?.enabled ?? true,
    queryKey: hermesProfilesQueryKey,
    queryFn: listHermesProfiles,
  });
}
