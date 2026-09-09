part of '../voice_note_composer_recorder.dart';

/// Review step shown when the setting is on, laid out like the Preview
/// artboard: a compact `0:12 · Tap to review` row with an X, then trash,
/// Record again, and Send.
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
          VoiceNoteAttachment.review(
            path: recording.file.path,
            duration: recording.duration,
            waveform: recording.waveform,
            onDismiss: onDiscard,
          ),
        const SizedBox(height: Grid.xxs),
        Row(
          children: [
            _RecorderRoundButton(
              key: const ValueKey('voice-note-recorder-discard'),
              label: 'Discard voice note',
              icon: LucideIcons.trash2,
              size: _recorderPanelControlSize,
              foreground: context.colors.error,
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
                height: _recorderPanelControlSize,
                onPressed: onRecordAgain,
              ),
            ),
            const SizedBox(width: Grid.twelve),
            Expanded(
              child: _RecorderWideButton(
                key: const ValueKey('voice-note-recorder-send'),
                label: 'Send',
                icon: LucideIcons.arrowUp,
                height: _recorderPanelControlSize,
                onPressed: recording == null ? null : onSend,
              ),
            ),
          ],
        ),
      ],
    );
  }
}
