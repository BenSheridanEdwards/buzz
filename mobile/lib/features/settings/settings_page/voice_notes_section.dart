part of '../settings_page.dart';

class _VoiceNotesSection extends ConsumerWidget {
  const _VoiceNotesSection();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final reviewBeforeSending = ref.watch(voiceNoteReviewSettingProvider);
    void setReviewBeforeSending(bool enabled) =>
        ref.read(voiceNoteReviewSettingProvider.notifier).set(enabled);
    return AppListCard(
      label: 'Voice notes',
      verticalPadding: Grid.twelve,
      children: [
        AppListRow(
          key: const ValueKey('voice-note-review-before-sending'),
          icon: LucideIcons.mic,
          title: 'Review voice notes before sending',
          subtitle: 'Listen back and re-record before a note is attached',
          trailing: Switch.adaptive(
            value: reviewBeforeSending,
            onChanged: setReviewBeforeSending,
          ),
          onTap: () => setReviewBeforeSending(!reviewBeforeSending),
        ),
      ],
    );
  }
}
