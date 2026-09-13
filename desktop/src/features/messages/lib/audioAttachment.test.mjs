import assert from "node:assert/strict";
import test from "node:test";

import {
  formatVoiceNoteDuration,
  isAudioAttachment,
  isVoiceNoteAttachment,
  isVoiceNoteFile,
  formatVoiceNotePlaybackRate,
  nextVoiceNotePlaybackRate,
  OWN_VOICE_NOTE_RATE_STEP,
  readTranscriptPreference,
  RECEIVED_VOICE_NOTE_RATE_STEP,
  resolveAudioAttachment,
  resolveTranscriptOpen,
  summarizeWaveform,
  transcriptDefaultOpen,
  VOICE_NOTE_MAX_DURATION_SECONDS,
  VOICE_NOTE_MAX_PLAYBACK_RATE,
  VOICE_NOTE_MIN_PLAYBACK_RATE,
  voiceNoteDefaultPlaybackRate,
  VOICE_NOTE_TRANSCRIPT_STORAGE_KEY,
  voiceNoteBarHeight,
  voiceNoteTranscript,
  waveformPeaks,
  WAVEFORM_SUMMARY_RESOLUTION,
  writeTranscriptPreference,
} from "./audioAttachment.ts";

test("isVoiceNoteFile scopes deferred audio uploads to recorder output", () => {
  assert.equal(
    isVoiceNoteFile(
      new File([new Uint8Array([1])], "voice-note-123.wav", {
        type: "audio/wav",
      }),
    ),
    true,
  );
  assert.equal(
    isVoiceNoteFile(
      new File([new Uint8Array([1])], "meeting.wav", { type: "audio/wav" }),
    ),
    false,
  );
});

test("resolveAudioAttachment accepts audio imeta and preserves metadata", () => {
  assert.deepEqual(
    resolveAudioAttachment(
      {
        duration: 12.5,
        filename: "voice-note.webm",
        m: "audio/webm;codecs=opus",
        size: 2048,
      },
      "https://relay.example/media/voice-note.webm",
      "Voice note",
    ),
    {
      duration: 12.5,
      filename: "voice-note.webm",
      href: "https://relay.example/media/voice-note.webm",
      size: 2048,
    },
  );
});

test("generic audio renders without triggering voice-note exclusivity", () => {
  const entry = {
    duration: 42,
    filename: "meeting.mp3",
    m: "audio/mpeg",
  };
  assert.equal(isVoiceNoteAttachment(entry), false);
  assert.equal(isAudioAttachment(entry), true);
  assert.deepEqual(
    resolveAudioAttachment(
      entry,
      "https://relay.example/media/meeting.mp3",
      "meeting.mp3",
    ),
    {
      duration: 42,
      filename: "meeting.mp3",
      href: "https://relay.example/media/meeting.mp3",
      size: undefined,
    },
  );
});

test("resolveAudioAttachment leaves non-audio files on the generic path", () => {
  assert.equal(
    resolveAudioAttachment(
      { filename: "notes.pdf", m: "application/pdf" },
      "https://relay.example/media/notes.pdf",
      "notes.pdf",
    ),
    null,
  );
});

test("real audio voice notes from a buzz-audio relay resolve to the audio player", () => {
  for (const [filename, m] of [
    ["voice-note-123.m4a", "audio/mp4"],
    ["voice-note-456.mp3", "audio/mpeg"],
  ]) {
    const entry = { duration: 4.5, filename, m };
    assert.equal(isVoiceNoteAttachment(entry), true, filename);
    assert.equal(isAudioAttachment(entry), true, filename);
  }
  // A plain audio file without the voice-note prefix is audio, not a voice note.
  const plain = { filename: "song.mp3", m: "audio/mpeg" };
  assert.equal(isVoiceNoteAttachment(plain), false);
  assert.equal(isAudioAttachment(plain), true);
});

test("packaged MP4 voice notes still resolve to the audio player", () => {
  const entry = {
    duration: 7.2,
    filename: "voice-note-123.mp4",
    m: "video/mp4",
  };
  assert.equal(isVoiceNoteAttachment(entry), true);
  assert.deepEqual(
    resolveAudioAttachment(
      entry,
      "https://relay.example/media/hash.mp4",
      "voice-note-123.mp4",
    ),
    {
      duration: 7.2,
      filename: "voice-note-123.mp4",
      href: "https://relay.example/media/hash.mp4",
      size: undefined,
    },
  );
  assert.equal(
    isVoiceNoteAttachment({ filename: "meeting.mp4", m: "video/mp4" }),
    false,
  );
});

test("formatVoiceNoteDuration formats minutes and seconds", () => {
  assert.equal(formatVoiceNoteDuration(0), "0:00");
  assert.equal(formatVoiceNoteDuration(65.9), "1:05");
});

test("voice notes have a five-minute recording limit", () => {
  assert.equal(VOICE_NOTE_MAX_DURATION_SECONDS, 300);
  assert.equal(
    formatVoiceNoteDuration(VOICE_NOTE_MAX_DURATION_SECONDS),
    "5:00",
  );
});

test("your own notes step by a quarter to 2x, then back to 1x", () => {
  const own = { ownNote: true, defaultRate: 1 };
  assert.equal(nextVoiceNotePlaybackRate(1, own), 1.25);
  assert.equal(nextVoiceNotePlaybackRate(1.25, own), 1.5);
  assert.equal(nextVoiceNotePlaybackRate(1.5, own), 1.75);
  assert.equal(nextVoiceNotePlaybackRate(1.75, own), 2);
  assert.equal(nextVoiceNotePlaybackRate(2, own), 1);
  assert.equal(OWN_VOICE_NOTE_RATE_STEP, 0.25);
});

test("received notes step by a tenth and wrap to the voice's default", () => {
  const sky = { ownNote: false, defaultRate: 1.1 };
  assert.equal(nextVoiceNotePlaybackRate(1.1, sky), 1.2);
  // No float tail: 1.1 + 0.1 is 1.2, not 1.2000000000000002.
  assert.equal(nextVoiceNotePlaybackRate(1.2, sky), 1.3);
  assert.equal(nextVoiceNotePlaybackRate(1.9, sky), 2);
  assert.equal(nextVoiceNotePlaybackRate(2, sky), 1.1);
  // A voice hinted slower than 1x steps up from there.
  const slow = { ownNote: false, defaultRate: 0.8 };
  assert.equal(nextVoiceNotePlaybackRate(0.8, slow), 0.9);
  assert.equal(nextVoiceNotePlaybackRate(0.9, slow), 1);
  assert.equal(RECEIVED_VOICE_NOTE_RATE_STEP, 0.1);
  assert.equal(VOICE_NOTE_MAX_PLAYBACK_RATE, 2);
});

test("a rate the pill could not have produced resets to the default", () => {
  const own = { ownNote: true, defaultRate: 1 };
  assert.equal(nextVoiceNotePlaybackRate(Number.NaN, own), 1);
  assert.equal(nextVoiceNotePlaybackRate(0.25, own), 1);
  assert.equal(nextVoiceNotePlaybackRate(99, own), 1);
  const sky = { ownNote: false, defaultRate: 1.1 };
  assert.equal(nextVoiceNotePlaybackRate(Number.POSITIVE_INFINITY, sky), 1.1);
});

test("the starting rate is 1x for your notes and the hint for received ones", () => {
  assert.equal(voiceNoteDefaultPlaybackRate({ playbackSpeed: 1.1 }, true), 1);
  assert.equal(
    voiceNoteDefaultPlaybackRate({ playbackSpeed: 1.1 }, false),
    1.1,
  );
  assert.equal(
    voiceNoteDefaultPlaybackRate({ playbackSpeed: 0.8 }, false),
    0.8,
  );
  assert.equal(voiceNoteDefaultPlaybackRate({}, false), 1);
  assert.equal(voiceNoteDefaultPlaybackRate(undefined, false), 1);
});

test("an unusable speed hint falls back to 1x", () => {
  for (const playbackSpeed of [
    0,
    0.4,
    2.1,
    4,
    -1,
    Number.NaN,
    Number.POSITIVE_INFINITY,
  ]) {
    assert.equal(
      voiceNoteDefaultPlaybackRate({ playbackSpeed }, false),
      1,
      `hint ${playbackSpeed}`,
    );
  }
  assert.equal(VOICE_NOTE_MIN_PLAYBACK_RATE, 0.5);
  // A hint with a float tail is shown as the sender meant it.
  assert.equal(
    voiceNoteDefaultPlaybackRate({ playbackSpeed: 1.1000000001 }, false),
    1.1,
  );
});

test("the pill label never shows a float tail", () => {
  assert.equal(formatVoiceNotePlaybackRate(1), "1×");
  assert.equal(formatVoiceNotePlaybackRate(1.25), "1.25×");
  assert.equal(formatVoiceNotePlaybackRate(1.1 + 0.1), "1.2×");
  assert.equal(formatVoiceNotePlaybackRate(2), "2×");
});

test("transcripts start folded everywhere", () => {
  assert.equal(transcriptDefaultOpen(), false);
  assert.equal(resolveTranscriptOpen(undefined), false);
});

test("transcript preference round-trips through storage", () => {
  const store = new Map();
  const storage = {
    getItem: (key) => store.get(key) ?? null,
    setItem: (key, value) => store.set(key, value),
  };
  assert.equal(readTranscriptPreference(storage), null);

  writeTranscriptPreference(false, storage);
  assert.equal(store.get(VOICE_NOTE_TRANSCRIPT_STORAGE_KEY), "closed");
  assert.equal(readTranscriptPreference(storage), false);
  assert.equal(resolveTranscriptOpen(storage), false);

  // Opening one transcript keeps the next ones open: the choice sticks.
  writeTranscriptPreference(true, storage);
  assert.equal(readTranscriptPreference(storage), true);
  assert.equal(resolveTranscriptOpen(storage), true);

  store.set(VOICE_NOTE_TRANSCRIPT_STORAGE_KEY, "garbage");
  assert.equal(readTranscriptPreference(storage), null);
});

test("a throwing localStorage never breaks the transcript default", () => {
  const throwing = {
    getItem() {
      throw new DOMException("denied", "SecurityError");
    },
    setItem() {
      throw new DOMException("denied", "SecurityError");
    },
  };
  assert.equal(readTranscriptPreference(throwing), null);
  assert.doesNotThrow(() => writeTranscriptPreference(true, throwing));
  assert.equal(resolveTranscriptOpen(throwing), false);
});

test("the transcript is the voice note's own alt text, never the body", () => {
  const voiceNote = {
    alt: "  Status is green on the Studio.\nSay the word.  ",
    filename: "voice-note-1.mp4",
    m: "video/mp4",
  };
  assert.equal(
    voiceNoteTranscript(voiceNote),
    "Status is green on the Studio.\nSay the word.",
  );
  // No alt, or a blank one: no transcript row.
  assert.equal(
    voiceNoteTranscript({ filename: "voice-note-1.mp4", m: "video/mp4" }),
    undefined,
  );
  assert.equal(voiceNoteTranscript({ ...voiceNote, alt: "   " }), undefined);
  // Alt text on other media is a description, not a transcript.
  assert.equal(
    voiceNoteTranscript({
      alt: "A photo",
      filename: "pic.png",
      m: "image/png",
    }),
    undefined,
  );
  assert.equal(
    voiceNoteTranscript({
      alt: "Lyrics",
      filename: "song.mp3",
      m: "audio/mpeg",
    }),
    undefined,
  );
  assert.equal(voiceNoteTranscript(undefined), undefined);
});

test("waveformPeaks produces normalized accessible-height bars", () => {
  const peaks = waveformPeaks(new Float32Array([0, 0.25, -0.5, 1]), 2);
  assert.deepEqual(peaks, [0.25, 1]);
  assert.deepEqual(waveformPeaks(new Float32Array(), 2), [0.12, 0.12]);
});

test("voiceNoteBarHeight keeps quiet samples circular", () => {
  assert.equal(voiceNoteBarHeight(0), 3);
  assert.equal(voiceNoteBarHeight(0.12), 3);
  assert.equal(voiceNoteBarHeight(0.16), 3);
  assert.equal(voiceNoteBarHeight(1), 20);
});

test("summarizeWaveform bounds retained data regardless of clip length", () => {
  const longClip = new Float32Array(48_000 * 300).map(() => 0.5);
  const summary = summarizeWaveform(longClip);
  assert.equal(summary.length, WAVEFORM_SUMMARY_RESOLUTION);
  assert.ok(
    summary.length < longClip.length,
    "summary must be far smaller than the decoded clip",
  );

  const short = new Float32Array([0.2, 0.9]);
  assert.equal(summarizeWaveform(short).length, 2);
  assert.equal(summarizeWaveform(new Float32Array()).length, 1);
});

test("resampling the summary matches pooling the raw samples", () => {
  const samples = new Float32Array([0, 0.25, -0.5, 1, -0.3, 0.8]);
  const summary = summarizeWaveform(samples, 6);
  assert.deepEqual(
    Array.from(waveformPeaks(summary, 2)),
    Array.from(waveformPeaks(samples, 2)),
  );
});
