import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_hooks/flutter_hooks.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../shared/relay/relay.dart';
import '../../shared/theme/theme.dart';
import '../../shared/voice_notes/voice_note_preferences.dart';
import '../../shared/widgets/buzz_loading_indicator.dart';
import 'voice_note_play_pause_icon.dart';
import 'voice_note_recording.dart';
import 'voice_note_waveform.dart';

part 'voice_note_attachment/controls.dart';
part 'voice_note_attachment/header.dart';
part 'voice_note_attachment/transcript_row.dart';

/// Displays a recorded or remote voice note with playback controls.
///
/// The time row follows the design: `0:24 · Voice note` at idle and
/// `0:16 · Neo · 0:24` once playback has started, with the sender inline.
/// Remote cards cycle playback speed through the mobile rates (remembered
/// per card) and fold a transcript row whose choice is remembered per
/// message.
class VoiceNoteAttachment extends HookConsumerWidget {
  const VoiceNoteAttachment.local({
    super.key,
    required String path,
    required this.duration,
    required this.waveform,
    this.onRemove,
  }) : source = path,
       isRemote = false,
       isReview = false,
       senderName = null,
       transcript = null,
       transcriptOpenByDefault = false,
       messageId = null;

  /// Compact review row from the Preview artboard: `0:12 · Tap to review`
  /// with an X that discards the take.
  const VoiceNoteAttachment.review({
    super.key,
    required String path,
    required this.duration,
    required this.waveform,
    required VoidCallback onDismiss,
  }) : source = path,
       isRemote = false,
       isReview = true,
       onRemove = onDismiss,
       senderName = null,
       transcript = null,
       transcriptOpenByDefault = false,
       messageId = null;

  const VoiceNoteAttachment.remote({
    super.key,
    required String url,
    required this.duration,
    this.waveform = const [],
    this.senderName,
    this.transcript,
    this.transcriptOpenByDefault = false,
    this.messageId,
  }) : source = url,
       isRemote = true,
       isReview = false,
       onRemove = null;

  final String source;
  final bool isRemote;
  final bool isReview;
  final Duration duration;
  final List<double> waveform;
  final VoidCallback? onRemove;

  /// Display name of the sender, shown inline in the time row once playback
  /// has started.
  final String? senderName;

  /// Transcript body from the imeta `alt` tag; the row is hidden when absent.
  final String? transcript;

  /// Whether the transcript unfolds when playback starts (true in DMs) for a
  /// message without a remembered choice. Every card starts folded.
  final bool transcriptOpenByDefault;

  /// Message the note belongs to; keys the remembered transcript choice.
  final String? messageId;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final player = useMemoized(ref.read(voiceNotePlayerFactoryProvider), [
      source,
    ]);
    final playback = useListenable(player);
    final playbackRate = ref.watch(
      voiceNotePlaybackRatesProvider.select((rates) => rates[source] ?? 1.0),
    );
    useEffect(() {
      if (isRemote) {
        unawaited(
          player.loadRemote(
            source,
            headers: () =>
                ref.read(mediaGetAuthServiceProvider).headersFor(source),
            fallbackDuration: duration,
          ),
        );
      } else {
        unawaited(player.loadLocal(source, fallbackDuration: duration));
      }
      // A remounted card resumes the speed it was last set to.
      if (playbackRate != 1.0) unawaited(player.setSpeed(playbackRate));
      return player.dispose;
    }, [player, source, isRemote, duration]);

    final state = playback.state;
    final resolvedDuration = state.duration > Duration.zero
        ? state.duration
        : duration;
    final progress = resolvedDuration.inMilliseconds <= 0
        ? 0.0
        : (state.position.inMilliseconds / resolvedDuration.inMilliseconds)
              .clamp(0.0, 1.0);
    final progressAnimation = useAnimationController(initialValue: progress);
    final wasPlaying = useRef(false);
    final hasPlayed = useState(false);
    if (state.isPlaying && !hasPlayed.value) hasPlayed.value = true;

    void animateProgressFrom(double fraction) {
      final resolved = fraction.clamp(0.0, 1.0);
      progressAnimation
        ..stop()
        ..value = resolved;
      if (!state.isPlaying || resolvedDuration.inMilliseconds <= 0) return;
      final remainingMilliseconds = math.max(
        1,
        (resolvedDuration.inMilliseconds * (1 - resolved) / playbackRate)
            .round(),
      );
      unawaited(
        progressAnimation.animateTo(
          1,
          duration: Duration(milliseconds: remainingMilliseconds),
          curve: Curves.linear,
        ),
      );
    }

    useEffect(
      () {
        // When playback starts the fill re-syncs from the player's real
        // position, so a scrub the backend ignored cannot leave the waveform
        // ahead of the audio; mid-play changes (speed, resolved duration)
        // continue from where the fill already is.
        final startedNow = state.isPlaying && !wasPlaying.value;
        wasPlaying.value = state.isPlaying;
        animateProgressFrom(
          state.isPlaying && !startedNow ? progressAnimation.value : progress,
        );
        return null;
      },
      [
        state.isPlaying,
        state.isPlaying ? null : state.position.inMilliseconds,
        resolvedDuration.inMilliseconds,
        playbackRate,
      ],
    );
    final samples = normalizeVoiceNoteWaveform(
      waveform.isEmpty ? _seededWaveform(source) : waveform,
    );
    final isComposer = !isRemote;
    final radius = isComposer
        ? Radii.dialog + Grid.quarter - Grid.twelve
        : Radii.md;
    final transcriptBody = transcript?.trim();
    final hasTranscript =
        isRemote && transcriptBody != null && transcriptBody.isNotEmpty;

    return Container(
      key: ValueKey('voice-note-attachment:$source'),
      constraints: BoxConstraints(
        minWidth: 220,
        maxWidth: isComposer ? double.infinity : 320,
        minHeight: 64,
      ),
      padding: const EdgeInsets.all(Grid.twelve),
      decoration: BoxDecoration(
        color: context.colors.surface,
        borderRadius: BorderRadius.circular(radius),
        border: Border.all(color: context.colors.outlineVariant),
      ),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Row(
            children: [
              _VoiceNotePlaybackButton(
                state: state,
                isRemote: isRemote,
                player: player,
              ),
              const SizedBox(width: Grid.xxs),
              Expanded(
                child: Column(
                  mainAxisAlignment: MainAxisAlignment.center,
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    AnimatedBuilder(
                      animation: progressAnimation,
                      builder: (context, _) => VoiceNoteWaveform(
                        samples: samples,
                        progress: progressAnimation.value,
                        height: 24,
                        // Scrubbing needs a loaded source; before the first
                        // play a remote note has none and the backend would
                        // ignore the seek while the fill moved anyway.
                        onSeek: !state.canSeek
                            ? null
                            : (fraction) {
                                animateProgressFrom(fraction);
                                unawaited(
                                  player.seek(
                                    Duration(
                                      milliseconds:
                                          (resolvedDuration.inMilliseconds *
                                                  fraction)
                                              .round(),
                                    ),
                                  ),
                                );
                              },
                      ),
                    ),
                    Row(
                      children: [
                        Expanded(
                          child: _VoiceNoteTimeLabel(
                            state: state,
                            position: state.position,
                            total: resolvedDuration,
                            senderName: senderName,
                            idleLabel: isReview
                                ? 'Tap to review'
                                : 'Voice note',
                            onIdleTap: isReview
                                ? () => unawaited(player.toggle())
                                : null,
                          ),
                        ),
                        if (isRemote)
                          _VoiceNotePlaybackRateButton(
                            key: const ValueKey('voice-note-playback-rate'),
                            rate: playbackRate,
                            onPressed: () {
                              unawaited(HapticFeedback.selectionClick());
                              final next = nextVoiceNotePlaybackRate(
                                playbackRate,
                                rates: voiceNoteMobilePlaybackRates,
                              );
                              ref
                                  .read(voiceNotePlaybackRatesProvider.notifier)
                                  .set(source, next);
                              unawaited(player.setSpeed(next));
                            },
                          ),
                      ],
                    ),
                  ],
                ),
              ),
              if (onRemove != null) ...[
                const SizedBox(width: Grid.xxs),
                _VoiceNoteRemoveButton(
                  isReview: isReview,
                  onPressed: onRemove!,
                ),
              ],
            ],
          ),
          if (hasTranscript)
            _VoiceNoteTranscriptRow(
              messageId: messageId ?? source,
              transcript: transcriptBody,
              opensOnPlayback: transcriptOpenByDefault,
              hasPlayed: hasPlayed.value,
            ),
        ],
      ),
    );
  }
}

List<double> _seededWaveform(String seed) {
  var value = seed.hashCode & 0x7fffffff;
  return List.generate(48, (_) {
    value = (1103515245 * value + 12345) & 0x7fffffff;
    return 0.12 + ((value % 760) / 1000);
  });
}
