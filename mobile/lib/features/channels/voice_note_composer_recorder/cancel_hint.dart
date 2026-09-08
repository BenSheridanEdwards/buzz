part of '../voice_note_composer_recorder.dart';

/// "Slide to cancel" hint that follows the finger and fades to its arrow as
/// the cancel threshold approaches. Hands free it becomes the tap-to-cancel
/// path, the only owner of the "Cancel recording" label.
class _SlideToCancelHint extends StatelessWidget {
  const _SlideToCancelHint({required this.state, required this.onCancel});

  final VoiceNoteRecorderState state;
  final VoidCallback onCancel;

  @override
  Widget build(BuildContext context) {
    final tracking = state.hasPointer;
    final progress = state.cancelProgress;
    final travel = tracking
        ? state.dragOffset.dx.clamp(-voiceNoteCancelSlideDistance, 0.0) * 0.5
        : 0.0;
    final color = context.colors.onSurfaceVariant;
    final hint = Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        Icon(LucideIcons.chevronLeft, size: 16, color: color),
        AnimatedOpacity(
          duration: const Duration(milliseconds: 80),
          opacity: tracking ? 1 - progress : 1,
          child: Text(
            tracking ? 'Slide to cancel' : 'Cancel',
            key: const ValueKey('voice-note-recorder-cancel-hint'),
            style: context.textTheme.bodySmall?.copyWith(color: color),
          ),
        ),
      ],
    );
    if (tracking) {
      return ExcludeSemantics(
        child: Transform.translate(offset: Offset(travel, 0), child: hint),
      );
    }
    return Semantics(
      container: true,
      button: true,
      label: 'Cancel recording',
      onTap: onCancel,
      excludeSemantics: true,
      child: InkWell(
        key: const ValueKey('voice-note-recorder-cancel'),
        borderRadius: BorderRadius.circular(Radii.full),
        onTap: onCancel,
        child: Padding(
          padding: const EdgeInsets.symmetric(
            horizontal: Grid.half,
            vertical: Grid.xxs,
          ),
          child: hint,
        ),
      ),
    );
  }
}
