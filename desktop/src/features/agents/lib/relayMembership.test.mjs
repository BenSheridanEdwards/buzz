import assert from "node:assert/strict";
import { describe, it } from "node:test";

// Exercises the exact production module the card imports, so a change to the
// copy or the command fails here. The raw sidecar mapping lives in
// `shared/api/relayMembershipTypes` and is covered by its own test.
import {
  RELAY_CONTAINER_PLACEHOLDER,
  relayAddMemberCommand,
  relayMembershipNotice,
} from "./relayMembership.ts";

const AGENT_HEX =
  "a1f3c0ffee00000000000000000000000000000000000000000000000000beef";

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
