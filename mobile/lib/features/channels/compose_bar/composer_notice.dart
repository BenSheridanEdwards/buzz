part of '../compose_bar.dart';

/// The two messages the recorder raises when the microphone is refused.
///
/// They come from `VoiceNoteRecording` and `VoiceNoteComposerRecorder`, which
/// speak in strings; matching them here keeps the notice able to offer the
/// way back to Settings without threading a new error type through every
/// caller that sets `uploadError`.
const _microphoneNotices = {
  'Microphone access is required to record a voice note.',
  'Buzz could not start recording. Check microphone access.',
};

bool _isMicrophoneNotice(String message) =>
    _microphoneNotices.contains(message);

/// An inline notice in the composer for a failed attachment or recording.
///
/// Replaces the bare red `Text` that used to sit in the composer with no
/// container and nothing to tap. A microphone refusal is a permission the
/// user has to change in the system settings, so that case carries an
/// **Open settings** action: a notice that names a problem the user cannot
/// act on from where they are strands them (Review-Proven Rule 6).
class _ComposerNotice extends StatelessWidget {
  final String message;

  /// Opens the app's system settings. Only offered for a microphone refusal.
  final VoidCallback? onOpenSettings;

  const _ComposerNotice({required this.message, this.onOpenSettings});

  @override
  Widget build(BuildContext context) {
    final colors = context.colors;
    final isMicrophone = _isMicrophoneNotice(message);
    final showSettings = isMicrophone && onOpenSettings != null;
    return Semantics(
      liveRegion: true,
      container: true,
      child: Container(
        key: const ValueKey('composer-notice'),
        padding: const EdgeInsets.symmetric(
          horizontal: Grid.sm,
          vertical: Grid.xs,
        ),
        decoration: BoxDecoration(
          color: colors.errorContainer,
          borderRadius: BorderRadius.circular(Radii.md),
        ),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.center,
          children: [
            ExcludeSemantics(
              child: Icon(
                isMicrophone ? LucideIcons.micOff : LucideIcons.circleAlert,
                size: 16,
                color: colors.onErrorContainer,
              ),
            ),
            const SizedBox(width: Grid.xs),
            Expanded(
              child: Text(
                message,
                style: context.textTheme.bodySmall?.copyWith(
                  color: colors.onErrorContainer,
                ),
              ),
            ),
            if (showSettings) ...[
              const SizedBox(width: Grid.xs),
              TextButton(
                key: const ValueKey('composer-notice-open-settings'),
                onPressed: onOpenSettings,
                style: TextButton.styleFrom(
                  foregroundColor: colors.onErrorContainer,
                  padding: const EdgeInsets.symmetric(horizontal: Grid.xs),
                  minimumSize: const Size(0, 32),
                  tapTargetSize: MaterialTapTargetSize.shrinkWrap,
                ),
                child: const Text('Open settings'),
              ),
            ],
          ],
        ),
      ),
    );
  }
}
