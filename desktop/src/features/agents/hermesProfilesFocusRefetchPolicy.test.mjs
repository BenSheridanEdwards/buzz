/**
 * The Hermes profile scan reads and base64-encodes one avatar per profile, so
 * a focus refetch is megabytes of disk work for a list that only changes when
 * the user edits `~/.hermes` by hand. This drives the production policy object
 * through a real QueryObserver: dropping `refetchOnWindowFocus: false` makes
 * the stale case refetch and fails the second test.
 */

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";

import {
  focusManager,
  QueryClient,
  QueryObserver,
} from "@tanstack/react-query";

import { hermesProfilesFocusRefetchPolicy } from "./useHermesProfiles.ts";

afterEach(() => {
  focusManager.setFocused(undefined);
});

async function focusRefetchCount({ ageMs, policy }) {
  focusManager.setFocused(false);
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  queryClient.mount();

  const queryKey = ["focus-refetch-policy", policy.staleTime, ageMs];
  queryClient.setQueryData(queryKey, "cached", {
    updatedAt: Date.now() - ageMs,
  });
  let fetchCount = 0;
  const observer = new QueryObserver(queryClient, {
    queryKey,
    queryFn: async () => {
      fetchCount += 1;
      return "refetched";
    },
    refetchOnMount: false,
    ...policy,
  });
  const unsubscribe = observer.subscribe(() => {});

  focusManager.setFocused(true);
  await new Promise((resolve) => setImmediate(resolve));

  unsubscribe();
  queryClient.unmount();
  return fetchCount;
}

test("hermes-profiles: skips fresh focus refetch", async () => {
  assert.equal(
    await focusRefetchCount({
      ageMs: hermesProfilesFocusRefetchPolicy.staleTime - 1_000,
      policy: hermesProfilesFocusRefetchPolicy,
    }),
    0,
  );
});

test("hermes-profiles: does not re-scan stale data on focus", async () => {
  assert.equal(
    await focusRefetchCount({
      ageMs: hermesProfilesFocusRefetchPolicy.staleTime + 1,
      policy: hermesProfilesFocusRefetchPolicy,
    }),
    0,
  );
});
