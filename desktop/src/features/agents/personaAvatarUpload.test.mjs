/**
 * A persona edit must never carry a base64 avatar to the backend.
 *
 * `update_persona` copies the persona's avatar into every linked instance and
 * republishes their kind:0 `picture`; the relay rejects any event content over
 * 256 KiB and the persona sync sweep then retries the rejected head every 30 s
 * forever. Create already uploads data URLs to relay media, and a profile-
 * picked avatar arrives as exactly such a data URL, so edit must too.
 *
 * The contract on this seam: **an absent `avatarUrl` clears the stored
 * avatar.** `UpdatePersonaRequest.avatar_url` is an `Option<String>` that
 * serde fills with `None` for a missing key, `update.rs` assigns it
 * unconditionally, and that is how the definition dialog's "Remove avatar"
 * affordance works. So a failed upload must never resolve to an absent avatar:
 * that would read as a deliberate removal and wipe the persona's face and
 * every linked instance's while reporting success. It propagates instead.
 *
 * These tests drive the production mutation hooks through a real
 * QueryClientProvider against a stubbed Tauri bridge: deleting the
 * `personaInputWithResolvedAvatar` call in either mutationFn leaves the data
 * URL in the `update_persona` / `update_persona_and_publish` payload and fails
 * here, and restoring the swallow-and-return-`undefined` failure branch fails
 * `a failed upload rejects the save instead of clearing the avatar`.
 */

import assert from "node:assert/strict";
import { after, afterEach, before, describe, it } from "node:test";
import { JSDOM } from "jsdom";

const dom = new JSDOM("<!doctype html><html><body></body></html>", {
  url: "http://localhost",
});

Object.assign(globalThis, {
  HTMLElement: dom.window.HTMLElement,
  IS_REACT_ACT_ENVIRONMENT: true,
  MutationObserver: dom.window.MutationObserver,
  document: dom.window.document,
  self: dom.window,
  window: dom.window,
});
Object.defineProperty(globalThis, "navigator", {
  configurable: true,
  value: dom.window.navigator,
});

// ── Tauri IPC stub ────────────────────────────────────────────────────────────

const UPLOADED_URL = "https://relay.example/media/abc123.jpg";

/** Every invoke the code under test made, in order. */
const calls = [];

/** Flipped by the failure test so relay media rejects the upload. */
let uploadFails = false;

const rawPersona = {
  id: "persona-1",
  display_name: "Bond",
  avatar_url: UPLOADED_URL,
  system_prompt: "",
  is_builtin: false,
  created_at: 1,
  updated_at: 2,
};

globalThis.__TAURI_INTERNALS__ = {
  invoke: (command, args) => {
    calls.push({ args, command });
    if (command === "upload_media_bytes") {
      if (uploadFails) {
        return Promise.reject(new Error("relay media is offline"));
      }
      return Promise.resolve({
        sha256: "deadbeef",
        size: args.data.length,
        type: "image/jpeg",
        uploaded: 0,
        url: UPLOADED_URL,
      });
    }
    if (command === "update_persona") return Promise.resolve(rawPersona);
    if (command === "update_persona_and_publish") {
      return Promise.resolve({
        persona: rawPersona,
        publicationStatus: "published",
      });
    }
    return Promise.reject(new Error(`unmocked: ${command}`));
  },
  transformCallback: () => 1,
};
dom.window.__TAURI_INTERNALS__ = globalThis.__TAURI_INTERNALS__;

// A 1x1 JPEG-ish payload; what matters is that it is a base64 data URL, the
// shape `scan_hermes_profiles` hands the picker.
const AVATAR_DATA_URL = "data:image/jpeg;base64,/9j/4AAQ";

// ── Deferred imports ──────────────────────────────────────────────────────────

let React,
  act,
  createRoot,
  QueryClient,
  QueryClientProvider,
  useUpdatePersonaMutation,
  useUpdatePersonaAndPublishMutation;

before(async () => {
  ({ default: React, act } = await import("react"));
  ({ createRoot } = await import("react-dom/client"));
  ({ QueryClient, QueryClientProvider } = await import(
    "@tanstack/react-query"
  ));
  ({ useUpdatePersonaMutation } = await import("./hooks.ts"));
  ({ useUpdatePersonaAndPublishMutation } = await import(
    "./lib/usePersonaCatalogRelay.ts"
  ));
});

after(() => {
  dom.window.close();
});

/** Mount a mutation hook and expose its latest value. */
function mountMutation(useMutationHook) {
  const latest = { current: null };
  function Probe() {
    latest.current = useMutationHook();
    return null;
  }
  const container = dom.window.document.createElement("div");
  dom.window.document.body.appendChild(container);
  const root = createRoot(container);
  const queryClient = new QueryClient({
    // gcTime 0: the default 5-minute cache timer would keep the test runner's
    // event loop alive long after the assertions finish.
    defaultOptions: {
      mutations: { gcTime: 0, retry: false },
      queries: { gcTime: 0, retry: false },
    },
  });
  act(() => {
    root.render(
      React.createElement(
        QueryClientProvider,
        { client: queryClient },
        React.createElement(Probe),
      ),
    );
  });
  return {
    latest,
    unmount() {
      act(() => root.unmount());
      container.remove();
      queryClient.clear();
    },
  };
}

const editInput = {
  id: "persona-1",
  displayName: "Bond",
  systemPrompt: "",
};

function payloadFor(command) {
  return calls.find((call) => call.command === command)?.args.input;
}

describe("persona edit avatar resolution", () => {
  afterEach(() => {
    calls.length = 0;
  });

  it("uploads a base64 avatar instead of storing the data URL", async () => {
    const mounted = mountMutation(useUpdatePersonaMutation);
    await act(async () => {
      await mounted.latest.current.mutateAsync({
        ...editInput,
        avatarUrl: AVATAR_DATA_URL,
      });
    });

    assert.ok(
      calls.some((call) => call.command === "upload_media_bytes"),
      "the data URL must be uploaded to relay media",
    );
    assert.equal(payloadFor("update_persona").avatarUrl, UPLOADED_URL);
    mounted.unmount();
  });

  it("uploads on the save-and-publish path too", async () => {
    const mounted = mountMutation(() =>
      useUpdatePersonaAndPublishMutation("community-1"),
    );
    await act(async () => {
      await mounted.latest.current.mutateAsync({
        ...editInput,
        avatarUrl: AVATAR_DATA_URL,
      });
    });

    assert.equal(
      payloadFor("update_persona_and_publish").avatarUrl,
      UPLOADED_URL,
    );
    mounted.unmount();
  });

  it("a failed upload rejects the save instead of clearing the avatar", async () => {
    uploadFails = true;
    const mounted = mountMutation(useUpdatePersonaMutation);
    let thrown = null;
    await act(async () => {
      await mounted.latest.current
        .mutateAsync({ ...editInput, avatarUrl: AVATAR_DATA_URL })
        .catch((error) => {
          thrown = error;
        });
    });

    // Rule 1: the failure propagates. Swallowing it and dropping the key would
    // be read downstream as "the user removed the avatar" — `avatar_url` is an
    // Option that a missing key fills with None, and `update_persona` assigns
    // it unconditionally and republishes every linked instance's kind:0.
    assert.ok(thrown instanceof Error, "mutateAsync must reject");
    assert.match(thrown.message, /relay media is offline/);
    assert.ok(
      !calls.some((call) => call.command === "update_persona"),
      "no persona write may happen once the avatar upload failed",
    );
    mounted.unmount();
    uploadFails = false;
  });

  it("the same failure never reaches the save-and-publish path either", async () => {
    uploadFails = true;
    const mounted = mountMutation(() =>
      useUpdatePersonaAndPublishMutation("community-1"),
    );
    let thrown = null;
    await act(async () => {
      await mounted.latest.current
        .mutateAsync({ ...editInput, avatarUrl: AVATAR_DATA_URL })
        .catch((error) => {
          thrown = error;
        });
    });

    assert.ok(thrown instanceof Error, "mutateAsync must reject");
    assert.ok(
      !calls.some((call) => call.command === "update_persona_and_publish"),
      "no persona write may happen once the avatar upload failed",
    );
    mounted.unmount();
    uploadFails = false;
  });

  it("leaves an https avatar alone and lets an absent one clear the stored avatar", async () => {
    const mounted = mountMutation(useUpdatePersonaMutation);
    await act(async () => {
      await mounted.latest.current.mutateAsync({
        ...editInput,
        avatarUrl: "https://relay.example/existing.png",
      });
    });
    assert.ok(!calls.some((call) => call.command === "upload_media_bytes"));
    assert.equal(
      payloadFor("update_persona").avatarUrl,
      "https://relay.example/existing.png",
    );

    calls.length = 0;
    await act(async () => {
      await mounted.latest.current.mutateAsync(editInput);
    });
    // Absent means CLEAR: that is how "Remove avatar" works. This resolver
    // must therefore pass an absent avatar through untouched and never
    // manufacture one out of a failure.
    assert.ok(!calls.some((call) => call.command === "upload_media_bytes"));
    assert.equal(payloadFor("update_persona").avatarUrl, undefined);
    mounted.unmount();
  });
});
