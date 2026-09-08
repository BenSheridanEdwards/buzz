import * as React from "react";
import {
  AlertCircle,
  ChevronDown,
  Download,
  FileText,
  Loader2,
  X,
} from "lucide-react";
import { motion, useReducedMotion } from "motion/react";
import { toast } from "sonner";

import {
  formatVoiceNoteDuration,
  isVoiceNoteAttachment,
  nextVoiceNotePlaybackRate,
  resolveAudioAttachment,
  resolveTranscriptOpen,
  summarizeWaveform,
  voiceNoteBarHeight,
  waveformPeaks,
  writeTranscriptPreference,
  type AudioAttachmentImetaEntry,
  type VoiceNoteConversationContext,
} from "@/features/messages/lib/audioAttachment";
import { scheduleAudioMediaLoad } from "@/features/messages/lib/audioMediaLoadScheduler";
import { invokeTauri } from "@/shared/api/tauri";
import { fetchMediaBytes } from "@/shared/api/tauriMedia";
import { cn } from "@/shared/lib/cn";
import { rewriteRelayUrl } from "@/shared/lib/mediaUrl";
import {
  Attachment,
  AttachmentAction,
  AttachmentActions,
  AttachmentContent,
  AttachmentMedia,
  AttachmentTitle,
} from "@/shared/ui/attachment";
import type { VoiceNoteCardContext } from "@/shared/ui/markdown/types";
import { useSmoothCorners } from "@/shared/ui/smoothCorners";
import { MorphingPlayPauseIcon } from "./MorphingPlayPauseIcon";

const PLAY_EVENT = "buzz-voice-note-play";
const INITIAL_BAR_COUNT = 38;
const BAR_KEYS = Array.from(
  { length: 256 },
  (_, index) => `voice-note-bar-${index}`,
);

function dotPeaks(count: number): number[] {
  return Array.from({ length: count }, () => 0);
}

function playbackRateLabel(rate: number): string {
  return `${rate}×`;
}

/** Accessible name for the speed pill; "x" reads the same as the glyph. */
function playbackRateName(rate: number): string {
  return `${rate}x`;
}

/** Keyboard scrub steps on the playback position slider, in seconds. */
const SCRUB_STEP_SECONDS = 1;
const SCRUB_LARGE_STEP_SECONDS = 5;

export function renderAudioMessageAttachment(
  entry: AudioAttachmentImetaEntry | undefined,
  href: string | undefined,
  label: string,
  downloadUrl?: string,
  voiceNoteCard?: VoiceNoteCardContext,
) {
  const attachment = resolveAudioAttachment(entry, href, label);
  if (!attachment) return null;
  const voiceNote = isVoiceNoteAttachment(entry);
  return (
    <AudioMessageAttachment
      {...attachment}
      downloadUrl={voiceNote ? undefined : downloadUrl}
      sender={voiceNoteCard?.sender}
      transcript={voiceNote ? voiceNoteCard?.transcript : undefined}
      transcriptContext={voiceNoteCard?.conversation}
    />
  );
}

function audioMimeForUrl(url: string): string {
  const pathname = url.split("?", 1)[0]?.toLowerCase() ?? "";
  if (pathname.endsWith(".mp4")) return "audio/mp4";
  if (pathname.endsWith(".mp3")) return "audio/mpeg";
  if (pathname.endsWith(".ogg")) return "audio/ogg";
  return "audio/wav";
}

function isAbortError(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

async function decodeSamples(
  url: string,
  signal: AbortSignal,
): Promise<Float32Array> {
  const response = await fetch(url, { signal });
  if (!response.ok) throw new Error(`Audio fetch failed (${response.status})`);
  const bytes = await response.arrayBuffer();
  if (signal.aborted) {
    throw new DOMException("Audio decode cancelled", "AbortError");
  }
  const context = new AudioContext();
  let rejectCancellation: ((reason?: unknown) => void) | undefined;
  const cancellation = new Promise<never>((_resolve, reject) => {
    rejectCancellation = reject;
  });
  const onAbort = () => {
    void context.close().catch(() => undefined);
    rejectCancellation?.(
      new DOMException("Audio decode cancelled", "AbortError"),
    );
  };
  signal.addEventListener("abort", onAbort, { once: true });
  try {
    const buffer = await Promise.race([
      context.decodeAudioData(bytes),
      cancellation,
    ]);
    if (signal.aborted) {
      throw new DOMException("Audio decode cancelled", "AbortError");
    }
    return buffer.getChannelData(0);
  } finally {
    signal.removeEventListener("abort", onAbort);
    await context.close().catch(() => undefined);
  }
}

export function AudioMessageAttachment({
  composer = false,
  duration: taggedDuration,
  downloadUrl,
  filename,
  href,
  onRemove,
  sender,
  transcript,
  transcriptContext = "channel",
}: {
  composer?: boolean;
  duration?: number;
  downloadUrl?: string;
  filename: string;
  href: string;
  onRemove?: () => void;
  /** Display name of the sender, shown as the card title. */
  sender?: string;
  /** Accompanying prose shown in the Transcript row; omitted when empty. */
  transcript?: string;
  /** Decides the transcript default: open in DMs, folded in channels. */
  transcriptContext?: VoiceNoteConversationContext;
}) {
  const audioRef = React.useRef<HTMLAudioElement | null>(null);
  const playbackId = React.useId();
  const transcriptId = React.useId();
  const [transcriptOpen, setTranscriptOpen] = React.useState(() =>
    resolveTranscriptOpen(transcriptContext),
  );
  const toggleTranscript = React.useCallback(() => {
    setTranscriptOpen((open) => {
      writeTranscriptPreference(!open);
      return !open;
    });
  }, []);
  const mediaRef = React.useRef<HTMLDivElement | null>(null);
  const playbackRateRef = React.useRef<HTMLButtonElement | null>(null);
  const waveformRef = React.useRef<HTMLDivElement | null>(null);
  const progressWaveformRef = React.useRef<HTMLDivElement | null>(null);
  const progressFrameRef = React.useRef<number | null>(null);
  const shouldReduceMotion = useReducedMotion();
  const [playbackHref, setPlaybackHref] = React.useState<string | undefined>(
    composer || href.startsWith("blob:") || href.startsWith("data:")
      ? href
      : undefined,
  );
  const [loadRequest, setLoadRequest] = React.useState<
    { attempt: number; href: string } | undefined
  >(
    composer || href.startsWith("blob:") || href.startsWith("data:")
      ? { attempt: 0, href }
      : undefined,
  );
  const [barCount, setBarCount] = React.useState(INITIAL_BAR_COUNT);
  const [duration, setDuration] = React.useState(taggedDuration ?? 0);
  const [currentTime, setCurrentTime] = React.useState(0);
  const [isPlaying, setIsPlaying] = React.useState(false);
  const [playbackRate, setPlaybackRate] = React.useState(1);
  const [playbackError, setPlaybackError] = React.useState(false);
  const [waveformError, setWaveformError] = React.useState(false);
  // A Play click before the source is fetched is remembered here so playback
  // starts automatically once loading resolves, instead of silently no-opping.
  const [pendingPlay, setPendingPlay] = React.useState(false);
  const [waveformSummary, setWaveformSummary] = React.useState<
    Float32Array | undefined
  >();
  const [peaks, setPeaks] = React.useState(() => dotPeaks(INITIAL_BAR_COUNT));
  const [waveformReady, setWaveformReady] = React.useState(false);
  useSmoothCorners(mediaRef);
  useSmoothCorners(playbackRateRef);

  React.useEffect(() => {
    const localHref =
      composer || href.startsWith("blob:") || href.startsWith("data:");
    if (localHref) {
      setLoadRequest({ attempt: 0, href });
      return;
    }

    setLoadRequest(undefined);
    const waveform = waveformRef.current;
    if (!waveform || typeof IntersectionObserver === "undefined") {
      setLoadRequest({ attempt: 0, href });
      return;
    }

    const observer = new IntersectionObserver(
      (entries) => {
        if (!entries.some((entry) => entry.isIntersecting)) return;
        setLoadRequest({ attempt: 0, href });
        observer.disconnect();
      },
      { rootMargin: "240px 0px" },
    );
    observer.observe(waveform);
    return () => observer.disconnect();
  }, [composer, href]);

  React.useEffect(() => {
    if (loadRequest?.href !== href) {
      setPlaybackHref(undefined);
      return;
    }
    if (composer || href.startsWith("blob:") || href.startsWith("data:")) {
      setPlaybackHref(href);
      return;
    }

    let active = true;
    let objectUrl: string | undefined;
    setPlaybackHref(undefined);
    const load = scheduleAudioMediaLoad((signal) =>
      fetchMediaBytes(href, signal),
    );
    void load.promise
      .then((bytes) => {
        if (!active) return;
        objectUrl = URL.createObjectURL(
          new Blob([bytes], { type: audioMimeForUrl(href) }),
        );
        setPlaybackHref(objectUrl);
      })
      .catch((error: unknown) => {
        if (active && !isAbortError(error)) {
          setPlaybackHref(rewriteRelayUrl(href));
        }
      });
    return () => {
      active = false;
      load.cancel();
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  }, [composer, href, loadRequest]);

  React.useEffect(() => {
    const waveform = waveformRef.current;
    if (!waveform) return;
    const updateCount = () => {
      setBarCount(
        Math.min(256, Math.max(1, Math.floor((waveform.clientWidth + 2) / 5))),
      );
    };
    updateCount();
    const observer = new ResizeObserver(updateCount);
    observer.observe(waveform);
    return () => observer.disconnect();
  }, []);

  React.useEffect(() => {
    if (!playbackHref) return;
    let active = true;
    setWaveformReady(false);
    setWaveformError(false);
    setWaveformSummary(undefined);
    const load = scheduleAudioMediaLoad((signal) =>
      decodeSamples(playbackHref, signal),
    );
    void load.promise
      .then((samples) => {
        if (!active) return;
        setWaveformSummary(summarizeWaveform(samples));
      })
      .catch((error: unknown) => {
        if (active && !isAbortError(error)) setWaveformError(true);
      });
    return () => {
      active = false;
      load.cancel();
    };
  }, [playbackHref]);

  React.useEffect(() => {
    setPeaks(
      waveformSummary
        ? waveformPeaks(waveformSummary, barCount)
        : dotPeaks(barCount),
    );
    if (waveformSummary) setWaveformReady(true);
  }, [barCount, waveformSummary]);

  React.useEffect(() => {
    const handleOtherPlayback = (event: Event) => {
      const detail = (event as CustomEvent<string>).detail;
      if (detail !== playbackId) audioRef.current?.pause();
    };
    window.addEventListener(PLAY_EVENT, handleOtherPlayback);
    return () => window.removeEventListener(PLAY_EVENT, handleOtherPlayback);
  }, [playbackId]);

  const paintProgress = React.useCallback((time: number, knownDuration = 0) => {
    const audio = audioRef.current;
    const progressWaveform = progressWaveformRef.current;
    if (!progressWaveform) return;
    const audioDuration =
      knownDuration > 0
        ? knownDuration
        : audio && Number.isFinite(audio.duration)
          ? audio.duration
          : 0;
    const ratio = audioDuration > 0 ? time / audioDuration : 0;
    const remaining = Math.max(0, Math.min(1, 1 - ratio)) * 100;
    progressWaveform.style.clipPath = `inset(0 ${remaining}% 0 0)`;
  }, []);

  React.useEffect(() => {
    if (!isPlaying) {
      if (progressFrameRef.current !== null) {
        window.cancelAnimationFrame(progressFrameRef.current);
        progressFrameRef.current = null;
      }
      return;
    }

    const paintFrame = () => {
      const audio = audioRef.current;
      if (!audio || audio.paused) {
        progressFrameRef.current = null;
        return;
      }
      paintProgress(audio.currentTime);
      progressFrameRef.current = window.requestAnimationFrame(paintFrame);
    };
    progressFrameRef.current = window.requestAnimationFrame(paintFrame);
    return () => {
      if (progressFrameRef.current !== null) {
        window.cancelAnimationFrame(progressFrameRef.current);
        progressFrameRef.current = null;
      }
    };
  }, [isPlaying, paintProgress]);

  const startPlayback = React.useCallback(() => {
    const audio = audioRef.current;
    if (!audio) return;
    window.dispatchEvent(new CustomEvent(PLAY_EVENT, { detail: playbackId }));
    void audio.play().catch(() => {
      setIsPlaying(false);
      setPlaybackError(true);
    });
  }, [playbackId]);

  const togglePlayback = React.useCallback(() => {
    const audio = audioRef.current;
    if (!audio) return;
    if (!playbackHref) {
      // Source not fetched yet: request the load and remember the intent so
      // playback begins as soon as it arrives, rather than dropping the click.
      setPendingPlay(true);
      setLoadRequest((request) =>
        request?.href === href ? request : { attempt: 0, href },
      );
      return;
    }
    if (audio.paused) {
      startPlayback();
    } else {
      setPendingPlay(false);
      audio.pause();
    }
  }, [href, playbackHref, startPlayback]);

  // Fulfill a Play click that landed before the source finished loading.
  React.useEffect(() => {
    if (!pendingPlay || !playbackHref) return;
    setPendingPlay(false);
    startPlayback();
  }, [pendingPlay, playbackHref, startPlayback]);

  // Drop a pending intent if playback itself fails, so the button leaves its
  // loading state and the user can retry. Waveform decode failure is unrelated
  // to playback and must not cancel the intent.
  React.useEffect(() => {
    if (playbackError) setPendingPlay(false);
  }, [playbackError]);

  const retryPlayback = React.useCallback(() => {
    setPlaybackError(false);
    setWaveformError(false);
    setPlaybackHref(undefined);
    setLoadRequest((request) => ({
      attempt: request?.href === href ? request.attempt + 1 : 0,
      href,
    }));
  }, [href]);

  const timeLabel = `${formatVoiceNoteDuration(currentTime)} / ${formatVoiceNoteDuration(duration)}`;
  const nextPlaybackRate = nextVoiceNotePlaybackRate(playbackRate);
  const seekTo = React.useCallback(
    (next: number, knownDuration: number) => {
      const clamped = Math.max(0, Math.min(next, knownDuration));
      if (audioRef.current && Number.isFinite(clamped)) {
        audioRef.current.currentTime = clamped;
      }
      setCurrentTime(clamped);
      paintProgress(clamped, knownDuration);
    },
    [paintProgress],
  );
  const handleScrubKeyDown = React.useCallback(
    (event: React.KeyboardEvent<HTMLInputElement>) => {
      if (event.altKey || event.ctrlKey || event.metaKey) return;
      const max = Number(event.currentTarget.max);
      const current = Number(event.currentTarget.value);
      const step = event.shiftKey
        ? SCRUB_LARGE_STEP_SECONDS
        : SCRUB_STEP_SECONDS;
      let next: number | null = null;
      switch (event.key) {
        case "ArrowRight":
        case "ArrowUp":
          next = current + step;
          break;
        case "ArrowLeft":
        case "ArrowDown":
          next = current - step;
          break;
        case "Home":
          next = 0;
          break;
        case "End":
          next = max;
          break;
        default:
          return;
      }
      event.preventDefault();
      seekTo(next, max);
    },
    [seekTo],
  );

  const waveformBars = React.useCallback(
    (active: boolean) =>
      peaks.map((peak, index) => (
        <motion.span
          animate={{ height: voiceNoteBarHeight(peak) }}
          aria-hidden="true"
          className={cn(
            "w-[3px] shrink-0",
            active ? "bg-primary" : "bg-muted-foreground/35",
          )}
          initial={false}
          key={BAR_KEYS[index]}
          style={{ borderRadius: "9999px" }}
          transition={
            shouldReduceMotion
              ? { duration: 0 }
              : { duration: 0.24, ease: [0.23, 1, 0.32, 1] }
          }
        />
      )),
    [peaks, shouldReduceMotion],
  );

  const speedPill = !composer ? (
    <button
      ref={playbackRateRef}
      aria-label={`Playback speed ${playbackRateName(playbackRate)}; next ${playbackRateName(nextPlaybackRate)}`}
      className="grid rounded-full bg-primary px-2.5 py-0.5 text-2xs font-semibold tabular-nums text-primary-foreground transition-transform duration-150 ease-out active:scale-95 focus-visible:outline-hidden focus-visible:ring-1 focus-visible:ring-ring focus-visible:ring-offset-1 motion-reduce:transition-none motion-reduce:active:scale-100"
      data-testid="voice-note-playback-rate"
      onClick={() => {
        const next = nextVoiceNotePlaybackRate(playbackRate);
        setPlaybackRate(next);
        if (audioRef.current) {
          audioRef.current.defaultPlaybackRate = next;
          audioRef.current.playbackRate = next;
        }
      }}
      type="button"
    >
      <span aria-hidden="true" className="invisible col-start-1 row-start-1">
        1.5×
      </span>
      <span
        className="col-start-1 row-start-1 text-center"
        data-testid="voice-note-playback-rate-value"
      >
        {playbackRateLabel(playbackRate)}
      </span>
    </button>
  ) : null;

  return (
    <Attachment
      className={cn(
        "my-1 w-full gap-2 px-2.5 py-2",
        composer ? "max-w-[21rem] shadow-none" : "max-w-[32rem]",
      )}
      data-testid={
        composer ? "composer-voice-note-card" : "audio-message-attachment"
      }
      orientation="vertical"
      size="sm"
    >
      <div className="flex w-full min-w-0 items-center gap-2.5">
        <AttachmentMedia
          ref={mediaRef}
          className="rounded-lg bg-primary text-primary-foreground"
          data-testid="voice-note-playback-control"
        >
          <button
            aria-label={
              playbackError
                ? "Retry voice note"
                : pendingPlay
                  ? "Loading voice note"
                  : isPlaying
                    ? "Pause voice note"
                    : "Play voice note"
            }
            className="flex h-full w-full items-center justify-center rounded-md focus-visible:outline-hidden focus-visible:ring-2 focus-visible:ring-ring"
            onClick={playbackError ? retryPlayback : togglePlayback}
            type="button"
          >
            {playbackError ? (
              <AlertCircle aria-hidden="true" />
            ) : pendingPlay ? (
              <Loader2 aria-hidden="true" className="animate-spin" />
            ) : (
              <MorphingPlayPauseIcon isPlaying={isPlaying} />
            )}
          </button>
        </AttachmentMedia>
        <AttachmentContent className="min-w-0">
          <AttachmentTitle className="sr-only">{filename}</AttachmentTitle>
          {playbackError ? (
            <div className="text-xs font-medium text-destructive" role="alert">
              Audio unavailable. Retry playback.
            </div>
          ) : (
            <div
              className="relative h-6 overflow-hidden rounded-sm has-[:focus-visible]:ring-2 has-[:focus-visible]:ring-ring has-[:focus-visible]:ring-offset-1"
              data-testid="voice-note-playback-waveform"
              data-waveform-state={
                waveformError ? "error" : waveformReady ? "ready" : "loading"
              }
              ref={waveformRef}
            >
              {waveformError ? (
                <span className="sr-only" role="status">
                  Waveform preview unavailable. Playback may still work.
                </span>
              ) : null}
              <div className="flex h-full items-center gap-0.5">
                {waveformBars(false)}
              </div>
              <div
                aria-hidden="true"
                className="pointer-events-none absolute inset-0 flex items-center gap-0.5 will-change-[clip-path]"
                data-testid="voice-note-progress-waveform"
                ref={progressWaveformRef}
                style={{ clipPath: "inset(0 100% 0 0)" }}
              >
                {waveformBars(true)}
              </div>
              <input
                aria-label="Voice note playback position"
                aria-valuetext={timeLabel}
                className="absolute inset-0 h-full w-full cursor-pointer opacity-0"
                max={Math.max(duration, 0.01)}
                min="0"
                onInput={(event) => {
                  seekTo(
                    Number(event.currentTarget.value),
                    Number(event.currentTarget.max),
                  );
                }}
                onKeyDown={handleScrubKeyDown}
                step="0.01"
                type="range"
                value={Math.min(currentTime, Math.max(duration, 0.01))}
              />
            </div>
          )}
          {!composer ? (
            <div
              className="mt-1 flex min-w-0 items-center gap-2 text-xs text-muted-foreground"
              data-testid="voice-note-meta"
            >
              {sender ? (
                <>
                  <span
                    className="truncate font-medium text-foreground"
                    data-testid="voice-note-sender"
                  >
                    {sender}
                  </span>
                  <span aria-hidden="true">·</span>
                </>
              ) : null}
              <span
                className="shrink-0 tabular-nums"
                data-testid="voice-note-time"
              >
                {timeLabel}
              </span>
            </div>
          ) : null}
        </AttachmentContent>
        {composer ? (
          <AttachmentActions className="min-w-9 justify-end">
            <span
              className="text-xs tabular-nums text-muted-foreground"
              data-testid="voice-note-time"
            >
              {timeLabel}
            </span>
          </AttachmentActions>
        ) : (
          <AttachmentActions>{speedPill}</AttachmentActions>
        )}
        {!composer && downloadUrl ? (
          <AttachmentActions>
            <AttachmentAction
              aria-label={`Download ${filename}`}
              onClick={() => {
                invokeTauri("download_file", {
                  filename,
                  url: downloadUrl,
                }).catch((error: unknown) => {
                  toast.error(
                    error instanceof Error ? error.message : "Download failed",
                  );
                });
              }}
              title="Download"
              type="button"
            >
              <Download />
            </AttachmentAction>
          </AttachmentActions>
        ) : null}
        {!composer && onRemove ? (
          <AttachmentActions>
            <AttachmentAction
              aria-label="Remove voice note"
              onClick={onRemove}
              title="Remove"
              type="button"
            >
              <X />
            </AttachmentAction>
          </AttachmentActions>
        ) : null}
      </div>
      {!composer && transcript ? (
        <div
          className="w-full border-t border-border/60 pt-2"
          data-testid="voice-note-transcript"
        >
          <button
            aria-controls={transcriptId}
            aria-expanded={transcriptOpen}
            className="flex w-full items-center justify-between gap-2 rounded-sm text-xs font-medium text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-hidden focus-visible:ring-1 focus-visible:ring-ring"
            data-testid="voice-note-transcript-toggle"
            onClick={toggleTranscript}
            type="button"
          >
            <span className="inline-flex items-center gap-2">
              <FileText aria-hidden="true" className="h-3.5 w-3.5" />
              Transcript
            </span>
            <ChevronDown
              aria-hidden="true"
              className={cn(
                "h-4 w-4 transition-transform motion-reduce:transition-none",
                transcriptOpen && "rotate-180",
              )}
            />
          </button>
          <p
            className="mt-1.5 whitespace-pre-wrap pl-[1.375rem] text-xs leading-relaxed text-muted-foreground"
            data-testid="voice-note-transcript-text"
            hidden={!transcriptOpen}
            id={transcriptId}
          >
            {transcript}
          </p>
        </div>
      ) : null}
      {/* biome-ignore lint/a11y/useMediaCaption: voice notes are user-provided audio */}
      <audio
        onDurationChange={(event) => {
          const next = event.currentTarget.duration;
          if (Number.isFinite(next)) {
            setDuration(next);
            paintProgress(event.currentTarget.currentTime, next);
          }
        }}
        onEnded={() => {
          setCurrentTime(0);
          setIsPlaying(false);
          paintProgress(0);
        }}
        onPause={() => setIsPlaying(false)}
        onPlay={() => {
          setPlaybackError(false);
          setIsPlaying(true);
        }}
        onError={() => {
          setIsPlaying(false);
          setPlaybackError(true);
        }}
        onTimeUpdate={(event) =>
          setCurrentTime(event.currentTarget.currentTime)
        }
        preload="metadata"
        ref={audioRef}
        src={playbackHref}
      />
    </Attachment>
  );
}
