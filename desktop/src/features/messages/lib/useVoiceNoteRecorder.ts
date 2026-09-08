import * as React from "react";

import { encodeVoiceNoteWav } from "./voiceNoteWav";

const MIME_CANDIDATES = [
  "audio/webm;codecs=opus",
  "audio/ogg;codecs=opus",
  "audio/mp4",
  "audio/webm",
] as const;

function supportedMimeType(): string | undefined {
  if (typeof MediaRecorder === "undefined") return undefined;
  return MIME_CANDIDATES.find((type) => MediaRecorder.isTypeSupported(type));
}

export type VoiceNoteRecording = {
  duration: number;
  file: File;
};

export type VoiceNoteRecorderStatus =
  | "idle"
  | "requesting"
  | "recording"
  | "paused"
  | "processing";

type RecordingSession = {
  cancelled: boolean;
  chunks: Blob[];
  context: AudioContext | null;
  /** `performance.now()` when the current pause began, or null while live. */
  pausedAt: number | null;
  /** Total milliseconds spent paused; subtracted from the elapsed clock. */
  pausedTotal: number;
  recorder: MediaRecorder | null;
  resolveStop: ((recording: VoiceNoteRecording | null) => void) | null;
  startedAt: number;
  stream: MediaStream | null;
};

function sessionElapsedSeconds(session: RecordingSession, now: number): number {
  const pausedNow =
    session.pausedAt === null ? 0 : Math.max(0, now - session.pausedAt);
  return Math.max(
    0,
    (now - session.startedAt - session.pausedTotal - pausedNow) / 1000,
  );
}

function releaseSessionAudio(session: RecordingSession) {
  session.stream?.getTracks().forEach((track) => {
    track.stop();
  });
  session.stream = null;
  const context = session.context;
  session.context = null;
  if (context) void context.close().catch(() => undefined);
}

export function useVoiceNoteRecorder() {
  const mountedRef = React.useRef(true);
  const sessionRef = React.useRef<RecordingSession | null>(null);
  const [status, setStatus] = React.useState<VoiceNoteRecorderStatus>("idle");
  const [elapsedSeconds, setElapsedSeconds] = React.useState(0);
  const [levels, setLevels] = React.useState<number[]>([]);
  const [error, setError] = React.useState<string | null>(null);
  // Hands-free mode: the recording keeps going after the hold is released and
  // waits for an explicit Send, pause, or discard.
  const [locked, setLocked] = React.useState(false);

  const cancel = React.useCallback(() => {
    const session = sessionRef.current;
    if (!session) return;
    session.cancelled = true;
    sessionRef.current = null;
    session.resolveStop?.(null);
    session.resolveStop = null;
    const recorder = session.recorder;
    if (recorder && recorder.state !== "inactive") recorder.stop();
    releaseSessionAudio(session);
    if (mountedRef.current) {
      setStatus("idle");
      setElapsedSeconds(0);
      setLocked(false);
    }
  }, []);

  const start = React.useCallback(async () => {
    if (status !== "idle" || sessionRef.current) return;
    setError(null);
    if (!navigator.mediaDevices?.getUserMedia || !window.MediaRecorder) {
      setError("Voice recording is not available in this environment.");
      return;
    }

    const session: RecordingSession = {
      cancelled: false,
      chunks: [],
      context: null,
      pausedAt: null,
      pausedTotal: 0,
      recorder: null,
      resolveStop: null,
      startedAt: 0,
      stream: null,
    };
    sessionRef.current = session;
    setLocked(false);
    setStatus("requesting");

    try {
      const stream = await navigator.mediaDevices.getUserMedia({
        audio: {
          autoGainControl: true,
          echoCancellation: true,
          noiseSuppression: true,
        },
      });
      session.stream = stream;
      if (
        session.cancelled ||
        !mountedRef.current ||
        sessionRef.current !== session
      ) {
        releaseSessionAudio(session);
        return;
      }

      const mimeType = supportedMimeType();
      const recorder = mimeType
        ? new MediaRecorder(stream, { mimeType })
        : new MediaRecorder(stream);
      session.recorder = recorder;
      const context = new AudioContext();
      session.context = context;
      const analyser = context.createAnalyser();
      analyser.fftSize = 512;
      analyser.smoothingTimeConstant = 0.72;
      context.createMediaStreamSource(stream).connect(analyser);
      session.startedAt = performance.now();
      setElapsedSeconds(0);
      setLevels([]);

      recorder.addEventListener("dataavailable", (event) => {
        if (event.data.size > 0) session.chunks.push(event.data);
      });
      recorder.addEventListener("stop", () => {
        void (async () => {
          const actualMime = recorder.mimeType || mimeType || "audio/webm";
          const blob = new Blob(session.chunks, { type: actualMime });
          session.chunks = [];
          let recording: VoiceNoteRecording | null = null;
          if (!session.cancelled && blob.size > 0) {
            try {
              const encoded = await blob.arrayBuffer();
              const decoded = await context.decodeAudioData(encoded.slice(0));
              if (
                !session.cancelled &&
                mountedRef.current &&
                sessionRef.current === session
              ) {
                const channels = Array.from(
                  { length: decoded.numberOfChannels },
                  (_, index) => decoded.getChannelData(index),
                );
                const wav = encodeVoiceNoteWav(channels, decoded.sampleRate);
                const wavBuffer = new ArrayBuffer(wav.byteLength);
                new Uint8Array(wavBuffer).set(wav);
                recording = {
                  duration: decoded.duration,
                  file: new File([wavBuffer], `voice-note-${Date.now()}.wav`, {
                    type: "audio/wav",
                  }),
                };
              }
            } catch {
              if (
                !session.cancelled &&
                mountedRef.current &&
                sessionRef.current === session
              ) {
                setError("Buzz could not prepare this voice note for upload.");
              }
            }
          }
          releaseSessionAudio(session);
          if (sessionRef.current === session) {
            sessionRef.current = null;
            if (mountedRef.current) {
              setStatus("idle");
              setElapsedSeconds(0);
              setLocked(false);
            }
          }
          session.resolveStop?.(recording);
          session.resolveStop = null;
        })();
      });
      recorder.addEventListener("error", () => {
        if (mountedRef.current && sessionRef.current === session) {
          setError("The voice recording was interrupted.");
        }
      });
      recorder.start(250);
      setStatus("recording");

      const samples = new Uint8Array(analyser.fftSize);
      const levelTimer = window.setInterval(() => {
        if (
          recorder.state === "inactive" ||
          session.cancelled ||
          sessionRef.current !== session
        ) {
          window.clearInterval(levelTimer);
          return;
        }
        // A paused recording keeps its timer so resume continues the same
        // session, but neither the clock nor the waveform advances.
        if (recorder.state === "paused" || session.pausedAt !== null) return;
        analyser.getByteTimeDomainData(samples);
        let sumSquares = 0;
        for (const sample of samples) {
          const centered = (sample - 128) / 128;
          sumSquares += centered * centered;
        }
        const rms = Math.sqrt(sumSquares / samples.length);
        const level = Math.min(1, rms * 5.5);
        if (!mountedRef.current) return;
        setLevels((previous) => [...previous, level]);
        setElapsedSeconds(sessionElapsedSeconds(session, performance.now()));
      }, 90);
    } catch (cause) {
      releaseSessionAudio(session);
      if (
        session.cancelled ||
        !mountedRef.current ||
        sessionRef.current !== session
      ) {
        return;
      }
      sessionRef.current = null;
      setStatus("idle");
      setLocked(false);
      const denied =
        cause instanceof DOMException &&
        (cause.name === "NotAllowedError" || cause.name === "SecurityError");
      setError(
        denied
          ? "Allow Buzz to access your microphone to record a voice note."
          : "Buzz could not start the voice recorder.",
      );
    }
  }, [status]);

  const stop = React.useCallback(
    (discard = false): Promise<VoiceNoteRecording | null> => {
      if (discard) {
        cancel();
        return Promise.resolve(null);
      }
      const session = sessionRef.current;
      const recorder = session?.recorder;
      if (!session || !recorder || recorder.state === "inactive") {
        return Promise.resolve(null);
      }
      setStatus("processing");
      return new Promise((resolve) => {
        session.resolveStop = resolve;
        recorder.stop();
      });
    },
    [cancel],
  );

  const pause = React.useCallback(() => {
    const session = sessionRef.current;
    const recorder = session?.recorder;
    if (!session || !recorder || recorder.state !== "recording") return;
    if (session.pausedAt !== null) return;
    session.pausedAt = performance.now();
    recorder.pause();
    setElapsedSeconds(sessionElapsedSeconds(session, session.pausedAt));
    setStatus("paused");
  }, []);

  const resume = React.useCallback(() => {
    const session = sessionRef.current;
    const recorder = session?.recorder;
    if (!session || !recorder || recorder.state !== "paused") return;
    if (session.pausedAt !== null) {
      session.pausedTotal += Math.max(0, performance.now() - session.pausedAt);
      session.pausedAt = null;
    }
    recorder.resume();
    setStatus("recording");
  }, []);

  const lock = React.useCallback(() => {
    if (!sessionRef.current) return;
    setLocked(true);
  }, []);

  React.useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      cancel();
    };
  }, [cancel]);

  return {
    cancel,
    elapsedSeconds,
    error,
    levels,
    lock,
    locked,
    pause,
    resume,
    start,
    status,
    stop,
  };
}
