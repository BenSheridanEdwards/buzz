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
  /**
   * Who the relay's answer was about. `workspace` is the roster-read refusal:
   * the relay turned away the USER's identity before it was ever asked about
   * an agent, so the problem, the npub and the operator command are all the
   * user's and are identical for every agent on that relay. `agent` is every
   * other path, where the relay answered about this agent specifically.
   */
  subject: "agent" | "workspace";
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
  const subject = membership.subjectPubkey ? "workspace" : "agent";
  const npubLabel = membership.subjectPubkey ? "Your npub" : "Agent npub";
  if (membership.state === "not_member") {
    return {
      // The badge is the last thing on the card still able to say the agent
      // is at fault. On the workspace branch it is not: the relay never saw
      // the agent, and one refusal of the user renders on every card, so a
      // fleet reads as N broken agents for a single problem that is not
      // theirs. The strings differ; the block header still owns the label.
      badge:
        subject === "workspace"
          ? "Relay refused your identity"
          : "Not a relay member",
      subject,
      severity: "blocked",
      detail: membership.detail,
      npub: safeNpub(subjectHex),
      npubLabel,
      command: relayAddMemberCommand(subjectHex),
    };
  }
  return {
    badge: "Relay membership unverified",
    subject,
    severity: "unverified",
    detail: membership.detail,
    npub: safeNpub(subjectHex),
    npubLabel,
    command: null,
  };
}

/**
 * The notice an agent's own ROW should render, or `null` when the group owns
 * it instead.
 *
 * The two renderers are deliberately complementary: exactly one of
 * `rowRelayMembershipNotice` and [`workspaceRelayMembershipNotice`] carries
 * any given notice, never both (the user would read the same amber block
 * twice) and never neither (the npub and the operator command would vanish).
 * Keeping the rule here, rather than as a condition in each component, is
 * what makes that testable without mounting either of them.
 */
export function rowRelayMembershipNotice(
  agentPubkeyHex: string,
  membership: ManagedAgentRelayMembership | null | undefined,
): RelayMembershipNotice | null {
  const notice = relayMembershipNotice(agentPubkeyHex, membership);
  return notice?.subject === "workspace" ? null : notice;
}

/** The shape `workspaceRelayMembershipNotice` needs from an agent. */
export type RelayMembershipSubject = {
  pubkey: string;
  relayMembership?: ManagedAgentRelayMembership | null;
};

/**
 * The one notice to render for a whole group of agents when the relay's
 * answer was about the USER, not about any agent.
 *
 * A roster-read refusal is a single user-level fact: same identity, same
 * npub, same operator command, repeated once per agent. Rendered per row it
 * became seventeen identical amber blocks on a seventeen-agent fleet, each
 * one reading as if that agent were broken. Collapsing it to one keeps the
 * remedy exactly where it was and states how many agents it holds up.
 *
 * Returns `null` when no agent in the group carries one. Agents whose card
 * state is about themselves are untouched and keep their own block.
 */
export function workspaceRelayMembershipNotice(
  agents: readonly RelayMembershipSubject[],
): { notice: RelayMembershipNotice; agentCount: number } | null {
  const byNpub = new Map<string, RelayMembershipNotice[]>();
  for (const agent of agents) {
    const notice = relayMembershipNotice(agent.pubkey, agent.relayMembership);
    if (notice?.subject !== "workspace") continue;
    // Keyed by the refused identity, not by the agent: two workspace
    // identities cannot be collapsed into one card.
    const key = notice.npub ?? "";
    const group = byNpub.get(key);
    if (group) group.push(notice);
    else byNpub.set(key, [notice]);
  }
  let largest: RelayMembershipNotice[] | null = null;
  for (const group of byNpub.values()) {
    if (!largest || group.length > largest.length) largest = group;
  }
  const first = largest?.[0];
  if (!first || !largest) return null;
  return { notice: first, agentCount: largest.length };
}
