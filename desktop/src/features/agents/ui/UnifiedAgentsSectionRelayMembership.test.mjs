/**
 * Rule 6 regression: the ONLY recovery affordance for a relay that will not
 * admit the user must be reachable in the running app.
 *
 * The npub, the `buzz-admin add-member` command and the grouped "this holds
 * up N agents" sentence all lived in `RelayMembershipBlock`, which lived in
 * `ManagedAgentRow`, which was imported by `AgentGroupRows`, which nothing in
 * the app imported at all: an earlier commit deleted every usage of that row
 * and left the two files behind. So the entire operator remedy was
 * unreachable in the shipped build, and the only thing a user could actually
 * see was the badge on the card, which names the problem and offers no way
 * out of it.
 *
 * A pure test over the notice helpers cannot see that: the helpers were
 * correct the whole time. Only mounting the section the user really sees can,
 * so this renders `UnifiedAgentsSection` with the mock Tauri bridge and
 * asserts the command text reaches the DOM. Deleting the render from
 * `UnifiedAgentsSection` fails here.
 *
 * It covers BOTH subjects, because the first fix rehomed only the workspace
 * half: the row component that carried the agent half was deleted, its helper
 * was left with no caller, and an agent-subject refusal, the ordinary "you
 * are a relay member but not an admin" case, went back to a badge and nothing
 * else while a pure ownership test went on passing. An assertion that some
 * helper returns a command proves nothing here; only the DOM does.
 *
 * It also pins the multi-identity case end to end, because that is where the
 * grouped notice was silently dropping agents.
 */

import assert from "node:assert/strict";
import { after, afterEach, before, test } from "node:test";

import { JSDOM } from "jsdom";

const dom = new JSDOM("<!doctype html><html><body></body></html>", {
  url: "http://localhost",
});

const clients = [];
const ipcHandlers = new Map();

let act;
let cleanup;
let render;
let screen;
let createElement;
let QueryClient;
let QueryClientProvider;
let UnifiedAgentsSection;
let useAgentAvailabilityLookup;
let relayAddMemberCommand;

const SELF_PK = "c".repeat(64);
const AGENT_A = "a".repeat(64);
const AGENT_B = "b".repeat(64);
const AGENT_C = "d".repeat(64);
/** The workspace identity the relay turned away at the roster read. */
const USER_HEX =
  "d00d00d00d00000000000000000000000000000000000000000000000000face";
/** A second workspace identity, refused independently. */
const OTHER_USER_HEX =
  "cafebabe00000000000000000000000000000000000000000000000000001234";

function refusedByRelay(subjectPubkey) {
  return {
    state: "not_member",
    checkedAt: "2026-09-09T00:00:00Z",
    detail: "This relay only accepts members and it did not accept you.",
    subjectPubkey,
  };
}

function agent(pubkey, overrides = {}) {
  return {
    pubkey,
    name: `Agent ${pubkey.slice(0, 2)}`,
    personaId: null,
    status: "stopped",
    model: null,
    modelSource: "global",
    lastError: null,
    lastErrorCode: null,
    needsRestart: false,
    personaOrphaned: false,
    ...overrides,
  };
}

function baseProps(overrides = {}) {
  return {
    defaultModel: "gpt-x",
    actionErrorMessage: null,
    actionNoticeMessage: null,
    agents: [],
    agentsError: null,
    isActionPending: false,
    isAgentsLoading: false,
    restartingAgentPubkey: null,
    startingAgentPubkey: null,
    startingPersonaIds: new Set(),
    onOpenAgentProfile: () => {},
    onOpenPersonaProfile: () => {},
    onRestartAgent: () => {},
    onStartAgent: () => {},
    onStartPersona: () => {},
    personas: [],
    personasError: null,
    personaFeedbackErrorMessage: null,
    personaFeedbackNoticeMessage: null,
    isPersonasLoading: false,
    isPersonasPending: false,
    onOpenCatalog: () => {},
    onDuplicatePersona: () => {},
    onEditPersona: () => {},
    onSharePersona: () => {},
    onDeactivatePersona: () => {},
    onDeletePersona: () => {},
    ...overrides,
  };
}

function Surface(props) {
  const { getAvailability } = useAgentAvailabilityLookup(
    props.agents.map((a) => a.pubkey),
  );
  return createElement(UnifiedAgentsSection, { ...props, getAvailability });
}

function renderSection(props) {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, gcTime: 0 },
      mutations: { gcTime: 0 },
    },
  });
  clients.push(client);
  return render(
    createElement(
      QueryClientProvider,
      { client },
      createElement(Surface, props),
    ),
  );
}

before(async () => {
  Object.assign(globalThis, {
    document: dom.window.document,
    HTMLElement: dom.window.HTMLElement,
    window: dom.window,
    IS_REACT_ACT_ENVIRONMENT: true,
  });
  Object.defineProperty(globalThis, "navigator", {
    configurable: true,
    value: dom.window.navigator,
    writable: true,
  });
  dom.window.matchMedia = () => ({
    matches: true,
    addEventListener() {},
    removeEventListener() {},
  });
  dom.window.__TAURI_INTERNALS__ = {
    invoke: (cmd, args) => {
      const handler = ipcHandlers.get(cmd);
      if (handler) return handler(args);
      return Promise.reject(new Error(`unmocked Tauri command: ${cmd}`));
    },
    transformCallback: () => Math.random(),
  };

  ({ act, cleanup, render, screen } = await import("@testing-library/react"));
  ({ createElement } = await import("react"));
  ({ QueryClient, QueryClientProvider } = await import(
    "@tanstack/react-query"
  ));
  ({ UnifiedAgentsSection } = await import("./UnifiedAgentsSection.tsx"));
  ({ useAgentAvailabilityLookup } = await import(
    "../lib/useAgentAvailability.ts"
  ));
  ({ relayAddMemberCommand } = await import("../lib/relayMembership.ts"));
});

afterEach(() => {
  cleanup?.();
  for (const client of clients.splice(0)) {
    client.cancelQueries();
    client.clear();
  }
  ipcHandlers.clear();
});

after(() => dom.window.close());

function installIpc() {
  ipcHandlers.set("get_identity", () =>
    Promise.resolve({ pubkey: SELF_PK, display_name: "Me" }),
  );
  ipcHandlers.set("list_archived_identities", () => Promise.resolve([]));
  ipcHandlers.set("get_user_profile", () =>
    Promise.resolve({
      pubkey: AGENT_A,
      display_name: null,
      avatar_url: null,
      about: null,
      nip05_handle: null,
      owner_pubkey: null,
    }),
  );
}

test("the operator command for a refused user reaches the rendered app", async () => {
  installIpc();

  await act(async () => {
    renderSection(
      baseProps({
        agents: [
          agent(AGENT_A, { relayMembership: refusedByRelay(USER_HEX) }),
          agent(AGENT_B, { relayMembership: refusedByRelay(USER_HEX) }),
        ],
      }),
    );
  });

  // The remedy itself, verbatim from the production builder. A block rendered
  // by a component nothing mounts puts none of this in the document.
  const command = relayAddMemberCommand(USER_HEX);
  assert.ok(
    screen.getByText(command),
    "the buzz-admin command must be reachable in the running app",
  );
  assert.ok(
    screen.getByText("Your npub"),
    "the npub the operator needs must be labelled and shown",
  );
  assert.ok(
    screen.getByText(
      "This holds up 2 agents on this relay. Clearing it clears all of them.",
    ),
    "one block stands in for the group, and says how many it holds up",
  );
  // One block for one problem, not one per agent.
  assert.equal(
    screen.getAllByTestId("managed-agent-relay-membership-blocked").length,
    1,
  );
});

test("every refused identity gets its own block, not just the largest", async () => {
  installIpc();

  await act(async () => {
    renderSection(
      baseProps({
        agents: [
          agent(AGENT_A, { relayMembership: refusedByRelay(USER_HEX) }),
          agent(AGENT_B, { relayMembership: refusedByRelay(USER_HEX) }),
          agent(AGENT_C, { relayMembership: refusedByRelay(OTHER_USER_HEX) }),
        ],
      }),
    );
  });

  assert.equal(
    screen.getAllByTestId("managed-agent-relay-membership-blocked").length,
    2,
    "two refused identities are two problems with two remedies",
  );
  // The smaller group is the one a largest-only helper dropped, taking its
  // agent's only way out with it.
  assert.ok(screen.getByText(relayAddMemberCommand(OTHER_USER_HEX)));
  assert.ok(screen.getByText(relayAddMemberCommand(USER_HEX)));
});

// The case a non-admin member actually hits: the relay lists the USER as a
// plain member and does not list the agent, so it answers about the agent and
// `subject_pubkey` is absent. This rendered a badge and nothing else for a
// whole round while the helper that knew the remedy sat with no caller.
test("the operator command for a refused agent reaches the rendered app", async () => {
  installIpc();

  await act(async () => {
    renderSection(
      baseProps({
        agents: [
          agent(AGENT_A, {
            relayMembership: {
              state: "not_member",
              checkedAt: "2026-09-09T00:00:00Z",
              detail: "the relay does not list this agent",
            },
          }),
        ],
      }),
    );
  });

  assert.ok(
    screen.getByText(relayAddMemberCommand(AGENT_A)),
    "the agent's own buzz-admin command must be reachable in the running app",
  );
  assert.ok(
    screen.getByText("Agent npub"),
    "the npub the operator needs is the agent's, and it must be labelled so",
  );
  assert.ok(
    screen.getByText("the relay does not list this agent"),
    "the relay's own sentence must reach the card, not just the badge",
  );
  assert.ok(
    screen.getByText(`Agent: ${agent(AGENT_A).name}`),
    "one block per blocked agent, so each must say which agent it is for",
  );
  // The badge alone is the dead end this test exists to prevent.
  assert.ok(
    screen.getByTestId(`agent-relay-membership-blocked-${AGENT_A}`),
    "the card still badges the problem",
  );
  assert.equal(
    screen.getAllByTestId("managed-agent-relay-membership-blocked").length,
    1,
  );
});

// The two subjects are two problems with two npubs and two remedies. Mixing
// them on one screen must produce one block each, never one card speaking for
// both and never one of them swallowed.
test("an agent refusal and a user refusal each keep their own remedy", async () => {
  installIpc();

  await act(async () => {
    renderSection(
      baseProps({
        agents: [
          agent(AGENT_A, { relayMembership: refusedByRelay(USER_HEX) }),
          agent(AGENT_B, { relayMembership: refusedByRelay(USER_HEX) }),
          agent(AGENT_C, {
            relayMembership: {
              state: "not_member",
              checkedAt: "2026-09-09T00:00:00Z",
              detail: "the relay does not list this agent",
            },
          }),
        ],
      }),
    );
  });

  assert.equal(
    screen.getAllByTestId("managed-agent-relay-membership-blocked").length,
    2,
  );
  assert.ok(screen.getByText(relayAddMemberCommand(USER_HEX)));
  assert.ok(screen.getByText(relayAddMemberCommand(AGENT_C)));
  assert.ok(screen.getByText("Your npub"));
  assert.ok(screen.getByText("Agent npub"));
  // The count sentence belongs to the grouped user-level refusal only: the
  // agent block stands for exactly one agent and must not claim otherwise.
  assert.equal(
    screen.getAllByText(
      "This holds up 2 agents on this relay. Clearing it clears all of them.",
    ).length,
    1,
  );
});
