import assert from "node:assert/strict";
import { describe, it } from "node:test";

// Exercises the exact production module the card imports, so a change to the
// copy or the command fails here. The raw sidecar mapping lives in
// `shared/api/relayMembershipTypes` and is covered by its own test.
import {
  RELAY_CONTAINER_PLACEHOLDER,
  relayAddMemberCommand,
  relayMembershipNotice,
  workspaceRelayMembershipNotice,
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
  });
});

describe("workspace-level relay membership notice", () => {
  const OTHER_AGENT_HEX =
    "b2e4d1c0ffee00000000000000000000000000000000000000000000000face1";
  const refusedByRelay = (subjectPubkey) => ({
    state: "not_member",
    checkedAt: "t",
    detail: "This relay only accepts members and it did not accept your identity.",
    subjectPubkey,
  });

  it("collapses one refusal of the user into a single notice for the group", () => {
    const collapsed = workspaceRelayMembershipNotice([
      { pubkey: AGENT_HEX, relayMembership: refusedByRelay(USER_HEX) },
      { pubkey: OTHER_AGENT_HEX, relayMembership: refusedByRelay(USER_HEX) },
    ]);
    assert.ok(collapsed);
    assert.equal(collapsed.agentCount, 2);
    assert.equal(collapsed.notice.npub, npubOf(USER_HEX));
    assert.equal(collapsed.notice.command, relayAddMemberCommand(USER_HEX));
  });

  it("leaves an agent-level refusal to the agent's own row", () => {
    assert.equal(
      workspaceRelayMembershipNotice([
        {
          pubkey: AGENT_HEX,
          relayMembership: { state: "not_member", checkedAt: "t", detail: null },
        },
      ]),
      null,
    );
    assert.equal(workspaceRelayMembershipNotice([]), null);
    assert.equal(
      workspaceRelayMembershipNotice([
        { pubkey: AGENT_HEX, relayMembership: null },
      ]),
      null,
    );
  });

  // Two different workspace identities are two different problems with two
  // different npubs, so they must never be merged into one card.
  it("never merges refusals of two different identities", () => {
    const other =
      "cafebabe00000000000000000000000000000000000000000000000000001234";
    const collapsed = workspaceRelayMembershipNotice([
      { pubkey: AGENT_HEX, relayMembership: refusedByRelay(USER_HEX) },
      { pubkey: OTHER_AGENT_HEX, relayMembership: refusedByRelay(other) },
    ]);
    assert.ok(collapsed);
    assert.equal(collapsed.agentCount, 1);
  });
});
