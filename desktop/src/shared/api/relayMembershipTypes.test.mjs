import assert from "node:assert/strict";
import { describe, it } from "node:test";

// The exact production module `fromRawManagedAgent` imports, so a change to
// the raw sidecar mapping fails here.
import { fromRawRelayMembership } from "./relayMembershipTypes.ts";

describe("relayMembership raw mapping", () => {
  it("maps the sidecar record and defaults a missing detail to null", () => {
    assert.deepEqual(
      fromRawRelayMembership({ state: "member", checked_at: "t1" }),
      { state: "member", checkedAt: "t1", detail: null },
    );
    assert.deepEqual(
      fromRawRelayMembership({
        state: "not_member",
        checked_at: "t2",
        detail: "nope",
      }),
      { state: "not_member", checkedAt: "t2", detail: "nope" },
    );
  });

  it("open relay / no check yet is null, not a fabricated state", () => {
    assert.equal(fromRawRelayMembership(null), null);
    assert.equal(fromRawRelayMembership(undefined), null);
  });
});
