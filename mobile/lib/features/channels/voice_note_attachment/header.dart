part of '../voice_note_attachment.dart';

/// Sender display name shown as the card title above the waveform.
class _VoiceNoteCardTitle extends StatelessWidget {
  const _VoiceNoteCardTitle({required this.title});

  final String title;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.only(bottom: Grid.half),
    child: Text(
      title,
      key: const ValueKey('voice-note-sender'),
      maxLines: 1,
      overflow: TextOverflow.ellipsis,
      style: context.textTheme.labelLarge?.copyWith(
        color: context.colors.onSurface,
        fontWeight: FontWeight.w600,
      ),
    ),
  );
}

/// `current / total` timestamps, or the unavailable message on failure.
class _VoiceNoteTimeLabel extends StatelessWidget {
  const _VoiceNoteTimeLabel({
    required this.state,
    required this.position,
    required this.total,
  });

  final VoiceNotePlaybackState state;
  final Duration position;
  final Duration total;

  @override
  Widget build(BuildContext context) {
    final style = context.textTheme.labelSmall?.copyWith(
      color: context.colors.onSurfaceVariant,
      fontFeatures: const [FontFeature.tabularFigures()],
    );
    if (state.hasError) {
      return Text(
        'Voice note unavailable',
        key: const ValueKey('voice-note-duration'),
        style: style,
      );
    }
    final current = formatVoiceNoteDuration(position);
    final full = formatVoiceNoteDuration(total);
    return Text(
      '$current / $full',
      key: const ValueKey('voice-note-duration'),
      semanticsLabel: '$current of $full',
      style: style,
    );
  }
}
