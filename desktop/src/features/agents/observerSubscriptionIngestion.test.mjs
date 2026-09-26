import assert from "node:assert/strict";
import test from "node:test";
import { relayClient } from "../../shared/api/relayClient.ts";
import { KIND_AGENT_OBSERVER_FRAME } from "../../shared/constants/kinds.ts";
import {
  _testRegisterKnownAgents,
  ensureRelayObserverSubscription,
  getAgentObserverSnapshot,
  resetAgentObserverStore,
} from "./observerRelayStore.ts";

const owner = "11".repeat(32);
const agent = "aa".repeat(32);
const foreign = "bb".repeat(32);
const tick = () => new Promise((resolve) => setImmediate(resolve));
const snapshot = () => getAgentObserverSnapshot(agent);
const telemetry = {
  seq: 1,
  timestamp: "2026-06-18T00:00:01Z",
  kind: "turn_started",
  channelId: "channel",
  sessionId: "session",
  turnId: "turn",
  payload: {},
};

// Signature verification and decryption belong to transport/native code. These
// fixtures enter at subscribeLive's delivery boundary, not a store ingest helper.
function frame(claimedAgent = agent, signer = claimedAgent) {
  return {
    id: "cc".repeat(32),
    pubkey: signer,
    kind: KIND_AGENT_OBSERVER_FRAME,
    created_at: 1781740801,
    tags: [
      ["p", owner],
      ["agent", claimedAgent],
      ["frame", "telemetry"],
    ],
    content: "encrypted-telemetry",
    sig: "dd".repeat(64),
  };
}

function installIpc(t, invoke) {
  const previousWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  resetAgentObserverStore();
  globalThis.window = { __TAURI_INTERNALS__: { invoke } };
  t.after(() => {
    resetAgentObserverStore();
    if (previousWindow) {
      Object.defineProperty(globalThis, "window", previousWindow);
    } else {
      delete globalThis.window;
    }
  });
}

function installFrames(t) {
  const decryptions = [];
  installIpc(t, async (command, args) => {
    if (command === "get_identity")
      return { pubkey: owner, display_name: "Owner" };
    assert.equal(command, "decrypt_observer_event", "unexpected IPC command");
    decryptions.push(JSON.parse(args.eventJson));
    return telemetry;
  });
  t.mock.method(relayClient, "subscribeToConnectionState", (listener) => {
    listener("connected");
    return () => {};
  });
  const deliveries = [];
  const subscribe = t.mock.method(
    relayClient,
    "subscribeLive",
    async (_filter, listener) => {
      deliveries.push(listener);
      return async () => {};
    },
  );
  return { decryptions, deliveries, subscribe };
}

for (const trustTiming of ["before delivery", "after delivery"]) {
  test(`trusted live frame is ingested with ownership registered ${trustTiming}`, async (t) => {
    const { decryptions, deliveries, subscribe } = installFrames(t);
    if (trustTiming === "before delivery") {
      _testRegisterKnownAgents("community", [agent]);
    }
    await ensureRelayObserverSubscription();
    assert.equal(subscribe.mock.callCount(), 1);
    const filter = subscribe.mock.calls[0].arguments[0];
    assert.deepEqual(filter.kinds, [KIND_AGENT_OBSERVER_FRAME]);
    assert.deepEqual(filter["#p"], [owner]);
    const event = frame();
    deliveries[0](event);
    await tick();
    if (trustTiming === "after delivery") {
      assert.deepEqual(
        decryptions,
        [],
        "untrusted startup frames wait for ownership",
      );
      assert.deepEqual(snapshot().events, []);
      _testRegisterKnownAgents("community", [agent]);
      await tick();
    }
    assert.deepEqual(
      decryptions,
      [event],
      "accepted frame reaches native decryption exactly once",
    );
    assert.deepEqual(snapshot(), {
      connectionState: "open",
      errorMessage: null,
      events: [telemetry],
    });
    assert.deepEqual(getAgentObserverSnapshot(foreign).events, []);
  });
}

for (const [name, rejected] of [
  ["foreign agent", frame(foreign)],
  ["mismatched signer", frame(agent, foreign)],
]) {
  test(`${name} is rejected before decrypt while trusted traffic still flows`, async (t) => {
    const { decryptions, deliveries } = installFrames(t);
    _testRegisterKnownAgents("community", [agent]);
    await ensureRelayObserverSubscription();
    deliveries[0](rejected);
    await tick();
    assert.deepEqual(
      decryptions,
      [],
      "rejected frames must not reach native decryption",
    );
    assert.deepEqual(snapshot().events, []);
    assert.deepEqual(getAgentObserverSnapshot(foreign).events, []);
    assert.equal(snapshot().errorMessage, null);
    // Positive control on the same callback makes unconditional dropping fail.
    const accepted = frame();
    deliveries[0](accepted);
    await tick();
    assert.deepEqual(decryptions, [accepted]);
    assert.deepEqual(snapshot().events, [telemetry]);
    assert.deepEqual(getAgentObserverSnapshot(foreign).events, []);
    assert.equal(snapshot().connectionState, "open");
  });
}

for (const failureStage of ["identity", "subscription"]) {
  test(`${failureStage} setup failure cleans up lifecycle and permits a single retry`, async (t) => {
    let rejectSetup;
    const failure = new Promise((_resolve, reject) => {
      rejectSetup = reject;
    });
    let identityCalls = 0;
    const decryptions = [];
    installIpc(t, async (command, args) => {
      if (command === "get_identity") {
        identityCalls++;
        if (failureStage === "identity" && identityCalls === 1) return failure;
        return { pubkey: owner, display_name: "Owner" };
      }
      assert.equal(command, "decrypt_observer_event");
      decryptions.push(JSON.parse(args.eventJson));
      return telemetry;
    });
    const activeListeners = new Set();
    const removed = [];
    const lifecycle = t.mock.method(
      relayClient,
      "subscribeToConnectionState",
      (listener) => {
        const index = lifecycle.mock.callCount();
        activeListeners.add(listener);
        listener("connecting");
        return () => {
          removed.push(index);
          activeListeners.delete(listener);
        };
      },
    );
    let closeCalls = 0;
    let deliver;
    const subscribe = t.mock.method(
      relayClient,
      "subscribeLive",
      async (_filter, listener) => {
        if (failureStage === "subscription" && subscribe.mock.callCount() === 0)
          return failure;
        deliver = listener;
        return async () => {
          closeCalls++;
        };
      },
    );
    _testRegisterKnownAgents("community", [agent]);
    const start = ensureRelayObserverSubscription();
    assert.equal(
      ensureRelayObserverSubscription(),
      start,
      "concurrent callers share setup",
    );
    await tick();
    assert.equal(activeListeners.size, 1);
    assert.equal(
      subscribe.mock.callCount(),
      failureStage === "identity" ? 0 : 1,
    );
    rejectSetup(new Error(`${failureStage} unavailable`));
    await start;
    assert.deepEqual(snapshot(), {
      connectionState: "error",
      errorMessage: `${failureStage} unavailable`,
      events: [],
    });
    assert.equal(
      activeListeners.size,
      0,
      "failed setup detaches lifecycle listener",
    );
    assert.deepEqual(removed, [0]);
    assert.equal(closeCalls, 0, "failed setup never acquired a live handle");
    for (const listener of activeListeners) listener("connected");
    assert.equal(snapshot().connectionState, "error");

    const retry = ensureRelayObserverSubscription();
    assert.notEqual(
      retry,
      start,
      "settled failed startPromise must be cleared",
    );
    assert.equal(ensureRelayObserverSubscription(), retry);
    assert.equal(snapshot().connectionState, "connecting");
    assert.equal(snapshot().errorMessage, null);
    await retry;
    assert.equal(identityCalls, 2);
    assert.equal(lifecycle.mock.callCount(), 2);
    assert.equal(activeListeners.size, 1);
    assert.equal(
      subscribe.mock.callCount(),
      failureStage === "identity" ? 1 : 2,
    );
    assert.equal(
      snapshot().connectionState,
      "connecting",
      "setup completion is not authentication",
    );
    for (const listener of activeListeners) listener("connected");
    assert.equal(snapshot().connectionState, "open");
    const event = frame();
    deliver(event);
    await tick();
    assert.deepEqual(decryptions, [event]);
    assert.deepEqual(snapshot().events, [telemetry]);
    await ensureRelayObserverSubscription();
    assert.equal(identityCalls, 2, "successful retry is reused");
    assert.equal(lifecycle.mock.callCount(), 2);
    assert.equal(
      subscribe.mock.callCount(),
      failureStage === "identity" ? 1 : 2,
    );
    resetAgentObserverStore();
    resetAgentObserverStore();
    assert.equal(activeListeners.size, 0);
    assert.deepEqual(
      removed,
      [0, 1],
      "each lifecycle listener is removed once",
    );
    assert.equal(closeCalls, 1, "successful handle closes exactly once");
    assert.deepEqual(snapshot(), {
      connectionState: "idle",
      errorMessage: null,
      events: [],
    });
  });
}
