import 'dart:ui' show Offset, TextDirection;

import 'package:clock/clock.dart';
import 'package:flutter/foundation.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';

/// Horizontal travel, in logical pixels, that turns a hold into a cancel.
const voiceNoteCancelSlideDistance = 120.0;

/// Upward travel, in logical pixels, that locks a hold hands free.
const voiceNoteLockSlideDistance = 72.0;

/// Longest press that still counts as a tap on the mic rather than a hold.
const voiceNoteTapHoldThreshold = Duration(milliseconds: 250);

/// Pointer travel below which a short press is still a tap.
const voiceNoteTapSlop = 12.0;

/// Shortest capture the composer keeps; anything under it is discarded with
/// the hold-to-record hint instead of attaching a note shorter than a tick.
const voiceNoteMinDuration = Duration(seconds: 1);

/// Where the composer recorder is in its hold, lock, and review lifecycle.
enum VoiceNoteRecorderPhase {
  /// No recording is in flight; the pill shows the mic.
  idle,

  /// Recording while the mic is held (or hands free after a tap).
  holding,

  /// Recording hands free with trash, pause, and Send controls.
  locked,

  /// Locked recording with capture suspended.
  paused,

  /// The recorder is stopping so the note can be sent or reviewed.
  finishing,

  /// A finished note is playing back before the user sends it.
  reviewing,
}

/// Immutable gesture and phase snapshot for the composer voice-note recorder.
@immutable
class VoiceNoteRecorderState {
  const VoiceNoteRecorderState({
    this.phase = VoiceNoteRecorderPhase.idle,
    this.pointer,
    this.origin = Offset.zero,
    this.position = Offset.zero,
    this.heldSince,
    this.beganAt,
    this.generation = 0,
    this.cancelledByGesture = false,
    this.textDirection = TextDirection.ltr,
  });

  /// Current lifecycle phase.
  final VoiceNoteRecorderPhase phase;

  /// Pointer id of the finger holding the mic, or `null` when hands free.
  final int? pointer;

  /// Global position where the hold began.
  final Offset origin;

  /// Latest global pointer position while holding.
  final Offset position;

  /// When the current hold began, used to tell taps from holds.
  final DateTime? heldSince;

  /// When the user started this take, kept for the whole take. [heldSince] is
  /// cleared as soon as the finger leaves, so this is the only record of how
  /// long the press itself lasted, which is what the minimum-length check
  /// judges the user by.
  final DateTime? beganAt;

  /// Increments on every restart so stale async work can be ignored.
  final int generation;

  /// Whether the most recent return to idle came from a slide-to-cancel.
  final bool cancelledByGesture;

  /// Reading direction of the composer when the hold began; the cancel slide
  /// travels toward the start edge, so it mirrors under RTL.
  final TextDirection textDirection;

  /// Whether a finger is currently holding the mic.
  bool get hasPointer => pointer != null;

  /// Whether audio is being captured or held for review.
  bool get isActive => phase != VoiceNoteRecorderPhase.idle;

  /// Whether the recorder is capturing (held, locked, or paused).
  bool get isRecording =>
      phase == VoiceNoteRecorderPhase.holding ||
      phase == VoiceNoteRecorderPhase.locked ||
      phase == VoiceNoteRecorderPhase.paused;

  /// Finger displacement from where the hold started.
  Offset get dragOffset => position - origin;

  /// Finger travel toward the start edge (leftward in LTR, rightward in
  /// RTL), in logical pixels; negative while moving the other way.
  double get cancelTravel =>
      textDirection == TextDirection.rtl ? dragOffset.dx : -dragOffset.dx;

  /// Progress toward the cancel threshold, from 0 to 1.
  double get cancelProgress =>
      (cancelTravel / voiceNoteCancelSlideDistance).clamp(0.0, 1.0);

  /// Progress toward the lock threshold, from 0 to 1.
  double get lockProgress =>
      (-dragOffset.dy / voiceNoteLockSlideDistance).clamp(0.0, 1.0);

  VoiceNoteRecorderState copyWith({
    VoiceNoteRecorderPhase? phase,
    int? pointer,
    bool clearPointer = false,
    Offset? origin,
    Offset? position,
    DateTime? heldSince,
    bool clearHeldSince = false,
    DateTime? beganAt,
    int? generation,
    bool? cancelledByGesture,
    TextDirection? textDirection,
  }) => VoiceNoteRecorderState(
    phase: phase ?? this.phase,
    pointer: clearPointer ? null : (pointer ?? this.pointer),
    origin: origin ?? this.origin,
    position: position ?? this.position,
    heldSince: clearHeldSince ? null : (heldSince ?? this.heldSince),
    beganAt: beganAt ?? this.beganAt,
    generation: generation ?? this.generation,
    cancelledByGesture: cancelledByGesture ?? this.cancelledByGesture,
    textDirection: textDirection ?? this.textDirection,
  );
}

/// Owns the composer recorder phase and the hold, slide, and lock gesture.
///
/// Pointer tracking is fed from a raw [Listener] above the composer so the
/// gesture survives the pill morphing under the finger. Every exit path
/// (cancel, finish, failure) goes through [reset] or [finish], which the
/// composer observes to clear its own recording flags (rule 2).
class VoiceNoteRecorderPhaseNotifier extends Notifier<VoiceNoteRecorderState> {
  @override
  VoiceNoteRecorderState build() => const VoiceNoteRecorderState();

  /// Starts a hold. With a [pointer] the mic is being pressed; without one
  /// the recording is hands free from the start (tap or keyboard path).
  /// [textDirection] decides which way the cancel slide travels.
  void begin({
    int? pointer,
    Offset origin = Offset.zero,
    DateTime? now,
    TextDirection textDirection = TextDirection.ltr,
  }) {
    if (state.phase != VoiceNoteRecorderPhase.idle) return;
    state = VoiceNoteRecorderState(
      phase: VoiceNoteRecorderPhase.holding,
      pointer: pointer,
      origin: origin,
      position: origin,
      heldSince: now ?? clock.now(),
      beganAt: now ?? clock.now(),
      generation: state.generation + 1,
      textDirection: textDirection,
    );
  }

  /// Tracks the holding finger; crossing a threshold cancels or locks.
  void pointerMoved(int pointer, Offset position) {
    if (state.phase != VoiceNoteRecorderPhase.holding ||
        state.pointer != pointer) {
      return;
    }
    final next = state.copyWith(position: position);
    if (next.cancelProgress >= 1) {
      state = VoiceNoteRecorderState(
        generation: state.generation,
        cancelledByGesture: true,
      );
      return;
    }
    if (next.lockProgress >= 1) {
      state = next.copyWith(
        phase: VoiceNoteRecorderPhase.locked,
        clearPointer: true,
        clearHeldSince: true,
      );
      return;
    }
    state = next;
  }

  /// Releases the holding finger. A short press keeps recording hands free;
  /// a real hold finishes so the note can be sent.
  void pointerReleased(int pointer, {DateTime? now}) {
    if (state.pointer != pointer) return;
    if (state.phase != VoiceNoteRecorderPhase.holding) {
      state = state.copyWith(clearPointer: true);
      return;
    }
    final heldSince = state.heldSince;
    final heldFor = heldSince == null
        ? voiceNoteTapHoldThreshold
        : (now ?? clock.now()).difference(heldSince);
    final wasTap =
        heldFor < voiceNoteTapHoldThreshold &&
        state.dragOffset.distance < voiceNoteTapSlop;
    if (wasTap) {
      state = state.copyWith(clearPointer: true, clearHeldSince: true);
      return;
    }
    finish();
  }

  /// Drops the finger without deciding: the recording continues hands free.
  void pointerLost(int pointer) {
    if (state.pointer != pointer) return;
    state = state.copyWith(clearPointer: true, clearHeldSince: true);
  }

  /// Locks a hold hands free (lock chip tap, slide, or keyboard).
  void lock() {
    if (state.phase != VoiceNoteRecorderPhase.holding) return;
    state = state.copyWith(
      phase: VoiceNoteRecorderPhase.locked,
      clearPointer: true,
      clearHeldSince: true,
    );
  }

  /// Suspends a locked recording.
  void pause() {
    if (state.phase != VoiceNoteRecorderPhase.locked) return;
    state = state.copyWith(phase: VoiceNoteRecorderPhase.paused);
  }

  /// Resumes a paused recording.
  void resume() {
    if (state.phase != VoiceNoteRecorderPhase.paused) return;
    state = state.copyWith(phase: VoiceNoteRecorderPhase.locked);
  }

  /// Stops capture so the note can be sent or reviewed.
  void finish() {
    if (!state.isRecording) return;
    state = state.copyWith(
      phase: VoiceNoteRecorderPhase.finishing,
      clearPointer: true,
      clearHeldSince: true,
    );
  }

  /// Holds a finished note for playback before sending.
  void review() {
    if (state.phase != VoiceNoteRecorderPhase.finishing) return;
    state = state.copyWith(phase: VoiceNoteRecorderPhase.reviewing);
  }

  /// Discards the reviewed note and starts a fresh locked recording.
  void recordAgain() {
    if (state.phase != VoiceNoteRecorderPhase.reviewing) return;
    state = VoiceNoteRecorderState(
      phase: VoiceNoteRecorderPhase.locked,
      beganAt: clock.now(),
      generation: state.generation + 1,
    );
  }

  /// Returns to idle when the recorder that owns [generation] goes away
  /// while its phase is still active (the page was popped mid-recording), so
  /// the next composer's mic is not stuck behind a phase nobody owns.
  void release(int generation) {
    if (!ref.mounted) return;
    if (!state.isActive || state.generation != generation) return;
    reset();
  }

  /// Returns to idle from any phase; [cancelledByGesture] marks a slide.
  void reset({bool cancelledByGesture = false}) {
    if (state.phase == VoiceNoteRecorderPhase.idle &&
        state.cancelledByGesture == cancelledByGesture) {
      return;
    }
    state = VoiceNoteRecorderState(
      generation: state.generation,
      cancelledByGesture: cancelledByGesture,
    );
  }
}

/// Provider for the composer voice-note recorder phase and gesture.
final voiceNoteRecorderPhaseProvider =
    NotifierProvider<VoiceNoteRecorderPhaseNotifier, VoiceNoteRecorderState>(
      VoiceNoteRecorderPhaseNotifier.new,
    );
