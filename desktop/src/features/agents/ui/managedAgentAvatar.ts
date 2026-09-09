type BlobDescriptor = {
  url: string;
  sha256: string;
  size: number;
  type: string;
  uploaded: number;
};

export type UploadMediaBytes = (
  data: number[],
  filename?: string,
) => Promise<BlobDescriptor>;

/**
 * A persona edit whose avatar the relay will actually accept.
 *
 * Create already uploads a base64 data URL to relay media and stores the https
 * URL. Edit must do the same: `update_persona` copies the persona's
 * `avatar_url` into every linked instance and republishes their kind:0
 * `picture`, and the relay rejects any event content over 256 KiB — a rejected
 * persona head then sits in the sync queue and is retried every 30 s forever.
 * An avatar picked from a Hermes profile is exactly such a data URL.
 *
 * **On a persona update, an absent `avatarUrl` means CLEAR the stored avatar,
 * not "leave it alone."** `UpdatePersonaRequest.avatar_url` is an
 * `Option<String>` that serde fills with `None` for a missing key, and
 * `update.rs` assigns it unconditionally; the definition dialog's "Remove
 * avatar" affordance is exactly `setAvatarUrl("")` plus
 * `avatarUrl: avatarUrl.trim() || undefined` on submit. That is the contract,
 * and it is why this function must never *manufacture* an absent avatar: a
 * failed upload that returned `undefined` here would read downstream as a
 * deliberate removal and wipe the persona's face and every linked instance's,
 * reporting success. So the upload failure propagates (rule 1) and the dialog
 * shows it. `resolveManagedAgentAvatarUrl`'s swallow-and-fall-back branch
 * exists for create, which has a runtime avatar to fall back to and no stored
 * avatar to destroy; edit has neither.
 *
 * An input that carries no `avatarUrl` key, or one that is already an https or
 * inline-SVG emoji URL, is passed through untouched.
 */
export async function personaInputWithResolvedAvatar<
  T extends { avatarUrl?: string },
>(input: T, upload: UploadMediaBytes = defaultUploadMediaBytes): Promise<T> {
  const avatarUrl = input.avatarUrl?.trim() || undefined;
  // Emoji avatars are inline percent-encoded SVG, not base64: nothing to
  // upload, and `atob` would throw on them.
  if (!avatarUrl?.startsWith("data:image/") || !isBase64DataUri(avatarUrl)) {
    return input;
  }
  return { ...input, avatarUrl: await uploadAvatarDataUrl(avatarUrl, upload) };
}

export async function resolveManagedAgentAvatarUrl(
  avatarUrl: string | null | undefined,
  upload: UploadMediaBytes = defaultUploadMediaBytes,
  fallbackAvatarUrl?: string | null,
): Promise<string | undefined> {
  const resolvedAvatarUrl = avatarUrl?.trim() || undefined;
  if (!resolvedAvatarUrl?.startsWith("data:image/")) {
    return resolvedAvatarUrl;
  }

  // Emoji avatars are stored as inline, percent-encoded SVG data URLs
  // (`data:image/svg+xml,%3C...`) — the same self-contained form profile
  // persists. They are not base64 and must not be run through `atob`/upload;
  // pass them through unchanged so the emoji survives agent creation.
  if (!isBase64DataUri(resolvedAvatarUrl)) {
    return resolvedAvatarUrl;
  }

  // Create only: the caller supplies the runtime's own avatar as a fallback and
  // there is no stored avatar to lose, so a failed upload degrades to the
  // harness icon rather than blocking the create. Never reuse this on an
  // update path — see `personaInputWithResolvedAvatar`.
  try {
    return await uploadAvatarDataUrl(resolvedAvatarUrl, upload);
  } catch {
    return safeFallbackAvatarUrl(fallbackAvatarUrl);
  }
}

/**
 * Upload a base64 `data:image/...` avatar to relay media, returning its https
 * URL. Throws when the payload is malformed or the upload fails; callers that
 * can afford to degrade catch it, callers that would otherwise destroy stored
 * state must not.
 */
async function uploadAvatarDataUrl(
  dataUrl: string,
  upload: UploadMediaBytes,
): Promise<string> {
  const [, b64] = dataUrl.split(",", 2);
  if (!b64) {
    throw new Error("empty data URI payload");
  }
  const bytes = Array.from(atob(b64), (char) => char.charCodeAt(0));
  const blob = await upload(bytes);
  return blob.url;
}

async function defaultUploadMediaBytes(data: number[], filename?: string) {
  const { uploadMediaBytes } = await import("@/shared/api/tauri");
  return uploadMediaBytes(data, filename);
}

function isBase64DataUri(dataUri: string) {
  const header = dataUri.slice(0, dataUri.indexOf(","));
  return header.includes(";base64");
}

function safeFallbackAvatarUrl(avatarUrl: string | null | undefined) {
  const trimmed = avatarUrl?.trim() || undefined;
  return trimmed?.startsWith("data:image/") ? undefined : trimmed;
}
