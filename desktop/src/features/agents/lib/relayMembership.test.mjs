import assert from "node:assert/strict";
import { describe, it } from "node:test";

// Exercises the exact production module the card imports, so a change to the
// copy or the command fails here. The raw sidecar mapping lives in
// `shared/api/relayMembershipTypes` and is covered by its own test.
import {
  RELAY_CONTAINER_PLACEHOLDER,
  relayAddMemberCommand,
  relayMembershipNotice,
  rowRelayMembershipNotice,
  workspaceRelayMembershipNotices,
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

describe("workspace-level relay membership notice", () => {
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
    const groups = workspaceRelayMembershipNotices([
      { pubkey: AGENT_HEX, relayMembership: refusedByRelay(USER_HEX) },
      { pubkey: OTHER_AGENT_HEX, relayMembership: refusedByRelay(USER_HEX) },
    ]);
    assert.equal(groups.length, 1);
    assert.equal(groups[0].agentCount, 2);
    assert.equal(groups[0].notice.npub, npubOf(USER_HEX));
    assert.equal(groups[0].notice.command, relayAddMemberCommand(USER_HEX));
  });

  it("leaves an agent-level refusal to the agent's own row", () => {
    assert.deepEqual(
      workspaceRelayMembershipNotices([
        {
          pubkey: AGENT_HEX,
          relayMembership: {
            state: "not_member",
            checkedAt: "t",
            detail: null,
          },
        },
      ]),
      [],
    );
    assert.deepEqual(workspaceRelayMembershipNotices([]), []);
    assert.deepEqual(
      workspaceRelayMembershipNotices([
        { pubkey: AGENT_HEX, relayMembership: null },
      ]),
      [],
    );
  });

  // Two different workspace identities are two different problems with two
  // different npubs, so they must never be merged into one card, and neither
  // may the smaller of them be dropped, which is what returning only the
  // largest group did.
  it("never merges refusals of two different identities, and drops neither", () => {
    const other =
      "cafebabe00000000000000000000000000000000000000000000000000001234";
    const groups = workspaceRelayMembershipNotices([
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
    const groups = workspaceRelayMembershipNotices([
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
    const groups = workspaceRelayMembershipNotices([
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
    const [group] = workspaceRelayMembershipNotices([
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

    const [same] = workspaceRelayMembershipNotices([
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

  // Exactly one owner, always. Both would show the user the same amber block
  // twice; neither would drop the npub and the operator command entirely,
  // which is the only way out of the state the block describes.
  //
  // Checked PER AGENT over a multi-agent, multi-identity, multi-severity set,
  // because that is the only shape that can see the failure: with one agent
  // per call, a group renderer that returns just its largest group looks
  // perfectly complementary while silently dropping every other agent.
  it("gives every notice exactly one owner: the row or the group", () => {
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
    const groups = workspaceRelayMembershipNotices(agents);
    const grouped = new Set(
      groups.map(({ notice }) => `${notice.subjectHex}|${notice.severity}`),
    );
    assert.equal(
      groups.reduce((sum, group) => sum + group.agentCount, 0) + 0,
      4,
      "every workspace-subject agent must be counted into exactly one group",
    );

    for (const agent of agents) {
      const own = relayMembershipNotice(agent.pubkey, agent.relayMembership);
      const row = rowRelayMembershipNotice(agent.pubkey, agent.relayMembership);
      const ownedByGroup =
        own !== null &&
        grouped.has(`${own.subjectHex}|${own.severity}`) &&
        own.subject === "workspace";
      if (own === null) {
        assert.equal(row, null, `${agent.pubkey} has nothing to render`);
        continue;
      }
      assert.equal(
        Number(Boolean(row)) + Number(ownedByGroup),
        1,
        `exactly one renderer must own ${agent.pubkey}`,
      );
    }
  });

  it("leaves an agent-level refusal on the agent's own row", () => {
    assert.equal(
      rowRelayMembershipNotice(AGENT_HEX, agentLevel)?.badge,
      "Not a relay member",
    );
    assert.equal(rowRelayMembershipNotice(AGENT_HEX, userLevel), null);
    assert.equal(rowRelayMembershipNotice(AGENT_HEX, null), null);
  });
});
