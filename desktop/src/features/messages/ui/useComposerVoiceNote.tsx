import * as React from "react";
import { ArrowUp, RotateCcw } from "lucide-react";
import { toast } from "sonner";

import {
  isVoiceNoteAttachment,
  isVoiceNoteFile,
  VOICE_NOTE_MAX_DURATION_SECONDS,
} from "@/features/messages/lib/audioAttachment";
import type { MediaUploadController } from "@/features/messages/lib/useMediaUpload";
import { useVoiceNoteRecorder } from "@/features/messages/lib/useVoiceNoteRecorder";
import { useVoiceNoteReviewEnabled } from "@/features/messages/lib/voiceNoteReviewPreference";
import { Button } from "@/shared/ui/button";
import { VoiceNoteRecorder } from "./VoiceNoteRecorder";

/**
 * How a hold-to-record session was started. Pointer holds end on the matching
 * pointer release; keyboard holds end when Space is released. Both end when
 * the window loses focus (the hold cannot be observed any more), sending the
 * note unless it was locked first.
 */
export type VoiceNoteHoldSource = "pointer" | "keyboard";

type Hold = {
  release: () => void;
  source: VoiceNoteHoldSource;
};

function isPlainKey(event: {
  altKey: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
  shiftKey: boolean;
}): boolean {
  return !event.altKey && !event.ctrlKey && !event.metaKey && !event.shiftKey;
}

function isSpaceKey(event: { code: string; key: string }): boolean {
  return event.key === " " || event.code === "Space";
}

function isLockKey(event: { key: string }): boolean {
  return event.key === "l" || event.key === "L";
}

function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  return (
    target.isContentEditable ||
    target instanceof HTMLInputElement ||
    target instanceof HTMLTextAreaElement ||
    target instanceof HTMLSelectElement
  );
}

export function useComposerVoiceNote({
  draftKey,
  editTargetId,
  media,
  setEmojiPickerOpen,
  setFormattingOpen,
  submit,
}: {
  draftKey: string | null | undefined;
  editTargetId: string | null;
  media: MediaUploadController;
  setEmojiPickerOpen: (open: boolean) => void;
  setFormattingOpen: (open: boolean) => void;
  /** Submit the composer once a finished note has been queued (review off). */
  submit: () => void;
}) {
  const recorder = useVoiceNoteRecorder();
  const reviewEnabled = useVoiceNoteReviewEnabled();
  const reviewEnabledRef = React.useRef(reviewEnabled);
  reviewEnabledRef.current = reviewEnabled;
  const submitRef = React.useRef(submit);
  submitRef.current = submit;
  const limitReachedRef = React.useRef(false);
  const statusRef = React.useRef(recorder.status);
  statusRef.current = recorder.status;
  const startRef = React.useRef(recorder.start);
  startRef.current = recorder.start;
  // Mirrors `recorder.locked`, but set eagerly by `lock()` so a release that
  // lands in the same tick as the lock never sends.
  const lockedRef = React.useRef(recorder.locked);
  lockedRef.current = recorder.locked;
  const holdRef = React.useRef<Hold | null>(null);
  const lockKeyHeldRef = React.useRef(false);
  const pendingSubmitRef = React.useRef(false);
  const getAttachments = React.useCallback(
    () => ({
      pending: media.pendingImetaRef.current,
      queued: media.queuedAttachmentsRef.current,
    }),
    [media.pendingImetaRef, media.queuedAttachmentsRef],
  );
  const getAttachmentsRef = React.useRef(getAttachments);
  getAttachmentsRef.current = getAttachments;
  const onBeforeStartRef = React.useRef(() => {});
  onBeforeStartRef.current = () => {
    setEmojiPickerOpen(false);
    setFormattingOpen(false);
  };
  const currentContextRef = React.useRef({ draftKey, editTargetId });
  currentContextRef.current = { draftKey, editTargetId };
  const recordingContextRef = React.useRef({ draftKey, editTargetId });

  const releaseHold = React.useCallback(() => {
    const hold = holdRef.current;
    if (!hold) return null;
    holdRef.current = null;
    hold.release();
    return hold;
  }, []);

  const discard = React.useCallback(() => {
    releaseHold();
    lockKeyHeldRef.current = false;
    pendingSubmitRef.current = false;
    recorder.cancel();
  }, [recorder.cancel, releaseHold]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: composer identity fields are the cancellation triggers
  React.useEffect(() => {
    discard();
  }, [draftKey, editTargetId]);

  React.useEffect(() => {
    if (recorder.error) toast.error(recorder.error);
  }, [recorder.error]);

  const finish = React.useCallback(async () => {
    const recording = await recorder.stop();
    const recordingContext = recordingContextRef.current;
    const currentContext = currentContextRef.current;
    if (
      recording &&
      recordingContext.draftKey === currentContext.draftKey &&
      recordingContext.editTargetId === currentContext.editTargetId
    ) {
      await media.uploadFile(recording.file);
    }
    return recording;
  }, [recorder.stop, media.uploadFile]);

  /**
   * Finish the recording and, unless the review setting is on, submit the
   * composer as soon as the queued note is visible to the send path.
   */
  const send = React.useCallback(async () => {
    releaseHold();
    lockKeyHeldRef.current = false;
    const status = statusRef.current;
    if (status !== "recording" && status !== "paused") return null;
    const recording = await finish();
    if (recording && !reviewEnabledRef.current) {
      pendingSubmitRef.current = true;
    }
    return recording;
  }, [finish, releaseHold]);
  const sendRef = React.useRef(send);
  sendRef.current = send;

  React.useEffect(() => {
    if (recorder.status === "idle") limitReachedRef.current = false;
    if (
      recorder.status === "recording" &&
      recorder.elapsedSeconds >= VOICE_NOTE_MAX_DURATION_SECONDS &&
      !limitReachedRef.current
    ) {
      limitReachedRef.current = true;
      void send();
    }
  }, [send, recorder.elapsedSeconds, recorder.status]);

  const acceptsStart = React.useCallback(() => {
    const attachments = getAttachmentsRef.current();
    if (attachments.pending.length > 0 || attachments.queued.length > 0) {
      toast.error("A voice note must be the only attachment.");
      return false;
    }
    return true;
  }, []);

  const beginRecording = React.useCallback(
    ({ locked }: { locked: boolean }) => {
      recordingContextRef.current = currentContextRef.current;
      onBeforeStartRef.current();
      lockedRef.current = locked;
      void startRef.current();
      if (locked) recorder.lock();
    },
    [recorder.lock],
  );

  const endHold = React.useCallback(() => {
    const hold = releaseHold();
    if (!hold) return;
    if (lockedRef.current) return;
    const status = statusRef.current;
    if (status === "requesting") {
      // The microphone never arrived while the hold lasted: nothing to send.
      recorder.cancel();
      return;
    }
    if (status === "recording" || status === "paused") void sendRef.current();
  }, [recorder.cancel, releaseHold]);
  const endHoldRef = React.useRef(endHold);
  endHoldRef.current = endHold;

  const beginHold = React.useCallback(
    (source: VoiceNoteHoldSource) => {
      if (holdRef.current || statusRef.current !== "idle") return;
      if (!acceptsStart()) return;
      const onRelease = () => endHoldRef.current();
      const onKeyUp = (event: KeyboardEvent) => {
        if (isSpaceKey(event)) endHoldRef.current();
      };
      const onVisibilityChange = () => {
        if (document.visibilityState === "hidden") endHoldRef.current();
      };
      window.addEventListener("blur", onRelease);
      document.addEventListener("visibilitychange", onVisibilityChange);
      if (source === "pointer") {
        window.addEventListener("pointerup", onRelease);
        window.addEventListener("pointercancel", onRelease);
      } else {
        window.addEventListener("keyup", onKeyUp);
      }
      holdRef.current = {
        release: () => {
          window.removeEventListener("blur", onRelease);
          document.removeEventListener("visibilitychange", onVisibilityChange);
          window.removeEventListener("pointerup", onRelease);
          window.removeEventListener("pointercancel", onRelease);
          window.removeEventListener("keyup", onKeyUp);
        },
        source,
      };
      beginRecording({ locked: false });
    },
    [acceptsStart, beginRecording],
  );

  const beginPointerHold = React.useCallback(
    () => beginHold("pointer"),
    [beginHold],
  );

  /**
   * Click activation (Enter or Space on the focused mic, or assistive
   * technology) has no release to wait for, so it starts hands free.
   */
  const startLocked = React.useCallback(() => {
    if (holdRef.current || statusRef.current !== "idle") return;
    if (!acceptsStart()) return;
    beginRecording({ locked: true });
  }, [acceptsStart, beginRecording]);

  const lock = React.useCallback(() => {
    const status = statusRef.current;
    if (status !== "recording" && status !== "requesting") return;
    if (lockedRef.current) return;
    lockedRef.current = true;
    recorder.lock();
  }, [recorder.lock]);

  const releaseKeyboardHold = React.useCallback(() => {
    if (holdRef.current?.source === "keyboard") endHoldRef.current();
  }, []);

  /**
   * Editor key path. Space (plain, editor empty) holds to record; Esc discards;
   * L locks a live hold. Returns true when the key was consumed.
   */
  const handleEditorKeyDown = React.useCallback(
    (
      event: React.KeyboardEvent<HTMLElement>,
      { editorEmpty }: { editorEmpty: boolean },
    ): boolean => {
      const status = statusRef.current;
      if (isSpaceKey(event)) {
        // Shift+Space and other chords are not Space: they keep typing.
        if (!isPlainKey(event)) return false;
        if (holdRef.current?.source === "keyboard") {
          // Auto-repeat while the hold lasts must not type spaces.
          event.preventDefault();
          return true;
        }
        if (status !== "idle" || !editorEmpty || event.repeat) return false;
        event.preventDefault();
        beginHold("keyboard");
        return true;
      }
      if (status === "idle") return false;
      if (event.key === "Escape") {
        event.preventDefault();
        discard();
        return true;
      }
      if (isLockKey(event) && isPlainKey(event)) {
        if (lockKeyHeldRef.current) {
          // Holding L: swallow the auto-repeat instead of typing "l".
          event.preventDefault();
          return true;
        }
        if (event.repeat || lockedRef.current) return false;
        if (status !== "recording" && status !== "requesting") return false;
        event.preventDefault();
        lockKeyHeldRef.current = true;
        lock();
        return true;
      }
      return false;
    },
    [beginHold, discard, lock],
  );

  // Outside the editor (a focused chip or toolbar control), Esc still discards
  // and L still locks; the editor path above handles keys typed into it.
  const active = recorder.status !== "idle";
  React.useEffect(() => {
    if (!active) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.defaultPrevented) return;
      if (event.key === "Escape") {
        event.preventDefault();
        discard();
        return;
      }
      if (
        isLockKey(event) &&
        isPlainKey(event) &&
        !event.repeat &&
        !isEditableTarget(event.target)
      ) {
        lockKeyHeldRef.current = true;
        lock();
      }
    };
    const onKeyUp = (event: KeyboardEvent) => {
      if (isLockKey(event)) lockKeyHeldRef.current = false;
    };
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("keyup", onKeyUp);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("keyup", onKeyUp);
      lockKeyHeldRef.current = false;
    };
  }, [active, discard, lock]);

  React.useEffect(
    () => () => {
      releaseHold();
    },
    [releaseHold],
  );

  const attachments = getAttachments();
  const hasAttachment =
    attachments.pending.some((attachment) =>
      isVoiceNoteAttachment({
        filename: attachment.filename,
        m: attachment.type,
      }),
    ) || attachments.queued.some(({ file }) => isVoiceNoteFile(file));
  const hasAttachmentRef = React.useRef(hasAttachment);
  hasAttachmentRef.current = hasAttachment;

  // With review off, the note sends itself once the recorder is idle and the
  // queued file is visible to the send path (both land after `finish`).
  React.useEffect(() => {
    if (!pendingSubmitRef.current) return;
    if (recorder.status !== "idle" || !hasAttachment) return;
    pendingSubmitRef.current = false;
    submitRef.current();
  }, [hasAttachment, recorder.status]);

  const acceptsNewAttachment = React.useCallback(() => {
    const attachments = getAttachmentsRef.current();
    const hasVoiceNoteAttachment =
      attachments.pending.some((attachment) =>
        isVoiceNoteAttachment({
          filename: attachment.filename,
          m: attachment.type,
        }),
      ) || attachments.queued.some(({ file }) => isVoiceNoteFile(file));
    if (statusRef.current !== "idle" || hasVoiceNoteAttachment) {
      toast.error(
        statusRef.current === "idle"
          ? "A voice note must be the only attachment."
          : "Finish or discard the voice note before attaching a file.",
      );
      return false;
    }
    return true;
  }, []);

  const uploadFileWhenIdle = React.useCallback(
    async (file: File) => {
      if (!acceptsNewAttachment()) return;
      await media.uploadFile(file);
    },
    [acceptsNewAttachment, media.uploadFile],
  );
  const setPendingImetaWhenIdle = React.useCallback(
    (update: Parameters<typeof media.setPendingImeta>[0]) => {
      if (acceptsNewAttachment()) media.setPendingImeta(update);
    },
    [acceptsNewAttachment, media.setPendingImeta],
  );

  const rerecord = React.useCallback(() => {
    if (statusRef.current !== "idle") return;
    const attachments = getAttachmentsRef.current();
    for (const attachment of attachments.queued) {
      if (isVoiceNoteFile(attachment.file)) {
        media.removeQueuedAttachment(attachment.id);
      }
    }
    for (const attachment of attachments.pending) {
      if (
        isVoiceNoteAttachment({
          filename: attachment.filename,
          m: attachment.type,
        })
      ) {
        media.removeAttachment(attachment.url);
      }
    }
    beginRecording({ locked: true });
  }, [beginRecording, media.removeAttachment, media.removeQueuedAttachment]);

  const submitReview = React.useCallback(() => submitRef.current(), []);

  const reviewElement =
    reviewEnabled && hasAttachment && recorder.status === "idle" ? (
      <div
        className="mb-2 flex items-center gap-2"
        data-testid="voice-note-review"
      >
        <Button onClick={rerecord} size="sm" type="button" variant="outline">
          <RotateCcw aria-hidden="true" className="h-3.5 w-3.5" />
          Re-record
        </Button>
        <Button
          data-testid="voice-note-review-send"
          onClick={submitReview}
          size="sm"
          type="button"
        >
          <ArrowUp aria-hidden="true" className="h-3.5 w-3.5" />
          Send voice note
        </Button>
      </div>
    ) : null;

  return {
    ...recorder,
    acceptsAttachment: recorder.status === "idle" && !hasAttachment,
    beginPointerHold,
    discard,
    endHold,
    finish,
    handleEditorKeyDown,
    hasAttachment,
    hasAttachmentRef,
    isIdle: recorder.status === "idle",
    lock,
    recorderElement:
      recorder.status === "idle" ? null : (
        <VoiceNoteRecorder
          elapsedSeconds={recorder.elapsedSeconds}
          levels={recorder.levels}
          locked={recorder.locked}
          maxDurationSeconds={VOICE_NOTE_MAX_DURATION_SECONDS}
          onCancel={discard}
          onLock={lock}
          onPause={recorder.pause}
          onResume={recorder.resume}
          paused={recorder.status === "paused"}
          processing={recorder.status === "processing"}
          requesting={recorder.status === "requesting"}
        />
      ),
    releaseKeyboardHold,
    reviewElement,
    send,
    setPendingImetaWhenIdle,
    startLocked,
    statusRef,
    uploadFileWhenIdle,
  };
}
