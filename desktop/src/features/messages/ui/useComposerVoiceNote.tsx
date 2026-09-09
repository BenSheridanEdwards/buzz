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
import {
  claimVoiceNoteRecording,
  discardActiveVoiceNoteRecording,
  getVoiceNoteRecordingOwner,
  subscribeVoiceNoteRecording,
} from "@/features/messages/lib/voiceNoteRecordingRegistry";
import { useVoiceNoteReviewEnabled } from "@/features/messages/lib/voiceNoteReviewPreference";
import { acquireEscapeSurface } from "@/shared/hooks/escapeSurfaces";
import { Button } from "@/shared/ui/button";
import { VoiceNoteRecorder } from "./VoiceNoteRecorder";

/**
 * How a hold-to-record session was started. Pointer holds end on the matching
 * pointer release; keyboard holds end when Space is released. Both end when
 * the window loses focus (the hold cannot be observed any more), sending the
 * note unless it was locked first.
 */
export type VoiceNoteHoldSource = "pointer" | "keyboard";

/**
 * A press released sooner than this is a tap, not a hold: it locks the
 * recording hands free instead of sending a fraction of a second of audio.
 */
export const VOICE_NOTE_TAP_TO_LOCK_MS = 300;

type Hold = {
  release: () => void;
  source: VoiceNoteHoldSource;
  startedAt: number;
};

/**
 * Why a hold ended. A `release` is the user letting go (pointer up, Space
 * up) and may be a tap; an `interruption` (window blur, page hidden) is not a
 * gesture, so it never locks.
 */
type HoldEnd = "release" | "interruption";

type Outcome = "discarded" | "finished";

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
  dismissAutocomplete,
  draftKey,
  editTargetId,
  focusEditor,
  media,
  setEmojiPickerOpen,
  setFormattingOpen,
  submit,
}: {
  /**
   * Closes any open composer autocomplete list, reporting whether one was
   * open. The row's Escape runs in the window capture phase, ahead of the
   * editor handlers that would otherwise close the list, so it has to be able
   * to do it itself when focus is on a recorder control.
   */
  dismissAutocomplete: () => boolean;
  draftKey: string | null | undefined;
  editTargetId: string | null;
  /** Put the caret back in the editor (keyboard starts and row unmounts). */
  focusEditor: () => void;
  media: MediaUploadController;
  setEmojiPickerOpen: (open: boolean) => void;
  setFormattingOpen: (open: boolean) => void;
  /** Submit the composer once a finished note has been queued (review off). */
  submit: () => void;
}) {
  const recorder = useVoiceNoteRecorder();
  // Identity for the recording claim: composers are siblings with no shared
  // owner, so the registry keys on this instead of on a React tree position.
  const composerId = React.useId();
  const recordingOwner = React.useSyncExternalStore(
    subscribeVoiceNoteRecording,
    getVoiceNoteRecordingOwner,
    getVoiceNoteRecordingOwner,
  );
  const otherComposerIsRecording =
    recordingOwner !== null && recordingOwner !== composerId;
  const releaseRecordingRef = React.useRef<(() => void) | null>(null);
  const dismissAutocompleteRef = React.useRef(dismissAutocomplete);
  dismissAutocompleteRef.current = dismissAutocomplete;
  const reviewEnabled = useVoiceNoteReviewEnabled();
  const reviewEnabledRef = React.useRef(reviewEnabled);
  reviewEnabledRef.current = reviewEnabled;
  const submitRef = React.useRef(submit);
  submitRef.current = submit;
  const focusEditorRef = React.useRef(focusEditor);
  focusEditorRef.current = focusEditor;
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
  // Rendered twin of `holdRef.current?.source`: the recorder row changes its
  // lock affordance depending on which hand is busy.
  const [holdSource, setHoldSource] =
    React.useState<VoiceNoteHoldSource | null>(null);
  const lockKeyHeldRef = React.useRef(false);
  const pendingSubmitRef = React.useRef(false);
  const outcomeRef = React.useRef<Outcome | null>(null);
  const recorderRef = React.useRef<HTMLFieldSetElement | null>(null);
  const [announcement, setAnnouncement] = React.useState("");
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
    setHoldSource(null);
    hold.release();
    return hold;
  }, []);

  const discard = React.useCallback(() => {
    releaseHold();
    lockKeyHeldRef.current = false;
    pendingSubmitRef.current = false;
    if (statusRef.current !== "idle") outcomeRef.current = "discarded";
    recorder.cancel();
  }, [recorder.cancel, releaseHold]);
  const discardRef = React.useRef(discard);
  discardRef.current = discard;

  /**
   * Escape's discard, and the callback the recording claim carries. Reports
   * whether it actually discarded, so a caller can let the key fall through
   * to whatever is behind it instead of swallowing it on a refusal.
   *
   * It declines twice over. Once Send has been pressed the note is on its way
   * out, and an ambient key must not cancel an upload the user just committed
   * to: the row says "Preparing voice note" and finishes. The trash keeps
   * working throughout, because pressing it is a deliberate "throw this away"
   * and it is the only way back once the encode has started. And an idle
   * recorder has nothing to discard: if a claim ever outlives its recording,
   * saying "handled" here would kill Escape for the whole window.
   */
  const requestDiscard = React.useCallback((): boolean => {
    if (statusRef.current === "idle") return false;
    if (outcomeRef.current === "finished") return false;
    discard();
    return true;
  }, [discard]);
  const requestDiscardRef = React.useRef(requestDiscard);
  requestDiscardRef.current = requestDiscard;

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
    // A flag left over from a note whose upload never materialised must not
    // auto-send the next one past review.
    pendingSubmitRef.current = false;
    const status = statusRef.current;
    if (status !== "recording" && status !== "paused") return null;
    outcomeRef.current = "finished";
    const recording = await finish();
    pendingSubmitRef.current = recording !== null && !reviewEnabledRef.current;
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

  /**
   * The microphone half of the rule: only one composer in the window may hold
   * it. Separate from the attachment half because re-recording asks only this
   * one — it is replacing the attachment it already has.
   */
  const acceptsRecordingClaim = React.useCallback(() => {
    if (getVoiceNoteRecordingOwner() === null) return true;
    toast.error("Finish or discard the other voice note first.");
    return false;
  }, []);

  /**
   * A voice note must be the only attachment, and only one composer in the
   * window may hold the microphone.
   */
  const canStart = React.useCallback(() => {
    if (getVoiceNoteRecordingOwner() !== null) return false;
    const attachments = getAttachmentsRef.current();
    return attachments.pending.length === 0 && attachments.queued.length === 0;
  }, []);

  const acceptsStart = React.useCallback(() => {
    if (!acceptsRecordingClaim()) return false;
    if (canStart()) return true;
    toast.error("A voice note must be the only attachment.");
    return false;
  }, [acceptsRecordingClaim, canStart]);

  const beginRecording = React.useCallback(
    ({ locked }: { locked: boolean }) => {
      // Claim before the microphone opens: a second composer that gets here
      // in the same tick is refused rather than opening a rival recorder.
      // The claim carries `requestDiscard`, not the raw discard, so a sibling
      // composer's Escape obeys the same rules as this composer's own.
      releaseRecordingRef.current?.();
      const release = claimVoiceNoteRecording(composerId, () =>
        requestDiscardRef.current(),
      );
      releaseRecordingRef.current = release;
      if (release === null) return;
      recordingContextRef.current = currentContextRef.current;
      outcomeRef.current = null;
      onBeforeStartRef.current();
      lockedRef.current = locked;
      void startRef.current().then((started) => {
        // A start that never leaves idle (no MediaRecorder in this webview, a
        // denied microphone, a cancel that lands first) changes no status, so
        // the release effect below never runs. Without this the claim is held
        // by a composer that is not recording, for the life of the page: every
        // other microphone stays disabled and Escape is swallowed window-wide.
        // Token-guarded, so a newer claim is never the one released here.
        if (started || statusRef.current !== "idle") return;
        release();
        if (releaseRecordingRef.current === release) {
          releaseRecordingRef.current = null;
        }
      });
      if (locked) recorder.lock();
    },
    [composerId, recorder.lock],
  );

  // The claim outlives the row only as long as the recorder is busy: a
  // discard, a send, or a failed start all return the microphone.
  React.useEffect(() => {
    if (recorder.status !== "idle") return;
    releaseRecordingRef.current?.();
    releaseRecordingRef.current = null;
  }, [recorder.status]);

  const lock = React.useCallback(() => {
    const status = statusRef.current;
    if (status !== "recording" && status !== "requesting") return;
    if (lockedRef.current) return;
    lockedRef.current = true;
    recorder.lock();
  }, [recorder.lock]);

  const endHold = React.useCallback(
    (end: HoldEnd = "release") => {
      const hold = releaseHold();
      if (!hold) return;
      if (lockedRef.current) return;
      const status = statusRef.current;
      const isTap =
        end === "release" &&
        performance.now() - hold.startedAt < VOICE_NOTE_TAP_TO_LOCK_MS;
      if (status === "requesting") {
        // A tap means "record hands free": the lock takes effect the moment
        // the microphone arrives. An interrupted hold has nothing to keep.
        if (isTap) lock();
        else recorder.cancel();
        return;
      }
      if (status !== "recording" && status !== "paused") return;
      if (isTap) lock();
      else void sendRef.current();
    },
    [lock, recorder.cancel, releaseHold],
  );
  const endHoldRef = React.useRef(endHold);
  endHoldRef.current = endHold;
  const endPointerHold = React.useCallback(() => endHold("release"), [endHold]);
  /** The mic's own `pointercancel`, which fires before the window's. */
  const cancelPointerHold = React.useCallback(
    () => endHold("interruption"),
    [endHold],
  );

  const beginHold = React.useCallback(
    (source: VoiceNoteHoldSource) => {
      if (holdRef.current || statusRef.current !== "idle") return;
      if (!acceptsStart()) return;
      const onRelease = () => endHoldRef.current("release");
      const onInterruption = () => endHoldRef.current("interruption");
      const onKeyUp = (event: KeyboardEvent) => {
        if (isSpaceKey(event)) endHoldRef.current("release");
      };
      const onVisibilityChange = () => {
        if (document.visibilityState === "hidden") onInterruption();
      };
      window.addEventListener("blur", onInterruption);
      document.addEventListener("visibilitychange", onVisibilityChange);
      if (source === "pointer") {
        window.addEventListener("pointerup", onRelease);
        // A cancelled press is the system taking the pointer away, not the
        // user letting go: it must never read as a tap and lock hands free.
        window.addEventListener("pointercancel", onInterruption);
      } else {
        window.addEventListener("keyup", onKeyUp);
      }
      holdRef.current = {
        release: () => {
          window.removeEventListener("blur", onInterruption);
          document.removeEventListener("visibilitychange", onVisibilityChange);
          window.removeEventListener("pointerup", onRelease);
          window.removeEventListener("pointercancel", onInterruption);
          window.removeEventListener("keyup", onKeyUp);
        },
        source,
        startedAt: performance.now(),
      };
      setHoldSource(source);
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
   * technology) has no release to wait for, so it starts hands free. The mic
   * leaves the toolbar with the recorder's arrival, so focus moves to the
   * editor instead of dying with it.
   */
  const startLocked = React.useCallback(() => {
    if (holdRef.current || statusRef.current !== "idle") return;
    if (!acceptsStart()) return;
    beginRecording({ locked: true });
    focusEditorRef.current();
  }, [acceptsStart, beginRecording]);

  const releaseKeyboardHold = React.useCallback(() => {
    if (holdRef.current?.source === "keyboard") {
      endHoldRef.current("interruption");
    }
  }, []);

  /**
   * Editor key path. Space (plain, editor empty) holds to record; Esc discards;
   * L locks a keyboard hold. Returns true when the key was consumed.
   */
  const handleEditorKeyDown = React.useCallback(
    (
      event: React.KeyboardEvent<HTMLElement>,
      {
        editorEmpty,
        mentionOpen = false,
      }: { editorEmpty: boolean; mentionOpen?: boolean },
    ): boolean => {
      // Mid-composition keys belong to the IME (Space picks a candidate).
      if (event.nativeEvent.isComposing) return false;
      const status = statusRef.current;
      if (event.key === "Escape") {
        // An open autocomplete owns Escape: the first press closes the list,
        // and only a press with no list open reaches the recording. This
        // handler runs before the mention handler, so it has to decline
        // rather than rely on the list marking the event handled.
        if (mentionOpen) return false;
        if (status !== "idle") {
          event.preventDefault();
          requestDiscardRef.current();
          return true;
        }
        // ProseMirror marks Escape handled at the contenteditable, so a
        // sibling composer's recording is otherwise unreachable from here.
        if (discardActiveVoiceNoteRecording()) {
          event.preventDefault();
          return true;
        }
        return false;
      }
      if (isSpaceKey(event)) {
        // Shift+Space and other chords are not Space: they keep typing.
        if (!isPlainKey(event)) return false;
        if (holdRef.current?.source === "keyboard") {
          // Auto-repeat while the hold lasts must not type spaces.
          event.preventDefault();
          return true;
        }
        if (status !== "idle" || !editorEmpty || event.repeat) return false;
        // Beside another attachment Space is just a space: no toast, no swallow.
        if (!canStart()) return false;
        event.preventDefault();
        beginHold("keyboard");
        return true;
      }
      if (status === "idle") return false;
      if (isLockKey(event) && isPlainKey(event)) {
        if (lockKeyHeldRef.current) {
          // Holding L: swallow the auto-repeat instead of typing "l".
          event.preventDefault();
          return true;
        }
        // Only a Space hold makes L a shortcut here: Space is down, so the
        // key cannot be typing. During a pointer hold "l" is a letter.
        if (holdRef.current?.source !== "keyboard") return false;
        if (event.repeat || lockedRef.current) return false;
        if (status !== "recording" && status !== "requesting") return false;
        event.preventDefault();
        lockKeyHeldRef.current = true;
        lock();
        return true;
      }
      return false;
    },
    [beginHold, canStart, lock],
  );

  // Esc discards from anywhere: the recorder is a closable surface, so the
  // app-level Esc shortcut (mark channel read) and the panels around it yield
  // to it instead of winning on listener order.
  //
  // Two passes, because the row's own controls are Radix tooltip triggers and
  // an open (or still-animating-out) tooltip is a document-capture layer that
  // would swallow the key: for a target inside the row the recorder claims Esc
  // in the window capture phase, which runs before any document listener.
  // Everything else goes through the bubble pass, so menus, popovers and
  // dialogs opened over the composer still take Esc first as before.
  const active = recorder.status !== "idle";
  React.useEffect(() => {
    if (!active) return;
    const surface = acquireEscapeSurface();
    const rowOwnsTarget = (target: EventTarget | null) =>
      target instanceof Node &&
      (recorderRef.current?.contains(target) ?? false);
    const handle = (event: KeyboardEvent, insideRow: boolean) => {
      if (event.key !== "Escape" || event.defaultPrevented) return;
      if (!surface.isTopmost()) return;
      if (rowOwnsTarget(event.target) !== insideRow) return;
      // An open autocomplete owns Escape wherever focus is. From the editor
      // the list's own handler closes it and the recorder declines; from a
      // focused recorder control this pass runs before every other handler,
      // so it has to close the list itself rather than discard the recording
      // out from under a key the user aimed at the list (rule 8).
      if (dismissAutocompleteRef.current()) {
        event.preventDefault();
        return;
      }
      // Only claim the key if the recording actually took it: a note that is
      // already encoding declines, and Escape then belongs to whatever is
      // behind the composer, as it would with no recorder on screen at all.
      if (!requestDiscardRef.current()) return;
      event.preventDefault();
    };
    const onCapture = (event: KeyboardEvent) => handle(event, true);
    const onBubble = (event: KeyboardEvent) => handle(event, false);
    window.addEventListener("keydown", onCapture, { capture: true });
    window.addEventListener("keydown", onBubble);
    return () => {
      window.removeEventListener("keydown", onCapture, { capture: true });
      window.removeEventListener("keydown", onBubble);
      surface.release();
    };
  }, [active]);

  // Outside the editor (a focused chip, toolbar control, or nothing at all
  // during a pointer hold), L still locks; the editor path handles typing.
  React.useEffect(() => {
    if (!active) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.isComposing) return;
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
  }, [active, lock]);

  React.useEffect(
    () => () => {
      releaseHold();
      releaseRecordingRef.current?.();
      releaseRecordingRef.current = null;
      // Navigation can take the whole composer away mid-recording (a profile
      // or thread overlay replacing the channel on a narrow window). The live
      // region unmounts with it, so the only way the loss is not silent is a
      // toast, which lives at the app root and outlives the composer.
      if (statusRef.current !== "idle") {
        toast("Voice note discarded when the composer closed.");
      }
    },
    [releaseHold],
  );

  // Announce transitions, never the ticking clock. The region itself lives in
  // the composer (always mounted) so "sent" and "discarded" are still heard
  // after the row is gone.
  const previousStatusRef = React.useRef(recorder.status);
  const previousLockedRef = React.useRef(recorder.locked);
  React.useEffect(() => {
    const previousStatus = previousStatusRef.current;
    const previousLocked = previousLockedRef.current;
    previousStatusRef.current = recorder.status;
    previousLockedRef.current = recorder.locked;
    const status = recorder.status;
    let next: string | null = null;
    if (status !== previousStatus) {
      if (status === "requesting") next = "Waiting for microphone";
      else if (status === "recording" && previousStatus === "paused") {
        next = "Recording resumed";
      } else if (status === "recording") {
        next = recorder.locked
          ? "Recording voice note, hands free"
          : "Recording voice note, release to send";
      } else if (status === "paused") next = "Recording paused";
      else if (status === "processing") next = "Preparing voice note";
      else if (status === "idle") {
        const outcome = outcomeRef.current;
        outcomeRef.current = null;
        if (outcome === "discarded") next = "Voice note discarded";
        else if (outcome === "finished") {
          next = reviewEnabledRef.current
            ? "Voice note ready to review"
            : "Voice note sent";
        }
      }
    } else if (status === "recording" && recorder.locked && !previousLocked) {
      next = "Recording locked, hands free";
    }
    if (next !== null) setAnnouncement(next);
  }, [recorder.locked, recorder.status]);

  // Focus never dies with a control that just left (rule 7): a lock replaces
  // the chip with pause, and the row's departure hands focus to the editor.
  React.useEffect(() => {
    if (recorder.status !== "idle") return;
    const activeElement = document.activeElement;
    const row = recorderRef.current;
    if (
      activeElement === null ||
      activeElement === document.body ||
      row?.contains(activeElement)
    ) {
      focusEditorRef.current();
    }
  }, [recorder.status]);

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
    // Refuse before destroying anything. The queued note is the only copy of
    // the audio, and `beginRecording` bails the moment the claim is refused,
    // so removing it first and asking afterwards deletes the take and starts
    // nothing. The toast is the whole feedback the user gets here: the review
    // row has no other way to say why the button did nothing.
    if (!acceptsRecordingClaim()) return;
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
  }, [
    acceptsRecordingClaim,
    beginRecording,
    media.removeAttachment,
    media.removeQueuedAttachment,
  ]);

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

  const liveRegionElement = (
    <div
      aria-live="polite"
      className="sr-only"
      data-testid="voice-note-live-status"
      role="status"
    >
      {announcement}
    </div>
  );

  return {
    ...recorder,
    acceptsAttachment: recorder.status === "idle" && !hasAttachment,
    beginPointerHold,
    cancelHold: cancelPointerHold,
    discard,
    endHold: endPointerHold,
    /** Another composer holds the microphone: this one's mic is disabled. */
    otherComposerIsRecording,
    finish,
    handleEditorKeyDown,
    hasAttachment,
    hasAttachmentRef,
    isIdle: recorder.status === "idle",
    liveRegionElement,
    lock,
    recorderElement:
      recorder.status === "idle" ? null : (
        <VoiceNoteRecorder
          containerRef={recorderRef}
          elapsedSeconds={recorder.elapsedSeconds}
          holdSource={holdSource}
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
