part of '../voice_note_composer_recorder.dart';

/// Hands-free recorder: status row, then trash, pause or resume, and a
/// full-width Send.
class _LockedRecorderPanel extends StatelessWidget {
  const _LockedRecorderPanel({
    super.key,
    required this.isPaused,
    required this.elapsed,
    required this.samples,
    required this.sampleSequence,
    required this.canSend,
    required this.onDiscard,
    required this.onPause,
    required this.onResume,
    required this.onSend,
  });

  final bool isPaused;
  final Duration elapsed;
  final List<double> samples;
  final int sampleSequence;
  final bool canSend;
  final VoidCallback onDiscard;
  final VoidCallback onPause;
  final VoidCallback onResume;
  final VoidCallback onSend;

  @override
  Widget build(BuildContext context) {
    final statusColor = isPaused
        ? context.colors.onSurfaceVariant
        : context.appColors.success;
    return Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        Padding(
          padding: const EdgeInsets.symmetric(
            horizontal: Grid.half,
            vertical: Grid.xxs,
          ),
          child: Row(
            children: [
              _RecordingDot(isLive: !isPaused),
              const SizedBox(width: Grid.xxs),
              _RecorderTimer(elapsed: elapsed),
              const SizedBox(width: Grid.xxs),
              Expanded(
                child: _LiveWaveform(
                  samples: samples,
                  sampleSequence: sampleSequence,
                ),
              ),
              const SizedBox(width: Grid.xxs),
              Icon(
                isPaused ? LucideIcons.pause : LucideIcons.lock,
                size: 14,
                color: statusColor,
              ),
              const SizedBox(width: Grid.half),
              Text(
                isPaused ? 'Paused' : 'Locked',
                key: const ValueKey('voice-note-recorder-status'),
                style: context.textTheme.labelMedium?.copyWith(
                  color: statusColor,
                  fontWeight: FontWeight.w500,
                ),
              ),
            ],
          ),
        ),
        const SizedBox(height: Grid.xxs),
        Row(
          children: [
            _RecorderRoundButton(
              key: const ValueKey('voice-note-recorder-discard'),
              label: 'Discard voice note',
              icon: LucideIcons.trash2,
              foreground: context.colors.onSurface,
              background: context.colors.surface,
              onPressed: onDiscard,
            ),
            const SizedBox(width: Grid.twelve),
            _RecorderRoundButton(
              key: ValueKey(
                isPaused
                    ? 'voice-note-recorder-resume'
                    : 'voice-note-recorder-pause',
              ),
              label: isPaused ? 'Resume recording' : 'Pause recording',
              icon: isPaused ? LucideIcons.mic : LucideIcons.pause,
              foreground: context.colors.onSurface,
              background: context.colors.surface,
              onPressed: canSend ? (isPaused ? onResume : onPause) : null,
            ),
            const SizedBox(width: Grid.twelve),
            Expanded(
              child: _RecorderWideButton(
                key: const ValueKey('voice-note-recorder-send'),
                label: 'Send',
                icon: LucideIcons.arrowUp,
                onPressed: canSend ? onSend : null,
              ),
            ),
          ],
        ),
      ],
    );
  }
}
