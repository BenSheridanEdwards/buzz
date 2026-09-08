import 'dart:ui';

import 'package:buzz/features/channels/voice_note_recorder_phase.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';

void main() {
  late ProviderContainer container;
  late VoiceNoteRecorderPhaseNotifier notifier;
  final origin = const Offset(300, 700);
  final pressedAt = DateTime(2026, 9, 8, 9);

  VoiceNoteRecorderState state() =>
      container.read(voiceNoteRecorderPhaseProvider);

  setUp(() {
    container = ProviderContainer();
    addTearDown(container.dispose);
    notifier = container.read(voiceNoteRecorderPhaseProvider.notifier);
  });

  test('a short press without travel keeps recording hands free', () {
    notifier.begin(pointer: 1, origin: origin, now: pressedAt);
    expect(state().phase, VoiceNoteRecorderPhase.holding);
    expect(state().hasPointer, isTrue);

    notifier.pointerReleased(
      1,
      now: pressedAt.add(const Duration(milliseconds: 100)),
    );

    expect(state().phase, VoiceNoteRecorderPhase.holding);
    expect(state().hasPointer, isFalse);
  });

  test('a real hold finishes on release', () {
    notifier.begin(pointer: 1, origin: origin, now: pressedAt);
    notifier.pointerReleased(1, now: pressedAt.add(voiceNoteTapHoldThreshold));
    expect(state().phase, VoiceNoteRecorderPhase.finishing);
    expect(state().hasPointer, isFalse);
  });

  test('sliding left past the threshold cancels back to idle', () {
    notifier.begin(pointer: 1, origin: origin, now: pressedAt);
    notifier.pointerMoved(1, origin + const Offset(-40, 0));
    expect(state().phase, VoiceNoteRecorderPhase.holding);
    expect(
      state().cancelProgress,
      closeTo(40 / voiceNoteCancelSlideDistance, 0.001),
    );

    notifier.pointerMoved(1, origin + Offset(-voiceNoteCancelSlideDistance, 0));
    expect(state().phase, VoiceNoteRecorderPhase.idle);
    expect(state().cancelledByGesture, isTrue);

    // The finger lifting afterwards must not restart anything.
    notifier.pointerReleased(1, now: pressedAt.add(const Duration(seconds: 2)));
    expect(state().phase, VoiceNoteRecorderPhase.idle);
  });

  test('sliding up past the threshold locks and drops the pointer', () {
    notifier.begin(pointer: 1, origin: origin, now: pressedAt);
    notifier.pointerMoved(1, origin + Offset(0, -voiceNoteLockSlideDistance));
    expect(state().phase, VoiceNoteRecorderPhase.locked);
    expect(state().hasPointer, isFalse);

    notifier.pointerReleased(1, now: pressedAt.add(const Duration(seconds: 2)));
    expect(state().phase, VoiceNoteRecorderPhase.locked);
  });

  test('a different pointer cannot steer the hold', () {
    notifier.begin(pointer: 1, origin: origin, now: pressedAt);
    notifier.pointerMoved(2, origin + Offset(-voiceNoteCancelSlideDistance, 0));
    notifier.pointerReleased(2, now: pressedAt.add(const Duration(seconds: 2)));
    expect(state().phase, VoiceNoteRecorderPhase.holding);
    expect(state().pointer, 1);
  });

  test('locked recordings pause, resume, and finish', () {
    notifier.begin(now: pressedAt);
    notifier.lock();
    expect(state().phase, VoiceNoteRecorderPhase.locked);
    notifier.pause();
    expect(state().phase, VoiceNoteRecorderPhase.paused);
    notifier.resume();
    expect(state().phase, VoiceNoteRecorderPhase.locked);
    notifier.finish();
    expect(state().phase, VoiceNoteRecorderPhase.finishing);
    notifier.review();
    expect(state().phase, VoiceNoteRecorderPhase.reviewing);
  });

  test('record again bumps the generation into a fresh locked take', () {
    notifier.begin(now: pressedAt);
    final first = state().generation;
    notifier.finish();
    notifier.review();
    notifier.recordAgain();
    expect(state().phase, VoiceNoteRecorderPhase.locked);
    expect(state().generation, first + 1);
  });

  test('begin is ignored while a recording is active', () {
    notifier.begin(pointer: 1, origin: origin, now: pressedAt);
    final generation = state().generation;
    notifier.begin(pointer: 2, origin: Offset.zero, now: pressedAt);
    expect(state().pointer, 1);
    expect(state().generation, generation);
  });

  test('reset returns to idle from any phase', () {
    notifier.begin(now: pressedAt);
    notifier.lock();
    notifier.pause();
    notifier.reset();
    expect(state().phase, VoiceNoteRecorderPhase.idle);
    expect(state().cancelledByGesture, isFalse);
  });

  test('under RTL the cancel slide travels toward the right edge', () {
    notifier.begin(
      pointer: 1,
      origin: origin,
      now: pressedAt,
      textDirection: TextDirection.rtl,
    );
    notifier.pointerMoved(1, origin + const Offset(-90, 0));
    expect(state().cancelProgress, 0);
    expect(state().cancelTravel, -90);

    notifier.pointerMoved(1, origin + const Offset(60, 0));
    expect(state().cancelProgress, closeTo(0.5, 0.001));
    expect(state().phase, VoiceNoteRecorderPhase.holding);

    notifier.pointerMoved(
      1,
      origin + const Offset(voiceNoteCancelSlideDistance, 0),
    );
    expect(state().phase, VoiceNoteRecorderPhase.idle);
    expect(state().cancelledByGesture, isTrue);
  });

  test('release returns to idle only for the generation that owns it', () {
    notifier.begin(now: pressedAt);
    final owned = state().generation;
    notifier.lock();

    notifier.release(owned - 1);
    expect(state().phase, VoiceNoteRecorderPhase.locked);

    notifier.release(owned);
    expect(state().phase, VoiceNoteRecorderPhase.idle);

    // Idle already: a late release is a no-op and keeps the generation.
    notifier.release(owned);
    expect(state().generation, owned);
  });
}
