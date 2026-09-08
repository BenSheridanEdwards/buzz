import { useQuery } from "@tanstack/react-query";

import { listHermesProfiles } from "@/shared/api/tauriHermesProfiles";

export const hermesProfilesQueryKey = ["hermes-profiles"] as const;

/**
 * Hermes profiles installed under `~/.hermes/profiles`. Only enabled while a
 * form shows the Hermes harness, so non-Hermes flows never touch the disk.
 */
export function useHermesProfilesQuery(options?: { enabled?: boolean }) {
  return useQuery({
    enabled: options?.enabled ?? true,
    queryKey: hermesProfilesQueryKey,
    queryFn: listHermesProfiles,
    staleTime: 30_000,
  });
}
