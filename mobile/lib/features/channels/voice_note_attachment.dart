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
/// Remote cards title themselves with the sender, show `current / total`
/// time, cycle playback speed through the mobile rates, and fold a
/// transcript row whose last state is remembered per device.
class VoiceNoteAttachment extends HookConsumerWidget {
  const VoiceNoteAttachment.local({
    super.key,
    required String path,
    required this.duration,
    required this.waveform,
    this.onRemove,
  }) : source = path,
       isRemote = false,
       senderName = null,
       transcript = null,
       transcriptOpenByDefault = false;

  const VoiceNoteAttachment.remote({
    super.key,
    required String url,
    required this.duration,
    this.waveform = const [],
    this.senderName,
    this.transcript,
    this.transcriptOpenByDefault = false,
  }) : source = url,
       isRemote = true,
       onRemove = null;

  final String source;
  final bool isRemote;
  final Duration duration;
  final List<double> waveform;
  final VoidCallback? onRemove;

  /// Display name shown as the card title on received notes.
  final String? senderName;

  /// Transcript body from the imeta `alt` tag; the row is hidden when absent.
  final String? transcript;

  /// Fold default used until this device remembers a choice (open in DMs).
  final bool transcriptOpenByDefault;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final player = useMemoized(ref.read(voiceNotePlayerFactoryProvider), [
      source,
    ]);
    final playback = useListenable(player);
    final playbackRate = useState(1.0);
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

    void animateProgressFrom(double fraction) {
      final resolved = fraction.clamp(0.0, 1.0);
      progressAnimation
        ..stop()
        ..value = resolved;
      if (!state.isPlaying || resolvedDuration.inMilliseconds <= 0) return;
      final remainingMilliseconds = math.max(
        1,
        (resolvedDuration.inMilliseconds * (1 - resolved) / playbackRate.value)
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
        animateProgressFrom(
          state.isPlaying ? progressAnimation.value : progress,
        );
        return null;
      },
      [
        state.isPlaying,
        state.isPlaying ? null : state.position.inMilliseconds,
        resolvedDuration.inMilliseconds,
        playbackRate.value,
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
                    if (senderName case final title?
                        when title.trim().isNotEmpty)
                      _VoiceNoteCardTitle(title: title.trim()),
                    AnimatedBuilder(
                      animation: progressAnimation,
                      builder: (context, _) => VoiceNoteWaveform(
                        samples: samples,
                        progress: progressAnimation.value,
                        height: 24,
                        onSeek: (fraction) {
                          animateProgressFrom(fraction);
                          unawaited(
                            player.seek(
                              Duration(
                                milliseconds:
                                    (resolvedDuration.inMilliseconds * fraction)
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
                          ),
                        ),
                        if (isRemote)
                          _VoiceNotePlaybackRateButton(
                            key: const ValueKey('voice-note-playback-rate'),
                            rate: playbackRate.value,
                            onPressed: () {
                              unawaited(HapticFeedback.selectionClick());
                              final next = nextVoiceNotePlaybackRate(
                                playbackRate.value,
                                rates: voiceNoteMobilePlaybackRates,
                              );
                              playbackRate.value = next;
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
                SizedBox.square(
                  dimension: 40,
                  child: IconButton(
                    key: const ValueKey('composer-voice-note-remove'),
                    tooltip: 'Remove voice note',
                    onPressed: onRemove,
                    style: IconButton.styleFrom(
                      minimumSize: const Size.square(40),
                      maximumSize: const Size.square(40),
                      padding: EdgeInsets.zero,
                      tapTargetSize: MaterialTapTargetSize.shrinkWrap,
                    ),
                    icon: const Icon(LucideIcons.x, size: 18),
                  ),
                ),
              ],
            ],
          ),
          if (hasTranscript)
            _VoiceNoteTranscriptRow(
              transcript: transcriptBody,
              openByDefault: transcriptOpenByDefault,
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
