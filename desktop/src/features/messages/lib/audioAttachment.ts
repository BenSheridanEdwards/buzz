export type AudioAttachmentImetaEntry = {
  /** NIP-92 `alt`: for a voice note, the sender-supplied transcript. */
  alt?: string;
  duration?: number;
  filename?: string;
  m?: string;
  /**
   * `playback_speed`: the rate the sender's voice is meant to be heard at.
   * An agent's harness writes it from the voice's own configuration, so a
   * voice tuned to 1.1x starts there on every device.
   */
  playbackSpeed?: number;
  size?: number;
};

export type ResolvedAudioAttachment = {
  duration?: number;
  filename: string;
  href: string;
  size?: number;
};

export const VOICE_NOTE_MAX_DURATION_SECONDS = 5 * 60;

export function isVoiceNoteFile(file: File): boolean {
  const filename = file.name.toLowerCase();
  return (
    file.type.startsWith("audio/") &&
    filename.startsWith("voice-note-") &&
    filename.endsWith(".wav")
  );
}

export function isVoiceNoteAttachment(
  entry: AudioAttachmentImetaEntry | undefined,
): boolean {
  const mime = entry?.m?.toLowerCase() ?? "";
  const filename = entry?.filename?.toLowerCase() ?? "";
  if (!filename.startsWith("voice-note-")) return false;
  if (mime.startsWith("audio/")) return true;
  return mime === "video/mp4" && filename.endsWith(".mp4");
}

export function isAudioAttachment(
  entry: AudioAttachmentImetaEntry | undefined,
): boolean {
  const mime = entry?.m?.toLowerCase() ?? "";
  return mime.startsWith("audio/") || isVoiceNoteAttachment(entry);
}

export function resolveAudioAttachment(
  entry: AudioAttachmentImetaEntry | undefined,
  href: string | undefined,
  childText: string,
): ResolvedAudioAttachment | null {
  if (!href || !entry || !isAudioAttachment(entry)) return null;

  return {
    duration: entry.duration,
    filename:
      entry.filename ||
      childText.trim() ||
      href.split("/").pop() ||
      "voice-note",
    href,
    size: entry.size,
  };
}

export function formatVoiceNoteDuration(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return "0:00";
  const rounded = Math.floor(seconds);
  const minutes = Math.floor(rounded / 60);
  return `${minutes}:${String(rounded % 60).padStart(2, "0")}`;
}

/** Fastest a voice note plays; past it the pill wraps to the default. */
export const VOICE_NOTE_MAX_PLAYBACK_RATE = 2;
/** Slowest a sender's hint can ask for. */
export const VOICE_NOTE_MIN_PLAYBACK_RATE = 0.5;
/** Your own notes step by a quarter: 1, 1.25, 1.5, 1.75, 2, then 1 again. */
export const OWN_VOICE_NOTE_RATE_STEP = 0.25;
/** Received notes step by a tenth so a voice can be tuned finely. */
export const RECEIVED_VOICE_NOTE_RATE_STEP = 0.1;

function roundRate(rate: number): number {
  return Math.round(rate * 100) / 100;
}

/**
 * The rate a voice note starts at. Your own notes always start at 1x; a
 * received note starts at the sender's `playback_speed` hint when it carries
 * a sane one, else 1x.
 */
export function voiceNoteDefaultPlaybackRate(
  entry: Pick<AudioAttachmentImetaEntry, "playbackSpeed"> | undefined,
  ownNote: boolean,
): number {
  if (ownNote) return 1;
  const hint = entry?.playbackSpeed;
  if (typeof hint !== "number" || !Number.isFinite(hint)) return 1;
  if (
    hint < VOICE_NOTE_MIN_PLAYBACK_RATE ||
    hint > VOICE_NOTE_MAX_PLAYBACK_RATE
  ) {
    return 1;
  }
  return roundRate(hint);
}

/**
 * The rate after one tap on the speed pill: one step faster, wrapping to
 * `defaultRate` past 2x. A rate the pill could not have produced (not
 * finite, or below the slowest hint) also resets to the default.
 */
export function nextVoiceNotePlaybackRate(
  currentRate: number,
  { ownNote, defaultRate }: { ownNote: boolean; defaultRate: number },
): number {
  if (
    !Number.isFinite(currentRate) ||
    currentRate < VOICE_NOTE_MIN_PLAYBACK_RATE
  ) {
    return defaultRate;
  }
  const step = ownNote
    ? OWN_VOICE_NOTE_RATE_STEP
    : RECEIVED_VOICE_NOTE_RATE_STEP;
  const next = roundRate(currentRate + step);
  return next > VOICE_NOTE_MAX_PLAYBACK_RATE ? defaultRate : next;
}

/** Pill label: `1×`, `1.25×`, `1.1×`; never a float tail like `1.1000000001`. */
export function formatVoiceNotePlaybackRate(rate: number): string {
  return `${roundRate(rate)}×`;
}

export const VOICE_NOTE_TRANSCRIPT_STORAGE_KEY =
  "buzz.voiceNote.transcriptOpen";

type TranscriptPreferenceStorage = Pick<Storage, "getItem" | "setItem">;

function defaultTranscriptStorage(): TranscriptPreferenceStorage | undefined {
  try {
    return globalThis.localStorage ?? undefined;
  } catch {
    return undefined;
  }
}

/**
 * Every transcript starts folded: the words are there for when you want
 * them, never in the way of the note. Opening one is remembered
 * (`writeTranscriptPreference`), so a reader who wants them open keeps them
 * open.
 */
export function transcriptDefaultOpen(): boolean {
  return false;
}

/**
 * Last transcript fold choice on this device, or `null` when nothing was
 * stored or storage is unavailable (WKWebView can throw on `getItem`).
 */
export function readTranscriptPreference(
  storage: TranscriptPreferenceStorage | undefined = defaultTranscriptStorage(),
): boolean | null {
  try {
    const stored = storage?.getItem(VOICE_NOTE_TRANSCRIPT_STORAGE_KEY);
    if (stored === "open") return true;
    if (stored === "closed") return false;
    return null;
  } catch {
    return null;
  }
}

/** Remember the transcript fold choice; persistence is best-effort. */
export function writeTranscriptPreference(
  open: boolean,
  storage: TranscriptPreferenceStorage | undefined = defaultTranscriptStorage(),
): void {
  try {
    storage?.setItem(
      VOICE_NOTE_TRANSCRIPT_STORAGE_KEY,
      open ? "open" : "closed",
    );
  } catch {
    // Storage denied or full: the in-memory state still applies.
  }
}

/** The transcript starts from the remembered choice, else folded. */
export function resolveTranscriptOpen(
  storage: TranscriptPreferenceStorage | undefined = defaultTranscriptStorage(),
): boolean {
  return readTranscriptPreference(storage) ?? transcriptDefaultOpen();
}

/**
 * The transcript of a received voice note is the attachment's own `alt`
 * text (NIP-92 imeta), written by the sender or its transcriber. Prose in the
 * message body is a caption, not a transcript: it stays in the body. Returns
 * `undefined` for anything that is not a voice note or carries no `alt`.
 */
export function voiceNoteTranscript(
  entry: AudioAttachmentImetaEntry | undefined,
): string | undefined {
  if (!isVoiceNoteAttachment(entry)) return undefined;
  const transcript = entry?.alt?.trim();
  return transcript ? transcript : undefined;
}

const QUIET_LEVEL_THRESHOLD = 0.16;

// Waveform cards keep only this many peak buckets, not the full decoded clip.
// 256 matches the maximum bar count a card can display, so a resampled envelope
// is visually indistinguishable while retaining a fixed ~1KB regardless of clip
// duration (a 5-minute 48kHz mono note would otherwise pin ~57MB per card).
export const WAVEFORM_SUMMARY_RESOLUTION = 256;

// Reduce decoded PCM to a bounded peak envelope via max-pooling. Downstream
// display resamples this envelope to the (smaller) bar count; because 256 far
// exceeds the bars a card renders, the resampled result is visually
// indistinguishable from pooling the original samples directly.
export function summarizeWaveform(
  samples: Float32Array,
  resolution: number = WAVEFORM_SUMMARY_RESOLUTION,
): Float32Array {
  const buckets = Math.max(1, Math.min(resolution, samples.length || 1));
  const summary = new Float32Array(buckets);
  if (samples.length === 0) return summary;
  for (let index = 0; index < buckets; index += 1) {
    const start = Math.floor((index * samples.length) / buckets);
    const end = Math.max(
      start + 1,
      Math.floor(((index + 1) * samples.length) / buckets),
    );
    let peak = 0;
    for (let sampleIndex = start; sampleIndex < end; sampleIndex += 1) {
      peak = Math.max(peak, Math.abs(samples[sampleIndex] ?? 0));
    }
    summary[index] = peak;
  }
  return summary;
}

export function voiceNoteBarHeight(level: number): number {
  const audibleLevel = Math.max(
    0,
    (Math.min(1, level) - QUIET_LEVEL_THRESHOLD) / (1 - QUIET_LEVEL_THRESHOLD),
  );
  return 3 + Math.round(audibleLevel * 17);
}

export function waveformPeaks(
  samples: Float32Array,
  barCount: number,
): number[] {
  if (barCount <= 0) return [];
  if (samples.length === 0) return Array.from({ length: barCount }, () => 0.12);

  const peaks = Array.from({ length: barCount }, (_, index) => {
    const start = Math.floor((index * samples.length) / barCount);
    const end = Math.max(
      start + 1,
      Math.floor(((index + 1) * samples.length) / barCount),
    );
    let peak = 0;
    for (let sampleIndex = start; sampleIndex < end; sampleIndex += 1) {
      peak = Math.max(peak, Math.abs(samples[sampleIndex] ?? 0));
    }
    return peak;
  });
  const maximum = Math.max(...peaks, 0.001);
  return peaks.map((peak) => Math.max(0.12, Math.min(1, peak / maximum)));
}
