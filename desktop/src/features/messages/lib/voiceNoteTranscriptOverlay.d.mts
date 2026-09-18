/**
 * Type declarations for the pure overlay helpers in
 * `voiceNoteTranscriptOverlay.mjs`. Runtime lives in `.mjs` so the
 * (TS-loader-less) `node:test` runner can import it directly; this file gives
 * TypeScript callers a typed view, matching `applyEditTagOverlay.d.mts`.
 */

export type Tag = string[];

/** Kind 40009. `kinds.ts` exports the same value; a test asserts they agree. */
export const VOICE_NOTE_TRANSCRIPT_KIND: 40009;

/** Minimal event shape the overlay reads. */
export type TranscriptEvent = {
  id: string;
  kind: number;
  content?: string;
  tags?: Tag[];
  created_at: number;
};

/**
 * Make a third-party transcript safe to render in the author's card:
 * markdown escaped, control and bidi characters stripped, length re-capped.
 * `undefined` for whitespace-only or non-string input.
 */
export function sanitizePublishedTranscript(text: unknown): string | undefined;

/**
 * Index voice-note transcripts by the message they describe. First writer
 * wins, so a later event cannot rewrite what someone is shown to have said.
 */
export function indexVoiceNoteTranscripts(
  events: readonly TranscriptEvent[],
  deletedEventIds?: ReadonlySet<string>,
): Map<string, string>;

/**
 * Write `transcript` into the message's audio imeta tag as `alt`. An imeta
 * that already carries `alt` is left alone. Returns the same array when
 * nothing changed.
 */
export function applyTranscriptToTags(
  tags: Tag[],
  transcript: string | undefined,
): Tag[];
