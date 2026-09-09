import assert from "node:assert/strict";
import { describe, it } from "node:test";

// Exercises the exact production module the card imports, so a change to the
// copy or the command fails here. The raw sidecar mapping lives in
// `shared/api/relayMembershipTypes` and is covered by its own test.
import {
  RELAY_CONTAINER_PLACEHOLDER,
  noticeGroupKey,
  relayAddMemberCommand,
  relayMembershipNotice,
  relayMembershipNoticeGroups,
} from "./relayMembership.ts";

const AGENT_HEX =
  "a1f3c0ffee00000000000000000000000000000000000000000000000000beef";
/** The workspace identity the relay turned away at the roster read. */
const USER_HEX =
  "d00d00d00d00000000000000000000000000000000000000000000000000face";

/**
 * The npub the production module derives for a hex, obtained from the module
 * itself so the expectation cannot drift from the encoder the card uses.
 */
function npubOf(hex) {
  return relayMembershipNotice(hex, {
    state: "not_member",
    checkedAt: "t",
    detail: null,
  })?.npub;
}

describe("relayMembership operator command", () => {
  it("is the documented buzz-admin invocation with the lowercase hex pubkey", () => {
    assert.equal(
      relayAddMemberCommand(AGENT_HEX.toUpperCase()),
      `docker exec ${RELAY_CONTAINER_PLACEHOLDER} buzz-admin add-member --pubkey ${AGENT_HEX}`,
    );
  });
});

describe("relayMembership card notice", () => {
  it("shows nothing for a verified member or when no check ran", () => {
    assert.equal(relayMembershipNotice(AGENT_HEX, null), null);
    assert.equal(relayMembershipNotice(AGENT_HEX, undefined), null);
    assert.equal(
      relayMembershipNotice(AGENT_HEX, {
        state: "member",
        checkedAt: "t",
        detail: null,
      }),
      null,
    );
  });

  it("not_member is blocking and carries the npub, the detail and the command", () => {
    const notice = relayMembershipNotice(AGENT_HEX, {
      state: "not_member",
      checkedAt: "t",
      detail: "This relay only accepts members.",
    });
    assert.ok(notice);
    assert.equal(notice.severity, "blocked");
    assert.equal(notice.badge, "Not a relay member");
    assert.equal(notice.detail, "This relay only accepts members.");
    assert.ok(notice.npub?.startsWith("npub1"), notice.npub ?? "no npub");
    assert.equal(notice.command, relayAddMemberCommand(AGENT_HEX));
    assert.equal(notice.npubLabel, "Agent npub");
  });

  // The one path where the relay's answer is not about the agent: it refused
  // the workspace identity at the roster read, before the filters were even
  // parsed, so it was never asked about the agent. Naming the agent here
  // printed an `add-member` for a pubkey the operator may already have
  // admitted, under a card that could never clear.
  it("a roster-read refusal names the user, not the agent the relay never saw", () => {
    const notice = relayMembershipNotice(AGENT_HEX, {
      state: "not_member",
      checkedAt: "t",
      detail:
        "This relay only accepts members and it did not accept your identity.",
      subjectPubkey: USER_HEX,
    });
    assert.ok(notice);
    assert.equal(notice.npubLabel, "Your npub");
    assert.equal(notice.npub, npubOf(USER_HEX));
    assert.notEqual(notice.npub, npubOf(AGENT_HEX));
    assert.equal(notice.command, relayAddMemberCommand(USER_HEX));
    assert.equal(notice.subject, "workspace");
  });

  // The badge was the last thing on the card still saying the agent is the
  // problem: the detail, the npub, the label and the command are all the
  // user's. It also renders once per agent, so a fleet showed N broken
  // agents for one refusal of one identity.
  it("does not badge the agent as the problem when the relay refused the user", () => {
    const agentFault = relayMembershipNotice(AGENT_HEX, {
      state: "not_member",
      checkedAt: "t",
      detail: null,
    });
    const userFault = relayMembershipNotice(AGENT_HEX, {
      state: "not_member",
      checkedAt: "t",
      detail: null,
      subjectPubkey: USER_HEX,
    });
    assert.equal(agentFault?.badge, "Not a relay member");
    assert.equal(agentFault?.subject, "agent");
    assert.notEqual(userFault?.badge, agentFault?.badge);
    assert.equal(userFault?.badge, "Relay refused your identity");
  });

  // Same substitution on the non-blocking state, so an `unknown` carrying a
  // subject cannot show the user's copy next to the agent's npub.
  it("carries the subject through the unverified state too", () => {
    const notice = relayMembershipNotice(AGENT_HEX, {
      state: "unknown",
      checkedAt: "t",
      detail: "timed out",
      subjectPubkey: USER_HEX,
    });
    assert.ok(notice);
    assert.equal(notice.npubLabel, "Your npub");
    assert.equal(notice.npub, npubOf(USER_HEX));
  });

  it("unknown is non-blocking and offers no command", () => {
    const notice = relayMembershipNotice(AGENT_HEX, {
      state: "unknown",
      checkedAt: "t",
      detail: "timed out",
    });
    assert.ok(notice);
    assert.equal(notice.severity, "unverified");
    assert.equal(notice.command, null);
    assert.equal(notice.detail, "timed out");
  });

  it("a malformed pubkey degrades to a null npub instead of throwing", () => {
    const notice = relayMembershipNotice("not-hex", {
      state: "not_member",
      checkedAt: "t",
      detail: null,
    });
    assert.ok(notice);
    assert.equal(notice.npub, null);
    assert.equal(
      notice.command,
      null,
      "an add-member built from a non-pubkey could never run",
    );
  });
});

describe("relay membership notice groups", () => {
  const OTHER_AGENT_HEX =
    "b2e4d1c0ffee00000000000000000000000000000000000000000000000face1";
  const THIRD_AGENT_HEX =
    "c3f5e2d1ffee00000000000000000000000000000000000000000000000face2";
  const refusedByRelay = (subjectPubkey, detail) => ({
    state: "not_member",
    checkedAt: "t",
    detail:
      detail ??
      "This relay only accepts members and it did not accept your identity.",
    subjectPubkey,
  });

  it("collapses one refusal of the user into a single notice for the group", () => {
    const groups = relayMembershipNoticeGroups([
      { pubkey: AGENT_HEX, relayMembership: refusedByRelay(USER_HEX) },
      { pubkey: OTHER_AGENT_HEX, relayMembership: refusedByRelay(USER_HEX) },
    ]);
    assert.equal(groups.length, 1);
    assert.equal(groups[0].agentCount, 2);
    assert.equal(groups[0].notice.npub, npubOf(USER_HEX));
    assert.equal(groups[0].notice.command, relayAddMemberCommand(USER_HEX));
  });

  // The agent subject has no second renderer to fall back on: this list is
  // the only thing the section maps over, so an agent-level refusal missing
  // from it is a card with a badge and no way out.
  it("gives an agent-level refusal its own group, with the agent's remedy", () => {
    const groups = relayMembershipNoticeGroups([
      {
        pubkey: AGENT_HEX,
        name: "Bob",
        relayMembership: {
          state: "not_member",
          checkedAt: "t",
          detail: "the relay does not list this agent",
        },
      },
    ]);
    assert.equal(groups.length, 1);
    assert.equal(groups[0].notice.subject, "agent");
    assert.equal(groups[0].notice.npubLabel, "Agent npub");
    assert.equal(groups[0].notice.command, relayAddMemberCommand(AGENT_HEX));
    assert.deepEqual(groups[0].agentNames, ["Bob"]);
    assert.equal(groups[0].agentCount, 1);
  });

  it("has nothing to render when no agent carries a notice", () => {
    assert.deepEqual(relayMembershipNoticeGroups([]), []);
    assert.deepEqual(
      relayMembershipNoticeGroups([
        { pubkey: AGENT_HEX, relayMembership: null },
      ]),
      [],
    );
  });

  // Two agents refused in their own right are two remedies, never one card
  // speaking for both: the `add-member` names one agent's hex.
  it("never merges two agent-subject refusals", () => {
    const groups = relayMembershipNoticeGroups([
      {
        pubkey: AGENT_HEX,
        relayMembership: { state: "not_member", checkedAt: "t", detail: "no" },
      },
      {
        pubkey: OTHER_AGENT_HEX,
        relayMembership: { state: "not_member", checkedAt: "t", detail: "no" },
      },
    ]);
    assert.equal(groups.length, 2);
    assert.deepEqual(
      groups.map((group) => group.notice.command).sort(),
      [
        relayAddMemberCommand(AGENT_HEX),
        relayAddMemberCommand(OTHER_AGENT_HEX),
      ].sort(),
    );
  });

  // The subject is part of the key as well as the hex. A workspace refusal of
  // an identity and an agent refusal of an agent that happen to share a hex
  // are two different problems with two different labels and two different
  // sentences, so they must never land in one card.
  it("keys the subject, so one hex refused both ways is two cards", () => {
    const groups = relayMembershipNoticeGroups([
      { pubkey: AGENT_HEX, relayMembership: refusedByRelay(AGENT_HEX) },
      {
        pubkey: AGENT_HEX,
        relayMembership: { state: "not_member", checkedAt: "t", detail: "no" },
      },
    ]);
    assert.equal(groups.length, 2);
    assert.deepEqual(groups.map((group) => group.notice.subject).sort(), [
      "agent",
      "workspace",
    ]);
    assert.equal(
      new Set(groups.map(({ notice }) => noticeGroupKey(notice))).size,
      2,
      "the render key must separate them exactly as the grouping did",
    );
  });

  // Two different workspace identities are two different problems with two
  // different npubs, so they must never be merged into one card, and neither
  // may the smaller of them be dropped, which is what returning only the
  // largest group did.
  it("never merges refusals of two different identities, and drops neither", () => {
    const other =
      "cafebabe00000000000000000000000000000000000000000000000000001234";
    const groups = relayMembershipNoticeGroups([
      { pubkey: AGENT_HEX, relayMembership: refusedByRelay(USER_HEX) },
      { pubkey: OTHER_AGENT_HEX, relayMembership: refusedByRelay(USER_HEX) },
      { pubkey: THIRD_AGENT_HEX, relayMembership: refusedByRelay(other) },
    ]);
    assert.equal(groups.length, 2);
    assert.deepEqual(
      groups.map((group) => [group.notice.npub, group.agentCount]),
      [
        [npubOf(USER_HEX), 2],
        [npubOf(other), 1],
      ],
      "largest first, and the smaller identity still gets its own card",
    );
  });

  // A blocking refusal and a non-blocking unverified check on the SAME
  // identity are two states with two remedies: one carries the operator
  // command, the other deliberately carries none. Keyed on identity alone
  // they collapsed into whichever arrived first, so a genuinely blocked agent
  // could be counted into a grey card that offers no way out.
  it("does not collapse two severities on one identity into one card", () => {
    const groups = relayMembershipNoticeGroups([
      {
        pubkey: AGENT_HEX,
        relayMembership: {
          state: "unknown",
          checkedAt: "t",
          detail: "the check could not be completed",
          subjectPubkey: USER_HEX,
        },
      },
      { pubkey: OTHER_AGENT_HEX, relayMembership: refusedByRelay(USER_HEX) },
    ]);
    assert.equal(groups.length, 2);
    const blocked = groups.find((group) => group.notice.severity === "blocked");
    assert.ok(blocked, "the blocked agent must keep a blocking card");
    assert.equal(blocked.agentCount, 1);
    assert.equal(blocked.notice.command, relayAddMemberCommand(USER_HEX));
    const unverified = groups.find(
      (group) => group.notice.severity === "unverified",
    );
    assert.ok(unverified);
    assert.equal(unverified.notice.command, null);
  });

  // `safeNpub` returns null for anything it cannot encode, so grouping on the
  // rendered npub gave every malformed subject the same key.
  it("keys on the subject hex, so two malformed subjects are two cards", () => {
    const groups = relayMembershipNoticeGroups([
      { pubkey: AGENT_HEX, relayMembership: refusedByRelay("not-hex-1") },
      { pubkey: OTHER_AGENT_HEX, relayMembership: refusedByRelay("not-hex-2") },
    ]);
    assert.equal(groups.length, 2);
    assert.deepEqual(groups.map((group) => group.notice.subjectHex).sort(), [
      "not-hex-1",
      "not-hex-2",
    ]);
    for (const group of groups) {
      assert.equal(group.notice.npub, null);
      assert.equal(
        group.notice.command,
        null,
        "a malformed subject must not be printed into the operator command",
      );
      assert.equal(group.agentCount, 1);
    }
  });

  // The group card used to show the FIRST agent's sentence above a line
  // saying the problem holds up N agents, attributing one relay answer to
  // every agent in the group.
  it("renders only the part of the relay's sentence the group shares", () => {
    const [group] = relayMembershipNoticeGroups([
      {
        pubkey: AGENT_HEX,
        relayMembership: refusedByRelay(USER_HEX, "Relay said: alpha failed"),
      },
      {
        pubkey: OTHER_AGENT_HEX,
        relayMembership: refusedByRelay(USER_HEX, "Relay said: beta failed"),
      },
    ]);
    assert.equal(group.agentCount, 2);
    assert.equal(
      group.notice.detail,
      "Relay said:",
      "a differing sentence is cut back to what is true for both",
    );

    const [same] = relayMembershipNoticeGroups([
      {
        pubkey: AGENT_HEX,
        relayMembership: refusedByRelay(USER_HEX, "Relay said: the same"),
      },
      {
        pubkey: OTHER_AGENT_HEX,
        relayMembership: refusedByRelay(USER_HEX, "Relay said: the same"),
      },
    ]);
    assert.equal(same.notice.detail, "Relay said: the same");
  });
});

describe("who renders the membership block", () => {
  const agentLevel = { state: "not_member", checkedAt: "t", detail: "why" };
  const userLevel = { ...agentLevel, subjectPubkey: USER_HEX };
  const OTHER_AGENT_HEX =
    "b2e4d1c0ffee00000000000000000000000000000000000000000000000face1";
  const THIRD_AGENT_HEX =
    "c3f5e2d1ffee00000000000000000000000000000000000000000000000face2";
  const OTHER_USER_HEX =
    "cafebabe00000000000000000000000000000000000000000000000000001234";

  // Exactly one owner, always, and now there is only one renderer to be that
  // owner. The invariant used to be split across this function and a
  // `rowRelayMembershipNotice` helper, and the two were asserted to be
  // complementary here; when the component that called the helper was
  // deleted, the helper went on satisfying this test with no caller at all
  // and the agent subject rendered a badge and nothing else. Whether the
  // groups actually REACH the DOM is not something this file can see, which
  // is the whole reason `UnifiedAgentsSectionRelayMembership.test.mjs` mounts
  // the section; what this file pins is that no agent with something to say
  // is dropped on the way to it.
  //
  // Checked PER AGENT over a multi-agent, multi-identity, multi-severity,
  // multi-subject set, because that is the only shape that can see the
  // failure: with one agent per call, a renderer that returns just its
  // largest group looks complete while silently dropping every other agent.
  it("gives every notice exactly one group, and drops no agent", () => {
    const agents = [
      { pubkey: AGENT_HEX, relayMembership: userLevel },
      { pubkey: OTHER_AGENT_HEX, relayMembership: userLevel },
      {
        pubkey: THIRD_AGENT_HEX,
        relayMembership: { ...agentLevel, subjectPubkey: OTHER_USER_HEX },
      },
      {
        pubkey:
          "d4a6f3e2ffee00000000000000000000000000000000000000000000000face3",
        relayMembership: {
          state: "unknown",
          checkedAt: "t",
          detail: "why",
          subjectPubkey: USER_HEX,
        },
      },
      {
        pubkey:
          "e5b7a4f3ffee00000000000000000000000000000000000000000000000face4",
        relayMembership: agentLevel,
      },
      {
        pubkey:
          "f6c8b5a4ffee00000000000000000000000000000000000000000000000face5",
        relayMembership: null,
      },
    ];
    const groups = relayMembershipNoticeGroups(agents);
    const grouped = new Set(groups.map(({ notice }) => noticeGroupKey(notice)));
    assert.equal(
      groups.reduce((sum, group) => sum + group.agentCount, 0),
      5,
      "every agent with a notice must be counted into exactly one group",
    );

    for (const agent of agents) {
      const own = relayMembershipNotice(agent.pubkey, agent.relayMembership);
      if (own === null) continue;
      assert.ok(
        grouped.has(noticeGroupKey(own)),
        `no group renders ${agent.pubkey}`,
      );
    }
  });

  // The group is keyed on the normalized hex while the npub and the operator
  // command used to be derived from the raw one. Two agents refused as the
  // same identity, one of them carrying it with stray whitespace and in upper
  // case, grouped together correctly and then rendered whichever of the two
  // arrived first: with the padded one first the card had no npub and no
  // command at all, so the remedy for both agents disappeared on nothing more
  // than agent order.
  it("derives the npub and the command from the same hex it groups on", () => {
    const padded = `  ${USER_HEX.toUpperCase()}  `;
    for (const order of [
      [padded, USER_HEX],
      [USER_HEX, padded],
    ]) {
      const groups = relayMembershipNoticeGroups([
        { pubkey: AGENT_HEX, relayMembership: refusedAs(order[0]) },
        { pubkey: OTHER_AGENT_HEX, relayMembership: refusedAs(order[1]) },
      ]);
      assert.equal(groups.length, 1, "one identity is one card, either order");
      assert.equal(groups[0].agentCount, 2);
      assert.equal(groups[0].notice.subjectHex, USER_HEX);
      assert.equal(
        groups[0].notice.command,
        relayAddMemberCommand(USER_HEX),
        "the card must keep the remedy whichever agent arrived first",
      );
      assert.ok(groups[0].notice.npub?.startsWith("npub1"));
    }
  });

  // Presence of a named subject used to be decided twice, by `??` (which
  // keeps an empty string) and by truthiness (which does not), so an empty
  // `subject_pubkey` produced an agent-subject notice keyed on an empty hex,
  // with no npub and no command: a card about nobody.
  it("treats an empty or blank subject as no subject at all", () => {
    for (const subjectPubkey of ["", "   "]) {
      const notice = relayMembershipNotice(AGENT_HEX, {
        ...agentLevel,
        subjectPubkey,
      });
      assert.ok(notice);
      assert.equal(notice.subject, "agent");
      assert.equal(notice.subjectHex, AGENT_HEX);
      assert.equal(notice.npubLabel, "Agent npub");
      assert.equal(notice.command, relayAddMemberCommand(AGENT_HEX));
    }
  });

  function refusedAs(subjectPubkey) {
    return { ...agentLevel, subjectPubkey };
  }
});
