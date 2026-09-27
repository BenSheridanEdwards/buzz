import assert from "node:assert/strict";
import test from "node:test";
import { relayClient } from "../../shared/api/relayClient.ts";
import {
  _testRegisterKnownAgents,
  deriveObserverConnectionState,
  ensureRelayObserverSubscription,
  getAgentObserverSnapshot,
  resetAgentObserverStore,
} from "./observerRelayStore.ts";

const snapshot = () => getAgentObserverSnapshot("aa");
const tick = () => new Promise((resolve) => setImmediate(resolve));

test("reset during identity lookup never starts an old-community subscription", async (t) => {
  let resolveIdentity;
  globalThis.window = {
    __TAURI_INTERNALS__: {
      invoke: () =>
        new Promise((resolve) => {
          resolveIdentity = resolve;
        }),
    },
  };
  t.after(() => {
    resetAgentObserverStore();
    delete globalThis.window;
  });
  resetAgentObserverStore();
  const callbacks = [];
  let unsubscribed = 0;
  t.mock.method(relayClient, "subscribeToConnectionState", (listener) => {
    callbacks.push(listener);
    listener("connected");
    return () => {
      unsubscribed++;
    };
  });
  const subscribe = t.mock.method(
    relayClient,
    "subscribeLive",
    async () => async () => {},
  );
  const oldStart = ensureRelayObserverSubscription();
  await tick();
  resetAgentObserverStore();
  assert.equal(unsubscribed, 1);
  callbacks[0]("connected");
  assert.equal(snapshot().connectionState, "idle");
  resolveIdentity({ pubkey: "old-owner", display_name: "Old" });
  await oldStart;
  assert.equal(
    subscribe.mock.callCount(),
    0,
    "stale identity must not send a REQ in the new community",
  );
});

test("observer is Live only when the relay is connected and the observer REQ is registered", async (t) => {
  globalThis.window = {
    __TAURI_INTERNALS__: {
      invoke: async () => ({ pubkey: "owner", display_name: "Owner" }),
    },
  };
  t.after(() => {
    resetAgentObserverStore();
    delete globalThis.window;
  });
  resetAgentObserverStore();
  let stateListener;
  t.mock.method(relayClient, "subscribeToConnectionState", (listener) => {
    stateListener = listener;
    listener("connecting");
    return () => {};
  });
  t.mock.method(relayClient, "subscribeLive", async () => async () => {});
  await ensureRelayObserverSubscription();
  assert.equal(
    snapshot().connectionState,
    "connecting",
    "registered subscription on a connecting relay is not live",
  );
  for (const [relay, expected] of [
    ["connected", "open"],
    ["reconnecting", "connecting"],
    ["stalled", "closed"],
    ["disconnected", "closed"],
    ["connecting", "connecting"],
    ["connected", "open"],
    ["idle", "idle"],
  ]) {
    stateListener(relay);
    assert.equal(snapshot().connectionState, expected, relay);
  }
});

test("already-connected relay does not report Live until the observer REQ is registered", async (t) => {
  let resolveIdentity;
  globalThis.window = {
    __TAURI_INTERNALS__: {
      invoke: () =>
        new Promise((resolve) => {
          resolveIdentity = resolve;
        }),
    },
  };
  t.after(() => {
    resetAgentObserverStore();
    delete globalThis.window;
  });
  resetAgentObserverStore();
  t.mock.method(relayClient, "subscribeToConnectionState", (listener) => {
    // The real emitter replays current state synchronously on subscribe.
    listener("connected");
    return () => {};
  });
  let resolveSubscription;
  t.mock.method(
    relayClient,
    "subscribeLive",
    () =>
      new Promise((resolve) => {
        resolveSubscription = resolve;
      }),
  );
  const start = ensureRelayObserverSubscription();
  assert.equal(
    snapshot().connectionState,
    "connecting",
    "connected relay before identity lookup must not be Live",
  );
  resolveIdentity({ pubkey: "owner", display_name: "Owner" });
  await tick();
  assert.equal(
    snapshot().connectionState,
    "connecting",
    "connected relay with the observer REQ still pending must not be Live",
  );
  resolveSubscription(async () => {});
  await start;
  assert.equal(snapshot().connectionState, "open");
});

test("deriveObserverConnectionState covers every relay state x subscription readiness", () => {
  const table = {
    null: ["connecting", "connecting"],
    idle: ["idle", "idle"],
    connecting: ["connecting", "connecting"],
    connected: ["connecting", "open"],
    reconnecting: ["connecting", "connecting"],
    stalled: ["closed", "closed"],
    disconnected: ["closed", "closed"],
  };
  for (const [relay, [pending, established]] of Object.entries(table)) {
    const state = relay === "null" ? null : relay;
    assert.equal(deriveObserverConnectionState(state, false), pending, relay);
    assert.equal(
      deriveObserverConnectionState(state, true),
      established,
      relay,
    );
  }
});

test("retired subscription frames cannot enter the new generation pending trust buffer", async (t) => {
  const commands = [];
  globalThis.window = {
    __TAURI_INTERNALS__: {
      invoke: async (command) => {
        commands.push(command);
        if (command === "get_identity")
          return { pubkey: "owner", display_name: "Owner" };
        return {
          seq: 1,
          timestamp: "2026-06-18T00:00:01Z",
          kind: "turn_started",
          channelId: "channel",
          sessionId: "session",
          turnId: "turn",
          payload: {},
        };
      },
    },
  };
  t.after(() => {
    resetAgentObserverStore();
    delete globalThis.window;
  });
  resetAgentObserverStore();
  let onEvent;
  t.mock.method(relayClient, "subscribeLive", async (_filter, listener) => {
    onEvent = listener;
    return async () => {};
  });
  await ensureRelayObserverSubscription();
  resetAgentObserverStore();
  onEvent({
    pubkey: "aa",
    tags: [
      ["agent", "aa"],
      ["frame", "telemetry"],
    ],
    content: "encrypted",
  });
  await tick();
  _testRegisterKnownAgents("new-community", ["aa"]);
  await tick();
  assert.deepEqual(
    commands,
    ["get_identity"],
    "old signed frame must not be decrypted under new generation",
  );
  assert.deepEqual(snapshot().events, []);
});

test("reset fences a queued trusted-frame replay before it can decrypt", async (t) => {
  const commands = [];
  globalThis.window = {
    __TAURI_INTERNALS__: {
      invoke: async (command) => {
        commands.push(command);
        return { pubkey: "owner", display_name: "Owner" };
      },
    },
  };
  t.after(() => {
    resetAgentObserverStore();
    delete globalThis.window;
  });
  resetAgentObserverStore();
  let onEvent;
  t.mock.method(relayClient, "subscribeLive", async (_filter, listener) => {
    onEvent = listener;
    return async () => {};
  });
  await ensureRelayObserverSubscription();
  onEvent({
    pubkey: "aa",
    tags: [
      ["agent", "aa"],
      ["frame", "telemetry"],
    ],
    content: "encrypted",
  });
  await tick();
  _testRegisterKnownAgents("old-community", ["aa"]);
  resetAgentObserverStore();
  _testRegisterKnownAgents("new-community", ["aa"]);
  await tick();
  assert.deepEqual(commands, ["get_identity"]);
});

test("real relay disconnect clears Live and late subscription settlement cannot restore it", async (t) => {
  globalThis.window = {
    __TAURI_INTERNALS__: {
      invoke: async () => ({ pubkey: "owner", display_name: "Owner" }),
    },
  };
  t.after(() => {
    resetAgentObserverStore();
    delete globalThis.window;
  });
  resetAgentObserverStore();
  let resolveSubscription;
  t.mock.method(
    relayClient,
    "subscribeLive",
    () =>
      new Promise((resolve) => {
        resolveSubscription = resolve;
      }),
  );
  const start = ensureRelayObserverSubscription();
  await tick();
  // Use the session's real state emitter and public teardown, not a mocked
  // lifecycle subscription: this pins the shared/api -> store wiring.
  relayClient.connectionStateEmitter.set("connected");
  assert.equal(
    snapshot().connectionState,
    "connecting",
    "pending observer REQ is not Live even on a connected relay",
  );
  relayClient.disconnect();
  assert.equal(snapshot().connectionState, "idle");
  resolveSubscription(async () => {});
  await start;
  assert.equal(snapshot().connectionState, "idle");
});

test("late old subscription settlement only closes itself and leaves the replacement live", async (t) => {
  globalThis.window = {
    __TAURI_INTERNALS__: {
      invoke: async () => ({ pubkey: "owner", display_name: "Owner" }),
    },
  };
  t.after(() => {
    resetAgentObserverStore();
    delete globalThis.window;
  });
  resetAgentObserverStore();
  const callbacks = [];
  const pending = [];
  const closed = [];
  t.mock.method(relayClient, "subscribeToConnectionState", (listener) => {
    callbacks.push(listener);
    listener("connecting");
    return () => {};
  });
  const subscribe = t.mock.method(
    relayClient,
    "subscribeLive",
    () => new Promise((resolve) => pending.push(resolve)),
  );
  const oldStart = ensureRelayObserverSubscription();
  await tick();
  resetAgentObserverStore();
  const newStart = ensureRelayObserverSubscription();
  await tick();
  pending[1](async () => {
    closed.push("new");
  });
  await newStart;
  callbacks[1]("connected");
  callbacks[0]("disconnected");
  assert.equal(snapshot().connectionState, "open");
  pending[0](async () => {
    closed.push("old");
  });
  await oldStart;
  await ensureRelayObserverSubscription();
  assert.deepEqual(closed, ["old"]);
  assert.equal(subscribe.mock.callCount(), 2);
  assert.equal(snapshot().connectionState, "open");
  resetAgentObserverStore();
  assert.deepEqual(closed, ["old", "new"]);
});
