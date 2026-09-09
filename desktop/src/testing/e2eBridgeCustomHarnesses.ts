/**
 * In-memory custom harness store for the e2e bridge.
 *
 * Extracted as a separate module so the handler logic can be unit-tested
 * independently of the full e2eBridge.ts context (which requires a browser
 * environment and full Playwright setup).
 */
import { DEFAULT_AGENT_PARALLELISM } from "../features/agents/lib/agentParallelism.ts";
import type { RawAcpRuntimeCatalogEntry } from "../shared/api/tauri.ts";

/** In-memory store for custom harnesses saved via `save_custom_harness`. */
export const mockCustomHarnesses = new Map<string, RawAcpRuntimeCatalogEntry>();

/**
 * Ids removed via `delete_custom_harness` (or vacated by a rename).
 *
 * Needed because a test's `acpRuntimesCatalog` seed is static config, not the
 * mutation store: deleting a seeded row leaves nothing to remove from
 * `mockCustomHarnesses`, so without a tombstone the row would survive the
 * delete and the mock would report success while the UI still shows it.
 */
export const mockDeletedCustomHarnesses = new Set<string>();

/**
 * Command identities the Hermes preset answers to, mirroring Rust's
 * `preset_default_parallelism` over `normalize_command_identity`.
 */
const HERMES_COMMAND_IDENTITIES = new Set(["hermes", "hermes-acp"]);

/**
 * The per-harness default parallelism the backend derives from a command,
 * mirroring Rust's `harness_default_parallelism`.
 *
 * `save_custom_harness` in `commands/agent_discovery.rs` emits
 * `harness_default_parallelism(&definition.command)` for custom harnesses too,
 * so a custom harness wrapping `hermes-acp` comes back with 1, not the app
 * default. The identity fold matches `normalize_command_identity`: basename,
 * lowercased, `_`/space folded to `-`, and the Windows launcher suffixes
 * (`.exe` and npm's `.cmd`/`.bat`) stripped.
 */
export function mockHarnessDefaultParallelismForCommand(
  command: string | null | undefined,
): number {
  const basename = (command ?? "")
    .trim()
    .replace(/\\/g, "/")
    .replace(/\/+$/, "")
    .split("/")
    .pop();
  const identity = (basename ?? "")
    .toLowerCase()
    .replace(/[ _]/g, "-")
    .replace(/\.(exe|cmd|bat)$/, "");
  return HERMES_COMMAND_IDENTITIES.has(identity)
    ? 1
    : DEFAULT_AGENT_PARALLELISM;
}

/** The shape `mockHarnessDefaultParallelismFrom` matches catalogs on. */
type HarnessDefaultLookupEntry = {
  id: string;
  command?: string | null;
  default_parallelism?: number;
};

/**
 * The parallelism a blank create stores for `command`, mirroring Rust's
 * `mint_parallelism`: the matching catalog entry's `default_parallelism`, else
 * the app default.
 *
 * `declared` is the spec's static catalog config; the mutation store is
 * searched after it, because a harness a spec saved through
 * `handleSaveCustomHarness` exists only there. Without that second source a
 * create against a saved harness would mint the app default even though the
 * same mock had just advertised 1 for it.
 */
export function mockHarnessDefaultParallelismFrom(
  command: string,
  declared: readonly HarnessDefaultLookupEntry[],
): number {
  const matches = (runtime: HarnessDefaultLookupEntry) =>
    runtime.command === command || runtime.id === command;
  const entry =
    declared.find(matches) ?? [...mockCustomHarnesses.values()].find(matches);
  return entry?.default_parallelism ?? DEFAULT_AGENT_PARALLELISM;
}

/** Reset the store between tests. */
export function resetMockCustomHarnesses(): void {
  mockCustomHarnesses.clear();
  mockDeletedCustomHarnesses.clear();
}

/**
 * Overlay the mutation store onto a seeded catalog.
 *
 * Deleted ids drop out, saved ids replace their seeded entry in place (so a
 * same-id edit updates rather than duplicates), and newly added ids append.
 */
export function mergeMockCustomHarnesses(
  base: RawAcpRuntimeCatalogEntry[],
): RawAcpRuntimeCatalogEntry[] {
  const merged = base.filter(
    (entry) => !mockDeletedCustomHarnesses.has(entry.id),
  );
  for (const entry of mockCustomHarnesses.values()) {
    const index = merged.findIndex((existing) => existing.id === entry.id);
    if (index === -1) {
      merged.push(entry);
    } else {
      merged[index] = entry;
    }
  }
  return merged;
}

/**
 * Handle `save_custom_harness`.
 *
 * Persists the definition into `mockCustomHarnesses` so that the next
 * `discover_acp_providers` call includes it. Mirrors the Rust command's
 * return shape: an `AcpRuntimeCatalogEntry` for the saved harness.
 */
export function handleSaveCustomHarness(args: {
  definition?: {
    id?: string;
    label?: string;
    command?: string;
    args?: string[];
    env?: Record<string, string>;
    installInstructionsUrl?: string;
    installHint?: string;
  };
  originalId?: string | null;
}): RawAcpRuntimeCatalogEntry {
  const def = args.definition ?? {};
  const id = def.id ?? "";
  const originalId = args.originalId ?? null;

  // On rename: remove the old entry so the old id is no longer in the catalog.
  if (originalId && originalId !== id) {
    mockCustomHarnesses.delete(originalId);
    mockDeletedCustomHarnesses.add(originalId);
  }
  // A save resurrects an id that an earlier test step deleted.
  mockDeletedCustomHarnesses.delete(id);

  const entry: RawAcpRuntimeCatalogEntry = {
    id,
    label: def.label ?? id,
    avatar_url: "",
    availability: "not_installed", // PATH not probed in e2e mock
    command: def.command ?? null,
    binary_path: null,
    default_args: def.args ?? [],
    mcp_command: null,
    install_hint: def.installHint ?? "",
    install_instructions_url: def.installInstructionsUrl ?? "",
    can_auto_install: false,
    requires_external_cli: true,
    underlying_cli_path: null,
    node_required: false,
    auth_status: { status: "not_applicable" },
    source: "custom",
    // Keyed on the command, like the backend: `save_custom_harness` returns
    // `harness_default_parallelism(&definition.command)`, so a custom harness
    // wrapping `hermes-acp` (in any launcher spelling) advertises 1.
    default_parallelism: mockHarnessDefaultParallelismForCommand(def.command),
    // Omit definition_env when the env map is empty — mirrors Rust's BTreeMap
    // serialization which skips empty maps so the field is absent on the wire.
    definition_env:
      def.env && Object.keys(def.env).length > 0 ? def.env : undefined,
    login_hint: undefined,
  };
  mockCustomHarnesses.set(id, entry);
  return entry;
}

/**
 * Handle `delete_custom_harness`.
 * Removes the harness from the in-memory store. Idempotent (not-found is OK).
 * When `config?.mock?.deleteCustomHarnessError` is set, throws with that message
 * to exercise the UI's inline error path.
 */
export function handleDeleteCustomHarness(
  args: { id?: string },
  config?: { mock?: { deleteCustomHarnessError?: string } } | undefined,
): void {
  const errorMsg = config?.mock?.deleteCustomHarnessError;
  if (errorMsg) {
    throw new Error(errorMsg);
  }
  const id = args.id ?? "";
  mockCustomHarnesses.delete(id);
  mockDeletedCustomHarnesses.add(id);
}
