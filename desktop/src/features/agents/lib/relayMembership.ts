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
   * The identity the relay's answer was about, as the lowercased hex the
   * backend wrote. Grouping keys on THIS, never on `npub`: `safeNpub` returns
   * null for anything it cannot encode, so two distinct malformed subjects
   * shared one key and collapsed into a single card claiming to speak for
   * both.
   */
  subjectHex: string;
  /**
   * Whose npub it is. The single owner of that label: the block renders this
   * string and derives the copy button's accessible name from it, so the two
   * can never disagree about the subject.
   */
  npubLabel: string;
  /**
   * Full operator command; null when the check is only unverified, and null
   * for a subject that will not encode as an npub (no command built from a
   * non-pubkey could run).
   */
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
  const normalizedSubjectHex = subjectHex.trim().toLowerCase();
  const subject = membership.subjectPubkey ? "workspace" : "agent";
  const npubLabel = membership.subjectPubkey ? "Your npub" : "Agent npub";
  // A subject that will not encode as an npub is not a pubkey, so no
  // `add-member` built from it could ever run. The block already hides the
  // npub line in that case; printing `--pubkey not-hex` under "ask the relay
  // operator to run" would send the user off with a command that fails.
  const npub = safeNpub(subjectHex);
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
      npub,
      subjectHex: normalizedSubjectHex,
      npubLabel,
      command: npub ? relayAddMemberCommand(subjectHex) : null,
    };
  }
  return {
    badge: "Relay membership unverified",
    subject,
    severity: "unverified",
    detail: membership.detail,
    npub,
    subjectHex: normalizedSubjectHex,
    npubLabel,
    command: null,
  };
}

/**
 * The notice an agent's own ROW should render, or `null` when the group owns
 * it instead.
 *
 * The two renderers are deliberately complementary: exactly one of
 * `rowRelayMembershipNotice` and [`workspaceRelayMembershipNotices`] carries
 * any given notice, never both (the user would read the same amber block
 * twice) and never neither (the npub and the operator command would vanish).
 * Keeping the rule here, rather than as a condition in each component, is
 * what makes that testable without mounting either of them.
 *
 * "Never neither" is a per-AGENT property, not a per-call one: it is only
 * true if the group renderer returns a notice covering EVERY agent it
 * suppressed, which is why `workspaceRelayMembershipNotices` returns all of
 * them rather than the largest.
 */
export function rowRelayMembershipNotice(
  agentPubkeyHex: string,
  membership: ManagedAgentRelayMembership | null | undefined,
): RelayMembershipNotice | null {
  const notice = relayMembershipNotice(agentPubkeyHex, membership);
  return notice?.subject === "workspace" ? null : notice;
}

/** The shape `workspaceRelayMembershipNotices` needs from an agent. */
export type RelayMembershipSubject = {
  pubkey: string;
  relayMembership?: ManagedAgentRelayMembership | null;
};

/**
 * One notice per distinct problem for a whole set of agents, when the relay's
 * answer was about the USER rather than about any agent.
 *
 * A roster-read refusal is a single user-level fact: same identity, same
 * npub, same operator command, repeated once per agent. Rendered per row it
 * became seventeen identical amber blocks on a seventeen-agent fleet, each
 * one reading as if that agent were broken. Collapsing it keeps the remedy
 * exactly where it was and states how many agents it holds up.
 *
 * EVERY group is returned, not the largest one. Returning only the largest
 * meant that with agents refused as two different identities, the smaller
 * group's agents got no notice from anybody: the rows suppress every
 * workspace-subject notice unconditionally, so their npub and their operator
 * command vanished, the "never neither" half of the ownership rule, broken.
 * Ordered largest first, then by key, so the render order is stable across
 * re-renders and does not depend on agent order.
 *
 * The key is `subjectHex` AND `severity`, and both halves matter:
 *
 * - `subjectHex` rather than the rendered npub, because `safeNpub` returns
 *   null for anything malformed and every malformed subject then shared one
 *   key.
 * - `severity`, because a blocking `not_member` and a non-blocking `unknown`
 *   on the same identity are two different states with two different remedies
 *   one has an operator command, the other deliberately has none. Keyed on
 *   identity alone they collapsed into whichever arrived first, so a genuinely
 *   blocked agent could be counted into a grey "unverified" card and lose its
 *   command entirely.
 *
 * Returns an empty array when no agent carries a workspace-level notice.
 * Agents whose card state is about themselves are untouched and keep their
 * own block.
 */
export function workspaceRelayMembershipNotices(
  agents: readonly RelayMembershipSubject[],
): { notice: RelayMembershipNotice; agentCount: number }[] {
  const groups = new Map<string, RelayMembershipNotice[]>();
  for (const agent of agents) {
    const notice = relayMembershipNotice(agent.pubkey, agent.relayMembership);
    if (notice?.subject !== "workspace") continue;
    const key = `${notice.subjectHex}|${notice.severity}`;
    const group = groups.get(key);
    if (group) group.push(notice);
    else groups.set(key, [notice]);
  }
  return [...groups.entries()]
    .sort(([keyA, a], [keyB, b]) =>
      b.length === a.length ? keyA.localeCompare(keyB) : b.length - a.length,
    )
    .map(([, group]) => ({
      // Every field but `detail` is identical across a group by construction:
      // the key pins the identity and the severity, and badge, npub, label and
      // command are derived from those alone.
      notice: { ...group[0], detail: sharedDetail(group) },
      agentCount: group.length,
    }));
}

/**
 * The part of the relay's sentence that is true for every agent in a group.
 *
 * The card used to render the FIRST agent's `detail` above a line saying the
 * problem holds up N agents, which attributes one agent's sentence to all of
 * them. Identical details (the common case, since the group shares a cause)
 * are returned unchanged; when they differ, only the shared prefix survives,
 * trimmed back to a word boundary so a sentence is never cut mid-word. If
 * nothing meaningful is shared the group shows no detail rather than a
 * misattributed one.
 */
function sharedDetail(
  notices: readonly RelayMembershipNotice[],
): string | null {
  const details = notices.map((notice) => notice.detail ?? "");
  if (details.some((detail) => detail === "")) return null;
  const [first, ...rest] = details;
  if (rest.every((detail) => detail === first)) return first;

  let end = first.length;
  for (const detail of rest) {
    end = Math.min(end, detail.length);
    let index = 0;
    while (index < end && detail[index] === first[index]) index += 1;
    end = index;
  }
  // Cut back to the last word boundary: a prefix that stops mid-word reads as
  // a rendering bug rather than as a shared sentence.
  const prefix = first.slice(0, end).replace(/\S*$/, "").trim();
  return prefix.length > 0 ? prefix : null;
}
