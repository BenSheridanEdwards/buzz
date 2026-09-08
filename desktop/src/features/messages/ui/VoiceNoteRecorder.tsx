import * as React from "react";
import { useReducedMotion } from "motion/react";
import { Lock, Pause, Play, Trash2, X } from "lucide-react";

import {
  formatVoiceNoteDuration,
  voiceNoteBarHeight,
} from "@/features/messages/lib/audioAttachment";
import { cn } from "@/shared/lib/cn";
import { Button } from "@/shared/ui/button";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/shared/ui/tooltip";
import type { VoiceNoteHoldSource } from "./useComposerVoiceNote";

const BAR_PITCH_PX = 5;
const chipClassName =
  "inline-flex shrink-0 items-center gap-1.5 rounded-full border border-border/70 px-2.5 py-1 text-xs text-muted-foreground";

type WaveformSample = {
  id: string;
  level: number;
  recorded: boolean;
};

export function VoiceNoteRecorder({
  containerRef,
  elapsedSeconds,
  holdSource,
  levels,
  locked,
  maxDurationSeconds,
  onCancel,
  onLock,
  onPause,
  onResume,
  paused,
  processing,
  requesting,
}: {
  /** The row itself, so the composer can tell whether focus lived in it. */
  containerRef?: React.Ref<HTMLFieldSetElement>;
  elapsedSeconds: number;
  /**
   * Which hand is busy holding. A pointer hold cannot click the chip (the
   * button is down on the mic), so the chip becomes a slide-to-lock target.
   */
  holdSource: VoiceNoteHoldSource | null;
  levels: number[];
  /** Hands-free: the hold was released and the recording waits for Send. */
  locked: boolean;
  maxDurationSeconds: number;
  onCancel: () => void;
  onLock: () => void;
  onPause: () => void;
  onResume: () => void;
  paused: boolean;
  processing: boolean;
  requesting: boolean;
}) {
  const waveformRef = React.useRef<HTMLDivElement | null>(null);
  const trackRef = React.useRef<HTMLDivElement | null>(null);
  const rowRef = React.useRef<HTMLFieldSetElement | null>(null);
  const pauseResumeRef = React.useRef<HTMLButtonElement | null>(null);
  const previousFrameRef = React.useRef({ barCount: 0, levelCount: 0 });
  const prefersReducedMotion = useReducedMotion();
  const [barCount, setBarCount] = React.useState(1);

  const setRowRef = React.useCallback(
    (node: HTMLFieldSetElement | null) => {
      rowRef.current = node;
      if (typeof containerRef === "function") containerRef(node);
      else if (containerRef) containerRef.current = node;
    },
    [containerRef],
  );

  // Locking swaps the chip for a read-only badge. If the chip had focus
  // (Enter on it), focus moves to pause/resume, the control that took the
  // chip's place, rather than dying on the document body. Focus elsewhere
  // (the editor, or nowhere during a pointer hold) is left alone. The chip's
  // own focus events remember this: by the time the lock effect runs the chip
  // has unmounted and `document.activeElement` is already the body.
  const chipHadFocusRef = React.useRef(false);
  const handleChipFocus = React.useCallback(() => {
    chipHadFocusRef.current = true;
  }, []);
  const handleChipBlur = React.useCallback(() => {
    chipHadFocusRef.current = false;
  }, []);
  const wasLockedRef = React.useRef(locked);
  React.useEffect(() => {
    const wasLocked = wasLockedRef.current;
    wasLockedRef.current = locked;
    if (!locked || wasLocked || !chipHadFocusRef.current) return;
    chipHadFocusRef.current = false;
    pauseResumeRef.current?.focus();
  }, [locked]);

  React.useEffect(() => {
    const waveform = waveformRef.current;
    if (!waveform) return;
    const updateCount = () => {
      setBarCount(
        Math.min(
          1024,
          Math.max(1, Math.floor((waveform.clientWidth + 2) / BAR_PITCH_PX)),
        ),
      );
    };
    updateCount();
    const observer = new ResizeObserver(updateCount);
    observer.observe(waveform);
    return () => observer.disconnect();
  }, []);

  const visibleSamples = React.useMemo<WaveformSample[]>(() => {
    const startIndex = levels.length - barCount;
    return Array.from({ length: barCount }, (_, slot) => {
      const sampleIndex = startIndex + slot;
      return sampleIndex < 0
        ? {
            id: `baseline-${sampleIndex}`,
            level: 0,
            recorded: false,
          }
        : {
            id: `recorded-${sampleIndex}`,
            level: levels[sampleIndex] ?? 0,
            recorded: true,
          };
    });
  }, [barCount, levels]);

  React.useLayoutEffect(() => {
    const track = trackRef.current;
    if (!track) return;
    const previous = previousFrameRef.current;
    const shouldAdvance =
      !prefersReducedMotion &&
      previous.barCount === barCount &&
      levels.length === previous.levelCount + 1;
    previousFrameRef.current = { barCount, levelCount: levels.length };

    track.style.transition = "none";
    track.style.transform = shouldAdvance
      ? `translate3d(${BAR_PITCH_PX}px, 0, 0)`
      : "translate3d(0, 0, 0)";
    if (!shouldAdvance) return;

    const frame = window.requestAnimationFrame(() => {
      track.style.transition = "transform 90ms linear";
      track.style.transform = "translate3d(0, 0, 0)";
    });
    return () => window.cancelAnimationFrame(frame);
  }, [barCount, levels.length, prefersReducedMotion]);

  const live = !requesting && !processing;
  const canPauseResume = live && locked;

  return (
    <fieldset
      className="flex min-w-0 flex-1 items-center gap-1"
      data-testid="voice-note-recorder"
      ref={setRowRef}
      data-voice-note-state={
        requesting
          ? "requesting"
          : processing
            ? "processing"
            : paused
              ? "paused"
              : locked
                ? "locked"
                : "recording"
      }
    >
      <legend className="sr-only">
        {requesting
          ? "Waiting for microphone access"
          : processing
            ? "Preparing voice note"
            : paused
              ? "Voice note paused"
              : "Recording voice note"}
      </legend>
      <Tooltip disableHoverableContent>
        <TooltipTrigger asChild>
          <Button
            aria-label="Discard voice note"
            className={cn(
              "shrink-0",
              locked && "text-destructive hover:text-destructive",
            )}
            data-testid="voice-note-discard"
            onClick={onCancel}
            size="icon"
            type="button"
            variant="ghost"
          >
            {locked ? <Trash2 /> : <X />}
          </Button>
        </TooltipTrigger>
        <TooltipContent>Discard voice note</TooltipContent>
      </Tooltip>
      <div className="mx-1 h-5 w-px shrink-0 bg-border/60" />
      {canPauseResume ? (
        <Tooltip disableHoverableContent>
          <TooltipTrigger asChild>
            <Button
              aria-label={paused ? "Resume recording" : "Pause recording"}
              className="shrink-0 rounded-full bg-muted"
              data-testid="voice-note-pause-resume"
              onClick={paused ? onResume : onPause}
              ref={pauseResumeRef}
              size="icon"
              type="button"
              variant="ghost"
            >
              {paused ? <Play /> : <Pause />}
            </Button>
          </TooltipTrigger>
          <TooltipContent>
            {paused ? "Resume recording" : "Pause recording"}
          </TooltipContent>
        </Tooltip>
      ) : live ? (
        <span
          aria-hidden="true"
          className={cn(
            "mx-2 h-2 w-2 shrink-0 rounded-full bg-destructive ring-4 ring-destructive/25",
            !prefersReducedMotion && "animate-pulse",
          )}
          data-testid="voice-note-live-dot"
        />
      ) : null}
      {requesting || processing ? (
        <span className="shrink-0 whitespace-nowrap text-xs font-medium text-muted-foreground">
          {requesting ? "Waiting for microphone…" : "Preparing voice note…"}
        </span>
      ) : (
        <span className="shrink-0 whitespace-nowrap text-xs font-medium tabular-nums text-foreground">
          {formatVoiceNoteDuration(
            Math.min(elapsedSeconds, maxDurationSeconds),
          )}
          <span className="text-muted-foreground">
            {` / ${formatVoiceNoteDuration(maxDurationSeconds)}`}
          </span>
        </span>
      )}
      <div
        aria-hidden="true"
        className="h-6 min-w-0 flex-1 overflow-hidden"
        data-testid="voice-note-live-waveform"
        ref={waveformRef}
        style={{
          maskImage:
            "linear-gradient(to right, transparent, black 12px, black calc(100% - 12px), transparent)",
          WebkitMaskImage:
            "linear-gradient(to right, transparent, black 12px, black calc(100% - 12px), transparent)",
        }}
      >
        <div
          className="flex h-full items-center gap-0.5 will-change-transform"
          data-testid="voice-note-waveform-strip"
          ref={trackRef}
        >
          {visibleSamples.map((sample) => (
            <span
              className="w-[3px] shrink-0 rounded-full bg-primary/75"
              data-recorded-sample={sample.recorded ? "true" : undefined}
              data-waveform-sample={sample.id}
              key={sample.id}
              style={{ height: `${voiceNoteBarHeight(sample.level)}px` }}
            />
          ))}
        </div>
      </div>
      {live ? (
        locked ? (
          <span
            className="inline-flex shrink-0 items-center gap-1.5 rounded-full bg-emerald-500/15 px-2.5 py-1 text-xs font-medium text-emerald-600 dark:text-emerald-400"
            data-testid="voice-note-lock-chip"
            data-locked="true"
          >
            <Lock aria-hidden="true" className="h-3 w-3" />
            {paused ? "Paused" : "Locked"}
          </span>
        ) : holdSource === "pointer" ? (
          // The mouse button is down on the mic, so a click here is
          // impossible (releasing sends). Sliding the held pointer onto the
          // chip locks instead; L still works from the keyboard.
          <span
            className={cn(chipClassName, "cursor-default")}
            data-testid="voice-note-lock-chip"
            data-locked="false"
            data-lock-target="slide"
            onPointerEnter={onLock}
          >
            <Lock aria-hidden="true" className="h-3 w-3" />
            <span>
              Slide here or press{" "}
              <kbd className="font-semibold text-foreground">L</kbd> to lock
            </span>
          </span>
        ) : (
          <Tooltip disableHoverableContent>
            <TooltipTrigger asChild>
              <button
                className={cn(
                  chipClassName,
                  "transition-colors hover:bg-accent hover:text-accent-foreground focus-visible:outline-hidden focus-visible:ring-1 focus-visible:ring-ring",
                )}
                data-testid="voice-note-lock-chip"
                data-locked="false"
                data-lock-target="press"
                onBlur={handleChipBlur}
                onClick={onLock}
                onFocus={handleChipFocus}
                type="button"
              >
                <Lock aria-hidden="true" className="h-3 w-3" />
                <span>
                  Press <kbd className="font-semibold text-foreground">L</kbd>{" "}
                  to lock
                </span>
              </button>
            </TooltipTrigger>
            <TooltipContent>Record hands free</TooltipContent>
          </Tooltip>
        )
      ) : null}
    </fieldset>
  );
}
