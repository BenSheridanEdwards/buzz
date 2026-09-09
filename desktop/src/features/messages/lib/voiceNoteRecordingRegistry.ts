/**
 * Tracks the one composer that is allowed to be recording a voice note.
 *
 * A window can show several composers at once — a channel composer with a
 * docked thread panel beside it, a forum reply, a drawer. Each one owns its
 * own recorder hook, so without a shared claim two microphones open at the
 * same time: two live rows, two announcements racing in the same window, and
 * an Escape that only ever reaches one of them. The microphone is a single
 * physical device and a message can carry one voice note, so recording is
 * modelled as a claim exactly one composer holds at a time.
 *
 * Shaped like `escapeSurfaces`: module state, because the composers are
 * siblings with no common owner, and community switching remounts the tree
 * rather than reloading the page. The claim is released by the holder's own
 * cleanup (idle, discard, unmount), and a stale release cannot clear a newer
 * holder's claim.
 */
type RecordingClaim = {
  discard: () => void;
  owner: string;
  token: number;
};

let claim: RecordingClaim | null = null;
let nextToken = 1;
const listeners = new Set<() => void>();

function emit() {
  for (const listener of [...listeners]) listener();
}

/** Subscribe to claim changes (for `useSyncExternalStore`). */
export function subscribeVoiceNoteRecording(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** The composer id currently recording, or null when none is. */
export function getVoiceNoteRecordingOwner(): string | null {
  return claim?.owner ?? null;
}

/**
 * Claims recording for `owner`. Returns a release function, or null when
 * another composer already holds the claim — the caller must not start.
 */
export function claimVoiceNoteRecording(
  owner: string,
  discard: () => void,
): (() => void) | null {
  if (claim !== null && claim.owner !== owner) return null;
  const token = nextToken;
  nextToken += 1;
  claim = { discard, owner, token };
  emit();
  return () => {
    // A release from a superseded claim (the same composer restarting) must
    // not clear the claim that replaced it.
    if (claim?.token !== token) return;
    claim = null;
    emit();
  };
}

/**
 * Discards the live recording wherever it is, and reports whether there was
 * one. Lets a sibling composer's Escape reach a recorder it does not own.
 */
export function discardActiveVoiceNoteRecording(): boolean {
  const active = claim;
  if (!active) return false;
  active.discard();
  return true;
}
