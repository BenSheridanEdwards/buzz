import assert from "node:assert/strict";
import test from "node:test";

import {
  applyTranscriptToTags,
  indexVoiceNoteTranscripts,
  sanitizePublishedTranscript,
} from "./voiceNoteTranscriptOverlay.mjs";

const transcriptEvent = (id, target, content, createdAt) => ({
  id,
  kind: 40009,
  content,
  tags: [["e", target]],
  created_at: createdAt,
});

test("index: maps a transcript onto the note it references", () => {
  const map = indexVoiceNoteTranscripts([
    transcriptEvent("t1", "note1", "Yes Chief, it came through.", 100),
  ]);
  assert.equal(map.get("note1"), "Yes Chief, it came through.");
});

test("index: first writer wins, so a later event cannot rewrite what was said", () => {
  const map = indexVoiceNoteTranscripts([
    transcriptEvent("t2", "note1", "spoofed later", 200),
    transcriptEvent("t1", "note1", "the real words", 100),
  ]);
  assert.equal(map.get("note1"), "the real words");
});

test("index: ignores an empty transcript rather than mapping a blank", () => {
  const map = indexVoiceNoteTranscripts([
    transcriptEvent("t1", "note1", "   \n ", 100),
  ]);
  assert.equal(map.has("note1"), false);
});

test("index: ignores a transcript for a deleted note", () => {
  const map = indexVoiceNoteTranscripts(
    [transcriptEvent("t1", "note1", "words", 100)],
    new Set(["note1"]),
  );
  assert.equal(map.has("note1"), false);
});

test("index: ignores kinds that are not transcripts", () => {
  const map = indexVoiceNoteTranscripts([
    { ...transcriptEvent("t1", "note1", "words", 100), kind: 40003 },
  ]);
  assert.equal(map.size, 0);
});

test("apply: writes alt onto an audio imeta that lacks one", () => {
  const tags = [
    ["imeta", "url https://b/v.mp3", "m audio/mpeg", "duration 3"],
  ];
  const [tag] = applyTranscriptToTags(tags, "the words");
  assert.ok(tag.includes("alt the words"));
});

test("apply: never replaces an alt the author already supplied", () => {
  const tags = [
    ["imeta", "url https://b/v.mp3", "m audio/mpeg", "alt author's own"],
  ];
  const [tag] = applyTranscriptToTags(tags, "published later");
  assert.ok(tag.includes("alt author's own"));
  assert.ok(!tag.some((f) => f === "alt published later"));
});

test("apply: leaves a non-audio imeta untouched", () => {
  const tags = [["imeta", "url https://b/p.png", "m image/png"]];
  const [tag] = applyTranscriptToTags(tags, "the words");
  assert.ok(!tag.some((f) => f.startsWith("alt ")));
});

test("apply: with no transcript returns the tags unchanged", () => {
  const tags = [["imeta", "url https://b/v.mp3", "m audio/mpeg"]];
  assert.equal(applyTranscriptToTags(tags, undefined), tags);
});

test("the locally declared kind agrees with the shared constant", async () => {
  const { KIND_VOICE_NOTE_TRANSCRIPT } = await import(
    "../../../shared/constants/kinds.ts"
  );
  const { VOICE_NOTE_TRANSCRIPT_KIND } = await import(
    "./voiceNoteTranscriptOverlay.mjs"
  );
  assert.equal(VOICE_NOTE_TRANSCRIPT_KIND, KIND_VOICE_NOTE_TRANSCRIPT);
});


// ---------------------------------------------------------------------------
// A published transcript is written by a third party and rendered inside the
// author's own card, through <Markdown>. These pin the sanitising that keeps
// it from staging styled content that reads as though the author wrote it.
// ---------------------------------------------------------------------------

test("sanitize: markdown syntax is escaped so speech renders literally", () => {
  const out = sanitizePublishedTranscript("see [my link](https://evil.example)");
  assert.ok(!/(^|[^\\])\[/.test(out), `unescaped bracket in: ${out}`);
  assert.ok(!/(^|[^\\])\]\(/.test(out), `live link syntax survived: ${out}`);
  assert.ok(out.includes("my link"), "the words themselves must survive");
});

test("sanitize: emphasis and headings cannot format the author's card", () => {
  const out = sanitizePublishedTranscript("# SHOUTING **bold** _em_");
  assert.ok(out.startsWith("\\#"), `heading not escaped: ${out}`);
  assert.ok(!/(^|[^\\])\*/.test(out), `unescaped emphasis in: ${out}`);
});

test("sanitize: an @mention cannot be staged as a real mention", () => {
  const out = sanitizePublishedTranscript("ask @sky about it");
  assert.ok(out.includes("\\@sky"), `unescaped mention in: ${out}`);
});

test("sanitize: control characters and bidi overrides are stripped", () => {
  const hostile = "safe" + "\u202e" + "reversed" + "\u202c" + "\u0007" + " here";
  const out = sanitizePublishedTranscript(hostile);
  assert.ok(!/[\u200e\u200f\u202a-\u202e\u2066-\u2069]/.test(out), "bidi override survived");
  assert.ok(!/CTRL[\u200e\u200f\u202a-\u202e\u2066-\u2069]/.test(out), "control character survived");
  assert.ok(out.includes("safe"), "the readable words must survive");
});

test("sanitize: re-caps length rather than trusting the publisher", () => {
  const out = sanitizePublishedTranscript("a".repeat(5000));
  assert.ok(out.replace(/\\/g, "").length <= 1000, "unescaped length not capped");
});

test("sanitize: whitespace-only and non-strings yield no transcript", () => {
  assert.equal(sanitizePublishedTranscript("   \n\t "), undefined);
  assert.equal(sanitizePublishedTranscript(undefined), undefined);
  assert.equal(sanitizePublishedTranscript(42), undefined);
});

test("index: folds the sanitized form, not the raw content", () => {
  const map = indexVoiceNoteTranscripts([
    transcriptEvent("t1", "note1", "click [here](https://evil.example)", 100),
  ]);
  const got = map.get("note1");
  assert.ok(got && !/(^|[^\\])\]\(/.test(got), `raw markdown reached the fold: ${got}`);
});

test("sanitize: ordinary prose is not littered with backslashes", () => {
  const out = sanitizePublishedTranscript("Yes Chief, it came through. Can you hear me?");
  assert.equal(out, "Yes Chief, it came through. Can you hear me?");
});

test("sanitize: a leading block character is escaped even though prose is not", () => {
  assert.ok(sanitizePublishedTranscript("- a list item").startsWith("\\-"));
  assert.ok(sanitizePublishedTranscript("> a quote").startsWith("\\>"));
  assert.ok(sanitizePublishedTranscript("1. a numbered item").startsWith("\\1."));
});
