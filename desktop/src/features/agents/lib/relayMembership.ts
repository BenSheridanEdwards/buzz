import type { ManagedAgentRelayMembership } from "@/shared/api/types";
import { safeNpub } from "@/shared/lib/nostrUtils";

/**
 * Placeholder the operator replaces with their relay container name. Shown
 * verbatim so the command is copy-paste-then-edit, never silently wrong.
 */
export const RELAY_CONTAINER_PLACEHOLDER = "<relay-container>";

/**
 * The exact operator command that admits a managed agent on a closed relay.
 * `buzz-admin add-member` accepts hex or npub; hex is used because that is
 * the form the relay logs and the roster event carry.
 */
export function relayAddMemberCommand(agentPubkeyHex: string): string {
  return `docker exec ${RELAY_CONTAINER_PLACEHOLDER} buzz-admin add-member --pubkey ${agentPubkeyHex.trim().toLowerCase()}`;
}

export type RelayMembershipNotice = {
  /** Badge label on the agent card. */
  badge: string;
  /** Visual weight: `not_member` is a blocking condition, `unknown` is not. */
  severity: "blocked" | "unverified";
  /** Persisted detail from the check, verbatim. */
  detail: string | null;
  /** The npub for the operator; null only for a malformed pubkey. */
  npub: string | null;
  /**
   * Whose npub it is. The single owner of that label: the block renders this
   * string and derives the copy button's accessible name from it, so the two
   * can never disagree about the subject.
   */
  npubLabel: string;
  /** Full operator command; null when the relay verified the check as unknown. */
  command: string | null;
};

/**
 * Card copy for a membership record, or `null` when there is nothing to show
 * (open relay, no check yet, or a verified member).
 */
export function relayMembershipNotice(
  agentPubkeyHex: string,
  membership: ManagedAgentRelayMembership | null | undefined,
): RelayMembershipNotice | null {
  if (!membership || membership.state === "member") return null;
  // The relay answers about the agent on every path but one: when it refused
  // the workspace identity at the roster read it never saw the agent, and the
  // backend records whose identity it actually refused. Naming the agent
  // there, and printing an `add-member` for the agent's hex, sends the user
  // to an operator with a command that may already have been run and that
  // could never clear the card.
  const subjectHex = membership.subjectPubkey ?? agentPubkeyHex;
  const npubLabel = membership.subjectPubkey ? "Your npub" : "Agent npub";
  if (membership.state === "not_member") {
    return {
      badge: "Not a relay member",
      severity: "blocked",
      detail: membership.detail,
      npub: safeNpub(subjectHex),
      npubLabel,
      command: relayAddMemberCommand(subjectHex),
    };
  }
  return {
    badge: "Relay membership unverified",
    severity: "unverified",
    detail: membership.detail,
    npub: safeNpub(subjectHex),
    npubLabel,
    command: null,
  };
}
