part of '../voice_note_composer_recorder.dart';

/// Review step shown when the setting is on: play the note back, then
/// discard it, record again, or send it.
class _ReviewPanel extends StatelessWidget {
  const _ReviewPanel({
    super.key,
    required this.recording,
    required this.onDiscard,
    required this.onRecordAgain,
    required this.onSend,
  });

  final VoiceNoteRecording? recording;
  final VoidCallback onDiscard;
  final VoidCallback onRecordAgain;
  final VoidCallback onSend;

  @override
  Widget build(BuildContext context) {
    final recording = this.recording;
    return Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        if (recording != null)
          VoiceNoteAttachment.local(
            path: recording.file.path,
            duration: recording.duration,
            waveform: recording.waveform,
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
            Expanded(
              child: _RecorderWideButton(
                key: const ValueKey('voice-note-recorder-record-again'),
                label: 'Record again',
                icon: LucideIcons.rotateCcw,
                emphasized: false,
                onPressed: onRecordAgain,
              ),
            ),
            const SizedBox(width: Grid.twelve),
            Expanded(
              child: _RecorderWideButton(
                key: const ValueKey('voice-note-recorder-send'),
                label: 'Send',
                icon: LucideIcons.arrowUp,
                onPressed: recording == null ? null : onSend,
              ),
            ),
          ],
        ),
      ],
    );
  }
}
