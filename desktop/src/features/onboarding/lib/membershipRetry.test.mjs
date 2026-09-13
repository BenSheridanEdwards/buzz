import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  membershipRetryNotice,
  reengageRelayForMembershipRetry,
} from "./membershipRetry.ts";

describe("reengageRelayForMembershipRetry", () => {
  it("re-opens the session before the re-check", async () => {
    let calls = 0;
    await reengageRelayForMembershipRetry(async () => {
      calls += 1;
    });
    assert.equal(calls, 1);
  });

  it("swallows a rejected reconnect so the re-check can report it", async () => {
    // Still not a member: the relay rejects AUTH again. The retry must not
    // throw here; the membership check that follows owns the verdict.
    await assert.doesNotReject(() =>
      reengageRelayForMembershipRetry(async () => {
        throw new Error("restricted: not a relay member");
      }),
    );
  });
});

describe("membershipRetryNotice", () => {
  it("says something when the re-check reached no verdict", () => {
    assert.match(membershipRetryNotice("unreachable"), /reach the relay/);
    assert.match(membershipRetryNotice("error"), /returned an error/);
  });

  it("stays quiet when the screen already says what happened", () => {
    assert.equal(membershipRetryNotice("denied"), null);
    assert.equal(membershipRetryNotice("advanced"), null);
  });
});
