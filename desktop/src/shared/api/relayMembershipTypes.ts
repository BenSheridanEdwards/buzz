/**
 * Relay-membership sidecar record in both the shape Rust sends and the shape
 * the UI consumes. Lives in `shared/api` next to `fromRawManagedAgent`, which
 * is its only caller: the normalizer must not reach into a feature module for
 * the mapping of a field it owns.
 */

import type { ManagedAgentRelayMembership } from "@/shared/api/types";

/**
 * Raw shape emitted by the Rust `ManagedAgentRelayMembership`
 * (`managed_agents/relay_membership.rs`).
 */
export type RawRelayMembership = {
  state: "member" | "not_member" | "unknown";
  checked_at: string;
  detail?: string | null;
};

/** Map the Rust sidecar record onto the camelCase client type. */
export function fromRawRelayMembership(
  raw: RawRelayMembership | null | undefined,
): ManagedAgentRelayMembership | null {
  if (!raw) return null;
  return {
    state: raw.state,
    checkedAt: raw.checked_at,
    detail: raw.detail ?? null,
  };
}
