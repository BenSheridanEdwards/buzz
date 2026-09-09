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
   * The identity the relay's answer was about, trimmed and lowercased.
   *
   * This is the ONE form every other derived field is built from. Grouping
   * keys on it, never on `npub`: `safeNpub` returns null for anything it
   * cannot encode, so two distinct malformed subjects shared one key and
   * collapsed into a single card claiming to speak for both. `npub` and
   * `command` are derived from it too, so a group can never be keyed on the
   * normalized hex while rendering a remedy built from the raw one: with the
   * two disagreeing, a subject that arrived with stray whitespace or in
   * upper case grouped correctly and then rendered `npub: null,
   * command: null`, losing the remedy for every agent in the group depending
   * on which one happened to arrive first.
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
 * Whether the backend named a subject other than the agent, and what it is.
 *
 * One predicate for the whole module. Presence used to be decided twice, by
 * `??` (which keeps `""`) and by truthiness (which does not), so an empty
 * `subject_pubkey` produced an agent-subject notice carrying an empty
 * `subjectHex`: a card keyed on nothing, with no npub and no command. The
 * backend never writes one, which is exactly why the disagreement could sit
 * there unnoticed.
 */
function namedSubject(membership: ManagedAgentRelayMembership): string | null {
  const subject = membership.subjectPubkey?.trim() ?? "";
  return subject.length > 0 ? subject : null;
}

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
  const named = namedSubject(membership);
  const subjectHex = (named ?? agentPubkeyHex).trim().toLowerCase();
  const subject = named ? "workspace" : "agent";
  const npubLabel = named ? "Your npub" : "Agent npub";
  // A subject that will not encode as an npub is not a pubkey, so no
  // `add-member` built from it could ever run. The block already hides the
  // npub line in that case; printing `--pubkey not-hex` under "ask the relay
  // operator to run" would send the user off with a command that fails.
  //
  // Derived from `subjectHex`, the same value the group is keyed on: see the
  // field's doc comment for what deriving them from the raw string cost.
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
      subjectHex,
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
    subjectHex,
    npubLabel,
    command: null,
  };
}

/** The shape `relayMembershipNoticeGroups` needs from an agent. */
export type RelayMembershipSubject = {
  pubkey: string;
  /**
   * The agent's card name. An agent-subject block carries that agent's own
   * npub and its own `add-member`, so on a fleet there is one block per
   * blocked agent and the npub is the only thing telling them apart. The
   * name says which agent the remedy is for.
   */
  name?: string | null;
  relayMembership?: ManagedAgentRelayMembership | null;
};

/** One rendered block: the notice, how many agents it stands for, and which. */
export type RelayMembershipNoticeGroup = {
  notice: RelayMembershipNotice;
  agentCount: number;
  /** Names of the agents this block speaks for, in the order given. */
  agentNames: string[];
};

/**
 * Every relay-membership block the section must render, one per distinct
 * problem, for a whole set of agents.
 *
 * THIS IS THE ONLY PRODUCTION SEAM. There is exactly one renderer, so there
 * is no ownership rule to get wrong and no way for a notice to fall between
 * two of them. There used to be two: this function for the workspace subject
 * and a `rowRelayMembershipNotice` helper for the agent subject, with a test
 * asserting they were complementary. When the row component that called the
 * helper was deleted, the helper kept satisfying the test with no caller at
 * all, and an agent-subject refusal, the ordinary "you are a relay member but
 * not an admin" case, rendered a badge and nothing else: no detail, no npub,
 * no operator command. A card that names a problem and hides its only remedy.
 * Both subjects come out of here now, and
 * `UnifiedAgentsSectionRelayMembership.test.mjs` asserts both reach the DOM
 * of the mounted section, so orphaning either fails a test.
 *
 * Grouping is what keeps the workspace subject from shouting. A roster-read
 * refusal is a single user-level fact: same identity, same npub, same
 * operator command, repeated once per agent. Rendered per row it became
 * seventeen identical amber blocks on a seventeen-agent fleet, each one
 * reading as if that agent were broken. An AGENT-subject refusal groups on
 * the agent's own pubkey, so it is its own group of one: the relay said
 * something about that agent specifically and the `add-member` names that
 * agent's hex, so there is nothing to collapse.
 *
 * EVERY group is returned, never just the largest. Returning only the largest
 * meant that with agents refused as two different identities, the smaller
 * group's agents got no block at all, and their npub and operator command
 * vanished. Ordered largest first, then by key, so the render order is stable
 * across re-renders and does not depend on agent order.
 *
 * The key is `subject`, `subjectHex` AND `severity`, and every part matters:
 *
 * - `subject`, because the two subjects are different problems even for the
 *   same hex, and only the workspace one may be collapsed across agents.
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
 * Returns an empty array when no agent carries a notice at all.
 */
export function relayMembershipNoticeGroups(
  agents: readonly RelayMembershipSubject[],
): RelayMembershipNoticeGroup[] {
  const groups = new Map<
    string,
    { notices: RelayMembershipNotice[]; names: string[] }
  >();
  for (const agent of agents) {
    const notice = relayMembershipNotice(agent.pubkey, agent.relayMembership);
    if (!notice) continue;
    const key = noticeGroupKey(notice);
    const group = groups.get(key) ?? { notices: [], names: [] };
    group.notices.push(notice);
    const name = agent.name?.trim();
    if (name) group.names.push(name);
    groups.set(key, group);
  }
  return [...groups.entries()]
    .sort(([keyA, a], [keyB, b]) =>
      b.notices.length === a.notices.length
        ? keyA.localeCompare(keyB)
        : b.notices.length - a.notices.length,
    )
    .map(([, group]) => ({
      // Every field but `detail` is identical across a group by construction:
      // the key pins the subject, the identity and the severity, and badge,
      // npub, label and command are derived from `subjectHex` alone, which is
      // half the key.
      notice: { ...group.notices[0], detail: sharedDetail(group.notices) },
      agentCount: group.notices.length,
      agentNames: group.names,
    }));
}

/**
 * The React key and the grouping key, from one place so a re-render can never
 * split or merge a block the grouping did not.
 */
export function noticeGroupKey(notice: RelayMembershipNotice): string {
  return `${notice.subject}|${notice.subjectHex}|${notice.severity}`;
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
