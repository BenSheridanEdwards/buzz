/**
 * Pure helpers for showing the transcript of a voice note whose author could
 * not supply one.
 *
 * A voice note only carries a transcript when its author put one in the imeta
 * `alt` field. An agent does that for its own replies as it publishes. A
 * person cannot: the composer has no speech-to-text, and the credential that
 * would do it lives with the agents. Nothing can add the tag afterwards
 * either, because the author signed the event.
 *
 * So the agent that transcribed the clip publishes the words as their own
 * event (`KIND_VOICE_NOTE_TRANSCRIPT`) tagged `e` with the note's id, and this
 * module folds them back onto the note's imeta `alt`. Overlaying there rather
 * than plumbing a second transcript source through the UI means every
 * consumer that already reads `alt` keeps working untouched: the card, the
 * toggle, the chevron.
 *
 * Lives in `.mjs` (not `.ts`) so the test runner imports the same source the
 * renderer uses, matching `applyEditTagOverlay.mjs`.
 */

/**
 * Kind 40009. Declared locally so this stays dependency-free like its sibling;
 * `kinds.ts` exports the same value as `KIND_VOICE_NOTE_TRANSCRIPT` and a test
 * asserts the two agree.
 */
export const VOICE_NOTE_TRANSCRIPT_KIND = 40009;

/**
 * Hard ceiling on a transcript the client will show, in characters.
 *
 * The publisher caps its own output, but a publisher is exactly who this
 * defends against, so the cap is re-applied on the way in.
 */
const MAX_TRANSCRIPT_CHARS = 1000;

/**
 * Characters that would otherwise be interpreted by the Markdown renderer.
 *
 * The transcript row renders through `<Markdown>`. That was safe while `alt`
 * could only come from the message's own author, because it was their content
 * either way. A published transcript is written by someone else and rendered
 * inside the author's card, so formatting it would let a third party stage
 * styled content that reads as though the author wrote it.
 *
 * Speech has no markup, so a transcript is escaped to render literally rather
 * than filtered, which would silently drop words someone actually said.
 */
const MARKDOWN_INLINE = /[\\`*_[\]()~|@!]/g;

/**
 * Block constructs only fire at the start of a line, and a transcript is
 * collapsed to one line, so only the first character can open one. Escaping
 * these everywhere would litter ordinary prose with backslashes: "it came
 * through." must not render as "it came through\.".
 */
const LEADING_BLOCK = /^([#>\-+]|\d+\.)/;

/**
 * Make a third-party transcript safe to render in the author's card.
 *
 * Control characters go (they can reorder or hide text in a rendered line),
 * whitespace collapses to one line, the length is re-capped, and Markdown
 * syntax is escaped so the words appear exactly as spoken.
 */
export function sanitizePublishedTranscript(text) {
  if (typeof text !== "string") return undefined;
  // Strip C0/C1 controls and the bidirectional overrides that can visually
  // reverse a sentence, then collapse the remaining whitespace.
  const stripped = text
    // eslint-disable-next-line no-control-regex
    .replace(/[\u0000-\u001f\u007f-\u009f\u200e\u200f\u202a-\u202e\u2066-\u2069]/g, " ")
    .split(/\s+/)
    .filter(Boolean)
    .join(" ");
  if (!stripped) return undefined;
  const capped =
    stripped.length > MAX_TRANSCRIPT_CHARS
      ? stripped.slice(0, MAX_TRANSCRIPT_CHARS).trimEnd()
      : stripped;
  const inlineSafe = capped.replace(MARKDOWN_INLINE, (c) => `\\${c}`);
  return inlineSafe.replace(LEADING_BLOCK, (m) => `\\${m}`);
}

/** The `e` tag a transcript points at. */
function targetIdOf(tags) {
  for (const tag of tags) {
    if (tag[0] === "e" && tag[1]) return tag[1];
  }
  return undefined;
}

/**
 * Index voice-note transcripts by the message they describe.
 *
 * **First writer wins.** Any relay member can publish one of these, so keeping
 * the latest would let a later event silently rewrite what someone is shown to
 * have said. Keeping the earliest means a spoof cannot overwrite the genuine
 * transcript, only lose a race to it.
 */
export function indexVoiceNoteTranscripts(events, deletedEventIds = new Set()) {
  const byTarget = new Map();
  for (const event of events) {
    if (event.kind !== VOICE_NOTE_TRANSCRIPT_KIND) continue;
    if (deletedEventIds.has(event.id)) continue;
    const text = sanitizePublishedTranscript(event.content);
    if (!text) continue;
    const targetId = targetIdOf(event.tags ?? []);
    if (!targetId || deletedEventIds.has(targetId)) continue;
    const existing = byTarget.get(targetId);
    if (!existing || event.created_at < existing.createdAt) {
      byTarget.set(targetId, { text, createdAt: event.created_at });
    }
  }
  return new Map([...byTarget].map(([id, v]) => [id, v.text]));
}

/**
 * Write `transcript` into the message's audio imeta tag as `alt`.
 *
 * An imeta that already carries `alt` is left alone: the author's own
 * transcript is authoritative and must never be replaced by a published one.
 */
export function applyTranscriptToTags(tags, transcript) {
  if (!transcript) return tags;
  let changed = false;
  const next = tags.map((tag) => {
    if (tag[0] !== "imeta") return tag;
    const isAudio = tag.some(
      (field) =>
        typeof field === "string" &&
        field.startsWith("m ") &&
        field.slice(2).startsWith("audio/"),
    );
    if (!isAudio) return tag;
    if (tag.some((field) => typeof field === "string" && field.startsWith("alt "))) {
      return tag;
    }
    changed = true;
    return [...tag, `alt ${transcript}`];
  });
  return changed ? next : tags;
}
