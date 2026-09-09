import assert from "node:assert/strict";
import { describe, it } from "node:test";

// The exact production module `fromRawManagedAgent` imports, so a change to
// the raw sidecar mapping fails here.
import { fromRawRelayMembership } from "./relayMembershipTypes.ts";

describe("relayMembership raw mapping", () => {
  it("maps the sidecar record and defaults a missing detail to null", () => {
    assert.deepEqual(
      fromRawRelayMembership({ state: "member", checked_at: "t1" }),
      { state: "member", checkedAt: "t1", detail: null, subjectPubkey: null },
    );
    assert.deepEqual(
      fromRawRelayMembership({
        state: "not_member",
        checked_at: "t2",
        detail: "nope",
      }),
      {
        state: "not_member",
        checkedAt: "t2",
        detail: "nope",
        subjectPubkey: null,
      },
    );
  });

  // The backend omits `subject_pubkey` on every path where the relay answered
  // about the agent, and sets it only for the roster-read refusal, where the
  // relay refused the workspace identity and never saw the agent. Dropping it
  // in the mapping would silently send the card back to naming the agent.
  it("carries the subject pubkey when the relay refused the workspace identity", () => {
    assert.deepEqual(
      fromRawRelayMembership({
        state: "not_member",
        checked_at: "t3",
        detail: "did not accept your identity",
        subject_pubkey: "a1f3",
      }),
      {
        state: "not_member",
        checkedAt: "t3",
        detail: "did not accept your identity",
        subjectPubkey: "a1f3",
      },
    );
  });

  it("open relay / no check yet is null, not a fabricated state", () => {
    assert.equal(fromRawRelayMembership(null), null);
    assert.equal(fromRawRelayMembership(undefined), null);
  });
});
