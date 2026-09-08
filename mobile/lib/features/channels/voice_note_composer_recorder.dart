import 'dart:async';
import 'dart:io';

import 'package:clock/clock.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_hooks/flutter_hooks.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../shared/relay/app_lifecycle_provider.dart';
import '../../shared/theme/theme.dart';
import '../../shared/voice_notes/voice_note_preferences.dart';
import '../../shared/widgets/buzz_loading_indicator.dart';
import 'voice_note_attachment.dart';
import 'voice_note_recorder_phase.dart';
import 'voice_note_recording.dart';
import 'voice_note_waveform.dart';

part 'voice_note_composer_recorder/cancel_hint.dart';
part 'voice_note_composer_recorder/controls.dart';
part 'voice_note_composer_recorder/hold.dart';
part 'voice_note_composer_recorder/lock_chip.dart';
part 'voice_note_composer_recorder/locked.dart';
part 'voice_note_composer_recorder/review.dart';

class _VoiceNoteRouteAware extends RouteAware {
  _VoiceNoteRouteAware(this.onCovered);

  final VoidCallback onCovered;

  @override
  void didPushNext() => onCovered();
}

/// Composer control that records a voice note through the hold, lock, and
/// review phases owned by [voiceNoteRecorderPhaseProvider].
///
/// Every exit path ends in one of the callbacks: [onRecorded] with a finished
/// note, [onCancel] with nothing to keep, or [onError] with text for the
/// composer's error line. The parent clears its recording flags and resets
/// the phase in each case, so a gesture in flight can never leave the
/// composer stuck.
class VoiceNoteComposerRecorder extends HookConsumerWidget {
  const VoiceNoteComposerRecorder({
    super.key,
    required this.onCancel,
    required this.onRecorded,
    required this.onError,
  });

  final VoidCallback onCancel;
  final ValueChanged<VoiceNoteRecording> onRecorded;
  final ValueChanged<String> onError;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final phaseState = ref.watch(voiceNoteRecorderPhaseProvider);
    final phaseNotifier = ref.read(voiceNoteRecorderPhaseProvider.notifier);
    final reviewBeforeSending = ref.watch(voiceNoteReviewSettingProvider);
    final generation = phaseState.generation;
    final recorder = useMemoized(ref.read(voiceNoteRecorderFactoryProvider), [
      generation,
    ]);
    final samples = useState<List<double>>(const []);
    final sampleSequence = useState(0);
    final elapsed = useState(Duration.zero);
    final isStarted = useState(false);
    final isStopping = useRef(false);
    final segmentStartedAt = useRef<DateTime?>(null);
    final accumulated = useRef(Duration.zero);
    final reviewRecording = useState<VoiceNoteRecording?>(null);
    final lockChipLink = useMemoized(LayerLink.new);
    final lockChipController = useMemoized(OverlayPortalController.new);
    final routeAware = useMemoized(
      () => _VoiceNoteRouteAware(() {
        if (context.mounted) onCancel();
      }),
      [onCancel],
    );

    useEffect(() {
      final subscription = ref.listenManual(appLifecycleProvider, (
        previous,
        next,
      ) {
        if (next != AppLifecycleState.paused &&
            next != AppLifecycleState.detached) {
          return;
        }
        unawaited(recorder.cancel());
        if (context.mounted) onCancel();
      });
      return subscription.close;
    }, [recorder, onCancel]);

    final route = ModalRoute.of(context);
    useEffect(() {
      if (route != null) voiceNoteRouteObserver.subscribe(routeAware, route);
      return () => voiceNoteRouteObserver.unsubscribe(routeAware);
    }, [routeAware, route]);

    Future<void> discardReview() async {
      final recording = reviewRecording.value;
      reviewRecording.value = null;
      if (recording != null) {
        await deleteDroppedVoiceNoteRecording(recording.file.path);
      }
    }

    Future<void> finish() async {
      if (isStopping.value) return;
      if (!isStarted.value) {
        // Released before capture began: nothing was recorded.
        onCancel();
        return;
      }
      isStopping.value = true;
      unawaited(HapticFeedback.mediumImpact());
      try {
        final recording = await recorder.stop();
        if (!context.mounted) {
          await deleteDroppedVoiceNoteRecording(recording.file.path);
          return;
        }
        if (reviewBeforeSending) {
          reviewRecording.value = recording;
          phaseNotifier.review();
        } else {
          onRecorded(recording);
        }
      } catch (_) {
        if (context.mounted) onError('Buzz could not finish the voice note.');
      }
    }

    void sendReviewed() {
      final recording = reviewRecording.value;
      if (recording == null) return;
      reviewRecording.value = null;
      unawaited(HapticFeedback.mediumImpact());
      onRecorded(recording);
    }

    void cancelRecording() {
      unawaited(discardReview());
      onCancel();
    }

    ref.listen<VoiceNoteRecorderState>(voiceNoteRecorderPhaseProvider, (
      previous,
      next,
    ) {
      final from = previous?.phase;
      switch (next.phase) {
        case VoiceNoteRecorderPhase.idle:
          if (next.cancelledByGesture &&
              from == VoiceNoteRecorderPhase.holding) {
            unawaited(HapticFeedback.mediumImpact());
          }
        case VoiceNoteRecorderPhase.finishing:
          unawaited(finish());
        case VoiceNoteRecorderPhase.paused:
          if (from == VoiceNoteRecorderPhase.locked) {
            final segmentStart = segmentStartedAt.value;
            if (segmentStart != null) {
              accumulated.value += clock.now().difference(segmentStart);
              segmentStartedAt.value = null;
            }
            unawaited(HapticFeedback.selectionClick());
            unawaited(recorder.pause());
          }
        case VoiceNoteRecorderPhase.locked:
          if (from == VoiceNoteRecorderPhase.paused) {
            if (isStarted.value) segmentStartedAt.value = clock.now();
            unawaited(HapticFeedback.selectionClick());
            unawaited(recorder.resume());
          } else if (from == VoiceNoteRecorderPhase.holding) {
            unawaited(HapticFeedback.mediumImpact());
          } else if (from == VoiceNoteRecorderPhase.reviewing) {
            unawaited(discardReview());
          }
        case VoiceNoteRecorderPhase.holding:
        case VoiceNoteRecorderPhase.reviewing:
          break;
      }
    });

    useEffect(() {
      var active = true;
      samples.value = const [];
      elapsed.value = Duration.zero;
      accumulated.value = Duration.zero;
      segmentStartedAt.value = null;
      isStarted.value = false;
      isStopping.value = false;
      final levelSubscription = recorder.levels.listen((level) {
        if (!active) return;
        final nextSamples = [...samples.value, level];
        samples.value = nextSamples.length <= 120
            ? nextSamples
            : nextSamples.sublist(nextSamples.length - 120);
        sampleSequence.value += 1;
      });
      final timer = Timer.periodic(const Duration(milliseconds: 200), (_) {
        final segmentStart = segmentStartedAt.value;
        if (!active || segmentStart == null) return;
        elapsed.value =
            accumulated.value + clock.now().difference(segmentStart);
        if (elapsed.value >= voiceNoteMaxDuration) phaseNotifier.finish();
      });
      unawaited(() async {
        try {
          await recorder.start();
          if (!active) return;
          isStarted.value = true;
          final phase = ref.read(voiceNoteRecorderPhaseProvider).phase;
          if (phase != VoiceNoteRecorderPhase.paused) {
            segmentStartedAt.value = clock.now();
          }
        } on StateError catch (recordingError) {
          if (active) onError(recordingError.message);
        } catch (_) {
          if (active) {
            onError('Buzz could not start recording. Check microphone access.');
          }
        }
      }());
      return () {
        active = false;
        timer.cancel();
        unawaited(levelSubscription.cancel());
        unawaited(() async {
          await recorder.cancel();
          await recorder.dispose();
        }());
      };
    }, [recorder]);

    final phase = phaseState.phase;
    useEffect(() {
      // The portal controller must not change during build; settle it once
      // this frame has been laid out.
      final shouldShow = phase == VoiceNoteRecorderPhase.holding;
      var active = true;
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (!active || !context.mounted) return;
        if (shouldShow && !lockChipController.isShowing) {
          lockChipController.show();
        } else if (!shouldShow && lockChipController.isShowing) {
          lockChipController.hide();
        }
      });
      return () => active = false;
    }, [phase]);

    KeyEventResult handleKey(FocusNode node, KeyEvent event) {
      if (event is! KeyDownEvent) return KeyEventResult.ignored;
      if (event.logicalKey == LogicalKeyboardKey.escape) {
        cancelRecording();
        return KeyEventResult.handled;
      }
      final isEnter =
          event.logicalKey == LogicalKeyboardKey.enter ||
          event.logicalKey == LogicalKeyboardKey.numpadEnter;
      if (!isEnter) return KeyEventResult.ignored;
      switch (phase) {
        case VoiceNoteRecorderPhase.holding when !phaseState.hasPointer:
        case VoiceNoteRecorderPhase.locked:
        case VoiceNoteRecorderPhase.paused:
          phaseNotifier.finish();
          return KeyEventResult.handled;
        case VoiceNoteRecorderPhase.reviewing:
          sendReviewed();
          return KeyEventResult.handled;
        case VoiceNoteRecorderPhase.holding:
        case VoiceNoteRecorderPhase.idle:
        case VoiceNoteRecorderPhase.finishing:
          return KeyEventResult.ignored;
      }
    }

    final reducedMotion = MediaQuery.disableAnimationsOf(context);
    final Widget content = switch (phase) {
      VoiceNoteRecorderPhase.holding ||
      VoiceNoteRecorderPhase.finishing ||
      VoiceNoteRecorderPhase.idle => _HoldRecorderRow(
        key: const ValueKey('voice-note-recorder-hold'),
        state: phaseState,
        elapsed: elapsed.value,
        samples: samples.value,
        sampleSequence: sampleSequence.value,
        isStarted: isStarted.value,
        isFinishing: phase == VoiceNoteRecorderPhase.finishing,
        lockChipLink: lockChipLink,
        onSend: phaseNotifier.finish,
        onCancel: cancelRecording,
      ),
      VoiceNoteRecorderPhase.locked ||
      VoiceNoteRecorderPhase.paused => _LockedRecorderPanel(
        key: const ValueKey('voice-note-recorder-locked'),
        isPaused: phase == VoiceNoteRecorderPhase.paused,
        elapsed: elapsed.value,
        samples: samples.value,
        sampleSequence: sampleSequence.value,
        canSend: isStarted.value,
        onDiscard: cancelRecording,
        onPause: phaseNotifier.pause,
        onResume: phaseNotifier.resume,
        onSend: phaseNotifier.finish,
      ),
      VoiceNoteRecorderPhase.reviewing => _ReviewPanel(
        key: const ValueKey('voice-note-recorder-review'),
        recording: reviewRecording.value,
        onDiscard: cancelRecording,
        onRecordAgain: phaseNotifier.recordAgain,
        onSend: sendReviewed,
      ),
    };

    return Focus(
      key: const ValueKey('voice-note-recorder'),
      autofocus: true,
      onKeyEvent: handleKey,
      child: OverlayPortal(
        controller: lockChipController,
        overlayChildBuilder: (context) => _LockChipFollower(
          link: lockChipLink,
          progress: phaseState.lockProgress,
          onLock: phaseNotifier.lock,
        ),
        child: AnimatedSize(
          duration: reducedMotion
              ? Duration.zero
              : const Duration(milliseconds: 140),
          curve: Curves.easeOutCubic,
          alignment: Alignment.bottomCenter,
          child: content,
        ),
      ),
    );
  }
}

/// Best-effort deletion for a finalized recording the composer cannot retain.
Future<void> deleteDroppedVoiceNoteRecording(String path) async {
  try {
    final file = File(path);
    if (await file.exists()) await file.delete();
  } catch (_) {
    // Best-effort cleanup must not escape an already unmounted recorder.
  }
}
