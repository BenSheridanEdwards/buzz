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
 * An input that carries no `avatarUrl` at all is returned untouched: absent
 * means "leave the stored avatar alone", which is not the same as clearing it.
 */
export async function personaInputWithResolvedAvatar<
  T extends { avatarUrl?: string },
>(input: T, upload?: UploadMediaBytes): Promise<T> {
  if (input.avatarUrl === undefined) {
    return input;
  }
  const avatarUrl = await resolveManagedAgentAvatarUrl(input.avatarUrl, upload);
  return avatarUrl === input.avatarUrl ? input : { ...input, avatarUrl };
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

  try {
    const [, b64] = resolvedAvatarUrl.split(",", 2);
    if (!b64) {
      throw new Error("empty data URI payload");
    }
    const bytes = Array.from(atob(b64), (char) => char.charCodeAt(0));
    const blob = await upload(bytes);
    return blob.url;
  } catch {
    return safeFallbackAvatarUrl(fallbackAvatarUrl);
  }
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
