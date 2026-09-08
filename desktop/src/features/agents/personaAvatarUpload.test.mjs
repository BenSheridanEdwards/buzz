/**
 * A persona edit must never carry a base64 avatar to the backend.
 *
 * `update_persona` copies the persona's avatar into every linked instance and
 * republishes their kind:0 `picture`; the relay rejects any event content over
 * 256 KiB and the persona sync sweep then retries the rejected head every 30 s
 * forever. Create already uploads data URLs to relay media, and a profile-
 * picked avatar arrives as exactly such a data URL, so edit must too.
 *
 * These tests drive the production mutation hooks through a real
 * QueryClientProvider against a stubbed Tauri bridge: deleting the
 * `personaInputWithResolvedAvatar` call in either mutationFn leaves the data
 * URL in the `update_persona` / `update_persona_and_publish` payload and fails
 * here.
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
    defaultOptions: { mutations: { retry: false }, queries: { retry: false } },
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

  it("leaves an https avatar and an absent avatar alone", async () => {
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
    // Absent means "leave the stored avatar alone", not "clear it".
    assert.equal(payloadFor("update_persona").avatarUrl, undefined);
    mounted.unmount();
  });
});
