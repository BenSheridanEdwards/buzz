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
 * cleanup (idle, discard, failed start, unmount), and a stale release cannot
 * clear a newer holder's claim.
 *
 * Scope: one webview, deliberately. Module state dies with the realm, so the
 * claim cannot outlive the composers it describes, and the symptoms it exists
 * to prevent (two rows on one screen, two live regions announcing into one
 * screen-reader context, one Escape key that can only reach one of them) are
 * all properties of a single document. The huddle companion window is a
 * second webview on `index.html` with its own registry, so it can record
 * alongside the main window; see the PR discussion for why a process-wide
 * claim is a separate change rather than a bigger constant here.
 */
type RecordingClaim = {
  /** Discards the live recording, reporting whether it actually did. */
  discard: () => boolean;
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
  discard: () => boolean,
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
 * Discards the live recording wherever it is, and reports whether it actually
 * went. Lets a sibling composer's Escape reach a recorder it does not own.
 *
 * The holder answers, not the registry: a note that is already encoding
 * declines (an ambient key must not cancel a send the user committed to), and
 * a claim with nothing behind it declines too. A caller that treats the mere
 * existence of a claim as "handled" swallows Escape for the whole window.
 */
export function discardActiveVoiceNoteRecording(): boolean {
  const active = claim;
  if (!active) return false;
  return active.discard();
}
