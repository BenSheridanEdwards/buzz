import assert from "node:assert/strict";
import test from "node:test";

import {
  formatVoiceNoteDuration,
  isAudioAttachment,
  isVoiceNoteAttachment,
  isVoiceNoteFile,
  nextVoiceNotePlaybackRate,
  readTranscriptPreference,
  resolveAudioAttachment,
  resolveTranscriptOpen,
  summarizeWaveform,
  transcriptDefaultOpen,
  VOICE_NOTE_MAX_DURATION_SECONDS,
  VOICE_NOTE_PLAYBACK_RATES,
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

test("nextVoiceNotePlaybackRate cycles 1x, 1.5x, 2x and back", () => {
  assert.deepEqual([...VOICE_NOTE_PLAYBACK_RATES], [1, 1.5, 2]);
  assert.equal(nextVoiceNotePlaybackRate(1), 1.5);
  assert.equal(nextVoiceNotePlaybackRate(1.5), 2);
  assert.equal(nextVoiceNotePlaybackRate(2), 1);
  // A rate outside the cycle (the retired 0.5x) resumes from 1x.
  assert.equal(nextVoiceNotePlaybackRate(0.5), 1);
  assert.equal(nextVoiceNotePlaybackRate(99), 1);
});

test("transcripts default open in DMs and folded in channels", () => {
  assert.equal(transcriptDefaultOpen("dm"), true);
  assert.equal(transcriptDefaultOpen("channel"), false);
  assert.equal(resolveTranscriptOpen("dm", undefined), true);
  assert.equal(resolveTranscriptOpen("channel", undefined), false);
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
  // The remembered choice wins over the DM default.
  assert.equal(resolveTranscriptOpen("dm", storage), false);

  writeTranscriptPreference(true, storage);
  assert.equal(readTranscriptPreference(storage), true);
  assert.equal(resolveTranscriptOpen("channel", storage), true);

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
  assert.equal(resolveTranscriptOpen("dm", throwing), true);
  assert.equal(resolveTranscriptOpen("channel", throwing), false);
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
