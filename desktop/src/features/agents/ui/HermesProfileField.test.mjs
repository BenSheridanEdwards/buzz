/**
 * Mounted seam tests for the Hermes profile picker.
 *
 * The hook test drives the production `useHermesProfilePicker` through a real
 * QueryClientProvider against a stubbed `list_hermes_profiles` IPC command, so
 * a rename of the command, a break in the snake_case mapping, or a regression
 * in the pick-to-draft seeding turns this suite red. The render tests pin the
 * field's read-only path line and its empty/error affordances.
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

const invocations = [];
let listHandler = () => Promise.resolve([]);

globalThis.__TAURI_INTERNALS__ = {
  invoke: (command, args) => {
    invocations.push(command);
    if (command === "list_hermes_profiles") return listHandler(args);
    return Promise.reject(new Error(`unmocked: ${command}`));
  },
  transformCallback: () => 1,
};
dom.window.__TAURI_INTERNALS__ = globalThis.__TAURI_INTERNALS__;

const rawBond = {
  slug: "bond",
  name: "Bond",
  description: "Executor of the Fleet",
  path: "/Users/me/.hermes/profiles/bond",
  avatar_data_url: "data:image/jpeg;base64,/9j/4A==",
};

// ── Deferred imports ──────────────────────────────────────────────────────────

let React,
  act,
  createRoot,
  renderToStaticMarkup,
  QueryClient,
  QueryClientProvider,
  HermesProfileField,
  useHermesProfilePicker,
  HERMES_PROFILE_DEFAULT_INSTRUCTIONS;

before(async () => {
  ({ default: React, act } = await import("react"));
  ({ createRoot } = await import("react-dom/client"));
  ({ renderToStaticMarkup } = await import("react-dom/server"));
  ({ QueryClient, QueryClientProvider } = await import(
    "@tanstack/react-query"
  ));
  ({ HermesProfileField, useHermesProfilePicker } = await import(
    "./HermesProfileField.tsx"
  ));
  ({ HERMES_PROFILE_DEFAULT_INSTRUCTIONS } = await import(
    "./hermesProfileSelection.ts"
  ));
});

after(() => {
  dom.window.close();
});

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

/** Mount the production hook and expose its latest return value. */
function mountPicker({
  defaultParallelism = "",
  draft,
  enabled,
  inheritedEnvVars,
  onApply,
}) {
  const latest = { current: null };
  function Probe() {
    latest.current = useHermesProfilePicker({
      defaultParallelism,
      draft,
      enabled,
      inheritedEnvVars,
      onApply,
    });
    return React.createElement("span", null, latest.current.status);
  }
  const container = dom.window.document.createElement("div");
  dom.window.document.body.appendChild(container);
  const root = createRoot(container);
  const queryClient = new QueryClient({
    // gcTime 0: the default 5-minute cache timer would keep the test runner's
    // event loop alive long after the assertions finish.
    defaultOptions: { queries: { gcTime: 0, retry: false } },
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
    async settle() {
      for (let i = 0; i < 5; i += 1) {
        await act(async () => {
          await flush();
        });
      }
    },
    unmount() {
      act(() => root.unmount());
      container.remove();
      queryClient.clear();
    },
  };
}

const emptyDraft = {
  displayName: "",
  description: "",
  avatarUrl: "",
  systemPrompt: "",
  envVars: { OPENAI_API_KEY: "sk" },
  parallelism: "",
};

describe("useHermesProfilePicker", () => {
  afterEach(() => {
    invocations.length = 0;
    listHandler = () => Promise.resolve([]);
  });

  it("lists profiles through list_hermes_profiles and seeds the draft on pick", async () => {
    listHandler = () => Promise.resolve([rawBond]);
    const applied = [];
    const mounted = mountPicker({
      draft: emptyDraft,
      enabled: true,
      onApply: (next) => applied.push(next),
    });
    await mounted.settle();

    assert.deepEqual(invocations, ["list_hermes_profiles"]);
    assert.equal(mounted.latest.current.status, "ready");
    assert.deepEqual(
      mounted.latest.current.profiles.map((profile) => profile.avatarDataUrl),
      [rawBond.avatar_data_url],
      "the IPC snake_case avatar must reach the picker as avatarDataUrl",
    );

    const [bond] = mounted.latest.current.profiles;
    act(() => mounted.latest.current.handleProfileChange(bond));
    assert.equal(applied.length, 1);
    assert.deepEqual(applied[0], {
      displayName: "Bond",
      description: "Executor of the Fleet",
      avatarUrl: rawBond.avatar_data_url,
      systemPrompt: HERMES_PROFILE_DEFAULT_INSTRUCTIONS,
      envVars: {
        OPENAI_API_KEY: "sk",
        HERMES_HOME: rawBond.path,
        HERMES_ACP_SKIP_CONFIGURED_MCP: "0",
      },
      parallelism: "1",
    });
    mounted.unmount();
  });

  it("clears only the pin when No profile is picked", async () => {
    listHandler = () => Promise.resolve([rawBond]);
    const applied = [];
    const mounted = mountPicker({
      draft: {
        ...emptyDraft,
        displayName: "Bond",
        envVars: {
          OPENAI_API_KEY: "sk",
          HERMES_HOME: rawBond.path,
          HERMES_ACP_SKIP_CONFIGURED_MCP: "0",
        },
        parallelism: "1",
      },
      enabled: true,
      onApply: (next) => applied.push(next),
    });
    await mounted.settle();

    act(() => mounted.latest.current.handleProfileChange(null));
    assert.equal(applied.length, 1);
    assert.deepEqual(applied[0].envVars, { OPENAI_API_KEY: "sk" });
    // Identity is the agent's now; the seeded run settings are not, and must
    // not outlive the profile that explained them.
    assert.equal(applied[0].displayName, "Bond");
    assert.equal(applied[0].parallelism, "");
    mounted.unmount();
  });

  it("shows the definition's pin and writes no override for it", async () => {
    listHandler = () => Promise.resolve([rawBond]);
    const applied = [];
    const inheritedEnvVars = {
      HERMES_HOME: rawBond.path,
      HERMES_ACP_SKIP_CONFIGURED_MCP: "0",
    };
    const mounted = mountPicker({
      // The instance layer is empty: instances are never seeded from their
      // definition, so reading it alone would report "No profile".
      draft: { ...emptyDraft, envVars: {} },
      enabled: true,
      inheritedEnvVars,
      onApply: (next) => applied.push(next),
    });
    await mounted.settle();

    assert.equal(mounted.latest.current.isInherited, true);
    assert.equal(mounted.latest.current.inheritedPath, rawBond.path);
    assert.equal(
      mounted.latest.current.effectiveEnvVars.HERMES_HOME,
      rawBond.path,
    );

    // Re-picking the inherited profile is not a change, so no override lands.
    const [bond] = mounted.latest.current.profiles;
    act(() => mounted.latest.current.handleProfileChange(bond));
    assert.deepEqual(applied[0].envVars, {});
    mounted.unmount();
  });

  it("falls back to the definition's profile without clearing its seeding", async () => {
    listHandler = () => Promise.resolve([rawBond]);
    const applied = [];
    const mounted = mountPicker({
      defaultParallelism: "4",
      draft: {
        ...emptyDraft,
        envVars: { HERMES_HOME: "/Users/me/.hermes/profiles/sky" },
        parallelism: "1",
        systemPrompt: HERMES_PROFILE_DEFAULT_INSTRUCTIONS,
      },
      enabled: true,
      inheritedEnvVars: { HERMES_HOME: rawBond.path },
      onApply: (next) => applied.push(next),
    });
    await mounted.settle();

    act(() => mounted.latest.current.handleProfileChange(null));
    // The override goes; the definition's profile is still in effect, so the
    // instructions and parallelism it explains stay.
    assert.deepEqual(applied[0].envVars, {});
    assert.equal(applied[0].parallelism, "1");
    assert.equal(applied[0].systemPrompt, HERMES_PROFILE_DEFAULT_INSTRUCTIONS);
    mounted.unmount();
  });

  it("writes an instance override only when the pick differs", async () => {
    const rawSky = {
      slug: "sky",
      name: "Sky",
      path: "/Users/me/.hermes/profiles/sky",
    };
    listHandler = () => Promise.resolve([rawBond, rawSky]);
    const applied = [];
    const mounted = mountPicker({
      draft: { ...emptyDraft, envVars: {} },
      enabled: true,
      inheritedEnvVars: { HERMES_HOME: rawBond.path },
      onApply: (next) => applied.push(next),
    });
    await mounted.settle();

    const sky = mounted.latest.current.profiles.find(
      (profile) => profile.slug === "sky",
    );
    act(() => mounted.latest.current.handleProfileChange(sky));
    assert.deepEqual(applied[0].envVars, {
      HERMES_HOME: rawSky.path,
      HERMES_ACP_SKIP_CONFIGURED_MCP: "0",
    });
    mounted.unmount();
  });

  it("never touches the disk for a non-Hermes harness", async () => {
    listHandler = () => Promise.resolve([rawBond]);
    const mounted = mountPicker({
      draft: emptyDraft,
      enabled: false,
      onApply: () => {},
    });
    await mounted.settle();
    assert.deepEqual(invocations, []);
    mounted.unmount();
  });

  it("reports an error status when the scan fails", async () => {
    listHandler = () => Promise.reject(new Error("boom"));
    const mounted = mountPicker({
      draft: emptyDraft,
      enabled: true,
      onApply: () => {},
    });
    await mounted.settle();
    assert.equal(mounted.latest.current.status, "error");
    assert.deepEqual(mounted.latest.current.profiles, []);
    mounted.unmount();
  });
});

describe("HermesProfileField", () => {
  const bond = {
    slug: "bond",
    name: "Bond",
    description: "Executor of the Fleet",
    path: "/Users/me/.hermes/profiles/bond",
    avatarDataUrl: null,
  };

  function render(overrides = {}) {
    return renderToStaticMarkup(
      React.createElement(HermesProfileField, {
        disabled: false,
        envVars: {},
        onProfileChange: () => {},
        profiles: [bond],
        status: "ready",
        ...overrides,
      }),
    );
  }

  it("describes the control with its help, path and error text", () => {
    const html = render({
      envVars: { HERMES_HOME: bond.path },
      status: "error",
    });
    assert.ok(
      html.includes(
        'aria-describedby="persona-hermes-profile-error persona-hermes-profile-path persona-hermes-profile-help"',
      ),
      html,
    );
    assert.ok(html.includes('role="alert"'));
  });

  it("describes the control with the empty-state line too", () => {
    // "No profiles found in ~/.hermes/profiles." is the only thing on screen
    // explaining why the dropdown has nothing in it; leaving it out of
    // aria-describedby means it is never announced.
    const html = render({ profiles: [] });
    assert.ok(
      html.includes(
        'aria-describedby="persona-hermes-profile-empty persona-hermes-profile-help"',
      ),
      html,
    );
    assert.ok(html.includes('id="persona-hermes-profile-empty"'));
  });

  it("renders an inherited pin read-only with a way back to the definition", () => {
    let edited = 0;
    const html = renderToStaticMarkup(
      React.createElement(HermesProfileField, {
        disabled: false,
        envVars: { HERMES_HOME: bond.path },
        inherited: {
          isInherited: true,
          onEditDefinition: () => {
            edited += 1;
          },
          path: bond.path,
        },
        onProfileChange: () => {},
        profiles: [bond],
        status: "ready",
      }),
    );
    // An instance override cannot unset an inherited env var, so the control is
    // read-only and the recovery affordance points at the definition.
    //
    // `aria-disabled`, not `disabled`: a disabled button is not focusable, so
    // a screen reader never reaches it and never announces the explanation
    // below it or the route out. The control stays in the tab order and inert.
    assert.ok(html.includes('aria-disabled="true"'), html);
    assert.ok(!html.includes('disabled=""'), html);
    assert.ok(
      html.includes(
        'aria-describedby="persona-hermes-profile-inherited persona-hermes-profile-path persona-hermes-profile-help"',
      ),
      html,
    );
    assert.ok(html.includes("Set by this agent&#x27;s definition."));
    assert.ok(html.includes("Edit definition"));
    assert.equal(edited, 0);
  });

  it("labels the control and shows the pinned directory read-only", () => {
    const html = render({ envVars: { HERMES_HOME: bond.path } });
    assert.ok(html.includes('for="persona-hermes-profile"'));
    assert.ok(html.includes('id="persona-hermes-profile"'));
    assert.ok(html.includes("Hermes profile"));
    assert.ok(html.includes('data-testid="hermes-profile-path"'));
    assert.ok(html.includes(bond.path));
    // The pinned profile's name is the trigger text.
    assert.ok(html.includes(">Bond<"));
  });

  it("uses the caller's id so two forms never collide", () => {
    const html = render({ id: "edit-agent-hermes-profile" });
    assert.ok(html.includes('id="edit-agent-hermes-profile"'));
    assert.ok(!html.includes("persona-hermes-profile"));
  });

  it("explains an empty scan and a failed scan", () => {
    assert.ok(render({ profiles: [] }).includes("No profiles found"));
    const failed = render({ profiles: [], status: "error" });
    assert.ok(failed.includes("Could not read Hermes profiles"));
    assert.ok(!failed.includes("No profiles found"));
  });

  it("omits the directory line when nothing is pinned", () => {
    assert.ok(!render().includes('data-testid="hermes-profile-path"'));
  });
});
