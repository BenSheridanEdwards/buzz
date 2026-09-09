import { invokeTauri } from "@/shared/api/tauri";

/**
 * One locally installed Hermes Agent profile (`~/.hermes/profiles/<slug>`),
 * as listed by the `list_hermes_profiles` Tauri command.
 */
export type HermesProfile = {
  /** Directory name under the profiles root. */
  slug: string;
  /** Display name from the SOUL heading, or a title-cased slug. */
  name: string;
  /** Short description from a `Title:` line in the SOUL, when present. */
  description: string | null;
  /** Absolute profile directory; becomes `HERMES_HOME` on the spawned agent. */
  path: string;
  /** Inline `data:image/...;base64,` avatar when the profile ships one. */
  avatarDataUrl: string | null;
};

type RawHermesProfile = {
  slug: string;
  name: string;
  description?: string | null;
  path: string;
  avatar_data_url?: string | null;
};

export function fromRawHermesProfile(raw: RawHermesProfile): HermesProfile {
  return {
    slug: raw.slug,
    name: raw.name,
    description: raw.description ?? null,
    path: raw.path,
    avatarDataUrl: raw.avatar_data_url ?? null,
  };
}

/** List the Hermes profiles installed on this machine (empty when none). */
export async function listHermesProfiles(): Promise<HermesProfile[]> {
  const raw = await invokeTauri<RawHermesProfile[]>("list_hermes_profiles");
  return raw.map(fromRawHermesProfile);
}
