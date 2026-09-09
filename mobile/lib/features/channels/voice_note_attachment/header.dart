part of '../voice_note_attachment.dart';

/// Time row from the design: `0:24 · Voice note` at idle, then
/// `0:16 · Neo · 0:24` once playback has started (sender inline), or the
/// unavailable message on failure.
class _VoiceNoteTimeLabel extends StatelessWidget {
  const _VoiceNoteTimeLabel({
    required this.state,
    required this.position,
    required this.total,
    required this.senderName,
    required this.idleLabel,
    required this.onIdleTap,
  });

  final VoiceNotePlaybackState state;
  final Duration position;
  final Duration total;
  final String? senderName;

  /// Trailing word at idle: "Voice note" on cards, "Tap to review" while
  /// reviewing a take.
  final String idleLabel;

  /// Tap handler for the idle label (the review row plays on tap); the play
  /// button stays the only semantics owner of that action.
  final VoidCallback? onIdleTap;

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
    final sender = senderName?.trim();
    final hasSender = sender != null && sender.isNotEmpty;
    final full = formatVoiceNoteDuration(total);
    final hasStarted = state.isPlaying || position > Duration.zero;
    if (!hasStarted) {
      final label = Text(
        '$full · $idleLabel',
        key: const ValueKey('voice-note-duration'),
        semanticsLabel: hasSender
            ? 'Voice note from $sender, $full'
            : 'Voice note, $full',
        maxLines: 1,
        overflow: TextOverflow.ellipsis,
        style: style,
      );
      if (onIdleTap == null) return label;
      return ExcludeSemantics(
        child: GestureDetector(
          key: const ValueKey('voice-note-idle-label-tap'),
          behavior: HitTestBehavior.opaque,
          onTap: onIdleTap,
          child: label,
        ),
      );
    }
    final current = formatVoiceNoteDuration(position);
    return Text(
      hasSender ? '$current · $sender · $full' : '$current · $full',
      key: const ValueKey('voice-note-duration'),
      semanticsLabel: hasSender
          ? '$current of $full, $sender'
          : '$current of $full',
      maxLines: 1,
      overflow: TextOverflow.ellipsis,
      style: style,
    );
  }
}

/// X that removes a pending attachment or, on the review row, discards the
/// take. Each variant owns exactly one label.
class _VoiceNoteRemoveButton extends StatelessWidget {
  const _VoiceNoteRemoveButton({
    required this.isReview,
    required this.onPressed,
  });

  final bool isReview;
  final VoidCallback onPressed;

  @override
  Widget build(BuildContext context) {
    final size = isReview ? 36.0 : 40.0;
    final label = isReview ? 'Cancel recording' : 'Remove voice note';
    return Semantics(
      container: true,
      button: true,
      label: label,
      onTap: onPressed,
      excludeSemantics: true,
      child: SizedBox.square(
        dimension: size,
        child: IconButton(
          key: ValueKey(
            isReview
                ? 'voice-note-review-dismiss'
                : 'composer-voice-note-remove',
          ),
          tooltip: label,
          onPressed: onPressed,
          style: IconButton.styleFrom(
            minimumSize: Size.square(size),
            maximumSize: Size.square(size),
            padding: EdgeInsets.zero,
            tapTargetSize: MaterialTapTargetSize.shrinkWrap,
          ),
          icon: const Icon(LucideIcons.x, size: 18),
        ),
      ),
    );
  }
}
