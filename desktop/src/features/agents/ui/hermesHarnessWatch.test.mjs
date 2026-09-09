/**
 * Timing seam for the instance dialog's Hermes harness watch.
 *
 * `AgentInstanceEditDialog` does not watch a user action — it watches
 * `harnessSelection`, which it derives from the runtime catalog and the
 * persona list. Those settle asynchronously, so the same Hermes agent renders
 * as cold Hermes (no runtime id yet), then as the app's default runtime (the
 * persona has not arrived, so `getDefaultPersonaRuntime` wins), then as Hermes
 * again. Two of those are transitions the user never made, and the middle one
 * looks exactly like "left Hermes": it drops `HERMES_HOME` and nothing
 * restores it.
 *
 * These tests mount the production `useHermesHarnessWatch` — the hook the
 * dialog calls — and drive that exact three-step settle. Deleting the
 * `settled` gate, or seeding the baseline from the first render instead of the
 * first settled selection, turns `holds the pin while the runtime catalog and
 * persona list are still settling` red. The last test keeps the gate honest:
 * a real harness change after the queries settle must still drop the pin.
 */

import assert from "node:assert/strict";
import { after, before, describe, it } from "node:test";
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

let React, act, createRoot, useHermesHarnessWatch;

before(async () => {
  ({ default: React, act } = await import("react"));
  ({ createRoot } = await import("react-dom/client"));
  ({ useHermesHarnessWatch } = await import("./useHermesHarnessWatch.ts"));
});

after(() => {
  dom.window.close();
});

const PROFILE_PATH = "/Users/me/.hermes/profiles/bond";

/** The three harness selections the dialog computes as its queries settle. */
const COLD_HERMES = { runtimeId: "", command: "hermes-acp" };
const INTERMEDIATE_DEFAULT = { runtimeId: "buzz-agent", command: "buzz-agent" };
const SETTLED_HERMES = { runtimeId: "hermes", command: "hermes-acp" };
const USER_PICKED_CLAUDE = { runtimeId: "claude-code", command: "claude" };

/**
 * Mount the watch over a draft the harness change can rewrite, exactly as the
 * dialog does: the draft is dialog state, `onChange` writes it back.
 */
function mountWatch({ harness, settled }) {
  const container = dom.window.document.createElement("div");
  dom.window.document.body.appendChild(container);
  const root = createRoot(container);
  const state = {
    draft: {
      displayName: "Bond",
      description: "",
      avatarUrl: "",
      systemPrompt:
        "Your SOUL, memory, skills and tools come from your Hermes profile. Follow them.",
      envVars: {
        HERMES_ACP_SKIP_CONFIGURED_MCP: "0",
        HERMES_HOME: PROFILE_PATH,
        MY_OWN: "keep me",
      },
      parallelism: "1",
    },
    changes: 0,
  };

  function Probe({ harness, settled }) {
    useHermesHarnessWatch({
      defaults: { parallelism: "4" },
      draft: state.draft,
      enabled: true,
      harness,
      onChange: (next) => {
        state.changes += 1;
        state.draft = next;
      },
      settled,
    });
    return null;
  }

  function render(props) {
    act(() => {
      root.render(React.createElement(Probe, props));
    });
  }

  render({ harness, settled });
  return {
    render,
    state,
    unmount() {
      act(() => root.unmount());
      container.remove();
    },
  };
}

describe("hermes harness watch timing", () => {
  it("holds the pin while the runtime catalog and persona list are still settling", () => {
    // Mount cold: no runtime id resolved yet, both queries loading.
    const mounted = mountWatch({ harness: COLD_HERMES, settled: false });

    // The persona has not arrived, so the dialog falls back to the app default
    // runtime. This is not a harness change; nobody touched the dropdown.
    mounted.render({ harness: INTERMEDIATE_DEFAULT, settled: false });
    assert.equal(
      mounted.state.draft.envVars.HERMES_HOME,
      PROFILE_PATH,
      "an unsettled intermediate selection must not drop the pin",
    );

    // Both queries land, and the selection resolves back to Hermes.
    mounted.render({ harness: SETTLED_HERMES, settled: true });
    assert.equal(mounted.state.draft.envVars.HERMES_HOME, PROFILE_PATH);
    assert.equal(
      mounted.state.draft.envVars.HERMES_ACP_SKIP_CONFIGURED_MCP,
      "0",
    );
    assert.equal(
      mounted.state.changes,
      0,
      "settling is not a user change: the draft must be untouched",
    );
    mounted.unmount();
  });

  it("still drops the pin when the user changes the harness after settling", () => {
    const mounted = mountWatch({ harness: COLD_HERMES, settled: false });
    mounted.render({ harness: INTERMEDIATE_DEFAULT, settled: false });
    mounted.render({ harness: SETTLED_HERMES, settled: true });

    // Now a real change: the user picks Claude Code from the dropdown.
    mounted.render({ harness: USER_PICKED_CLAUDE, settled: true });
    assert.equal(mounted.state.changes, 1);
    assert.equal(mounted.state.draft.envVars.HERMES_HOME, undefined);
    assert.equal(
      mounted.state.draft.envVars.HERMES_ACP_SKIP_CONFIGURED_MCP,
      undefined,
    );
    assert.equal(mounted.state.draft.envVars.MY_OWN, "keep me");
    assert.equal(mounted.state.draft.systemPrompt, "");
    assert.equal(mounted.state.draft.parallelism, "4");
    mounted.unmount();
  });

  it("treats the first settled selection as the baseline, not a change", () => {
    // A dialog that opens with everything already cached settles on its first
    // render. There is no previous selection to compare against, so a
    // non-Hermes agent must not be seen as "just left Hermes".
    const mounted = mountWatch({ harness: SETTLED_HERMES, settled: true });
    assert.equal(mounted.state.changes, 0);

    mounted.render({ harness: USER_PICKED_CLAUDE, settled: true });
    assert.equal(mounted.state.changes, 1);
    assert.equal(mounted.state.draft.envVars.HERMES_HOME, undefined);
    mounted.unmount();
  });

  it("re-baselines after the inputs go back to loading", () => {
    const mounted = mountWatch({ harness: SETTLED_HERMES, settled: true });
    // A refetch un-settles the inputs and the derived selection drifts to the
    // app default while the persona list is back in flight.
    mounted.render({ harness: INTERMEDIATE_DEFAULT, settled: false });
    // It settles there for one pass before the persona lands. Compared against
    // the pre-refetch baseline that reads as "left Hermes". It is not: nothing
    // observed across an unsettled gap may be treated as a user change.
    mounted.render({ harness: INTERMEDIATE_DEFAULT, settled: true });
    assert.equal(mounted.state.changes, 0);
    assert.equal(mounted.state.draft.envVars.HERMES_HOME, PROFILE_PATH);

    mounted.render({ harness: SETTLED_HERMES, settled: true });
    assert.equal(mounted.state.changes, 0);
    assert.equal(mounted.state.draft.envVars.HERMES_HOME, PROFILE_PATH);
    mounted.unmount();
  });
});
