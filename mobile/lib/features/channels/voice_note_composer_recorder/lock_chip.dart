part of '../voice_note_composer_recorder.dart';

/// Lock chip floating above the mic while the user holds. Sliding up fills
/// it; tapping it is the non-drag way to lock (rule 8).
class _LockChipFollower extends StatelessWidget {
  const _LockChipFollower({
    required this.link,
    required this.progress,
    required this.onLock,
  });

  final LayerLink link;
  final double progress;
  final VoidCallback onLock;

  @override
  Widget build(BuildContext context) => Positioned(
    left: 0,
    top: 0,
    child: CompositedTransformFollower(
      link: link,
      showWhenUnlinked: false,
      targetAnchor: Alignment.topCenter,
      followerAnchor: Alignment.bottomCenter,
      offset: const Offset(0, -Grid.xs),
      child: _LockChip(progress: progress, onLock: onLock),
    ),
  );
}

class _LockChip extends StatelessWidget {
  const _LockChip({required this.progress, required this.onLock});

  final double progress;
  final VoidCallback onLock;

  @override
  Widget build(BuildContext context) {
    final lift = -Grid.xxs * progress;
    final iconColor = Color.lerp(
      context.colors.onSurfaceVariant,
      context.colors.primary,
      progress,
    )!;
    return Semantics(
      container: true,
      button: true,
      label: 'Lock recording',
      onTap: onLock,
      excludeSemantics: true,
      child: Transform.translate(
        offset: Offset(0, lift),
        child: Material(
          key: const ValueKey('voice-note-lock-chip'),
          color: context.colors.surfaceContainerHighest,
          shape: StadiumBorder(
            side: BorderSide(color: context.colors.outlineVariant),
          ),
          elevation: 2,
          shadowColor: Colors.black.withValues(alpha: 0.2),
          child: InkWell(
            customBorder: const StadiumBorder(),
            onTap: () {
              unawaited(HapticFeedback.selectionClick());
              onLock();
            },
            child: SizedBox(
              width: _recorderControlSize,
              height: Grid.xxl,
              child: Column(
                mainAxisAlignment: MainAxisAlignment.center,
                children: [
                  Icon(
                    progress >= 1 ? LucideIcons.lock : LucideIcons.lockOpen,
                    size: 18,
                    color: iconColor,
                  ),
                  const SizedBox(height: Grid.half),
                  Icon(
                    LucideIcons.chevronUp,
                    size: 14,
                    color: iconColor.withValues(alpha: 0.6 + 0.4 * progress),
                  ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}
