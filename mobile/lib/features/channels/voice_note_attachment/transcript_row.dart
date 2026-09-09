part of '../voice_note_attachment.dart';

/// "Transcript" header that folds and unfolds the transcript body.
///
/// The header is the single semantics owner for the toggle. Every card
/// starts folded (ReceivedIdle); in a DM the transcript unfolds when
/// playback starts (Main) unless this message remembers a choice, and a
/// fold made while playing stays folded (ReceivedCollapsed). Choices are
/// remembered per message, so one toggle never moves another card.
class _VoiceNoteTranscriptRow extends ConsumerWidget {
  const _VoiceNoteTranscriptRow({
    required this.messageId,
    required this.transcript,
    required this.opensOnPlayback,
    required this.hasPlayed,
  });

  final String messageId;
  final String transcript;
  final bool opensOnPlayback;
  final bool hasPlayed;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final remembered = ref.watch(
      voiceNoteTranscriptChoicesProvider.select(
        (choices) => choices[messageId],
      ),
    );
    final isOpen = remembered ?? (opensOnPlayback && hasPlayed);
    final color = context.colors.onSurfaceVariant;
    final reducedMotion = MediaQuery.disableAnimationsOf(context);
    void toggle() {
      unawaited(HapticFeedback.selectionClick());
      ref
          .read(voiceNoteTranscriptChoicesProvider.notifier)
          .set(messageId, open: !isOpen);
    }

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.only(top: Grid.xxs),
          child: Divider(height: 1, color: context.colors.outlineVariant),
        ),
        Semantics(
          container: true,
          button: true,
          expanded: isOpen,
          label: 'Transcript',
          hint: isOpen ? 'Double tap to hide' : 'Double tap to show',
          onTap: toggle,
          excludeSemantics: true,
          child: InkWell(
            key: const ValueKey('voice-note-transcript-toggle'),
            onTap: toggle,
            child: Padding(
              padding: const EdgeInsets.only(top: Grid.xxs),
              child: Row(
                children: [
                  Icon(LucideIcons.text, size: 14, color: color),
                  const SizedBox(width: Grid.xxs),
                  Expanded(
                    child: Text(
                      isOpen || hasPlayed ? 'Transcript' : 'Show transcript',
                      key: const ValueKey('voice-note-transcript-header'),
                      style: context.textTheme.labelMedium?.copyWith(
                        color: color,
                        fontWeight: FontWeight.w500,
                      ),
                    ),
                  ),
                  Icon(
                    isOpen ? LucideIcons.chevronUp : LucideIcons.chevronDown,
                    size: 16,
                    color: color,
                  ),
                ],
              ),
            ),
          ),
        ),
        AnimatedSize(
          duration: reducedMotion
              ? Duration.zero
              : const Duration(milliseconds: 140),
          curve: Curves.easeOutCubic,
          alignment: Alignment.topCenter,
          child: isOpen
              ? Padding(
                  padding: const EdgeInsets.only(
                    top: Grid.xxs,
                    left: Grid.sm - Grid.quarter,
                  ),
                  child: Text(
                    transcript,
                    key: const ValueKey('voice-note-transcript-body'),
                    style: context.textTheme.bodyMedium?.copyWith(
                      color: context.colors.onSurfaceVariant,
                    ),
                  ),
                )
              : const SizedBox(
                  key: ValueKey('voice-note-transcript-folded'),
                  width: double.infinity,
                ),
        ),
      ],
    );
  }
}
