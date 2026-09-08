part of '../compose_bar.dart';

class _ComposerVoiceNote {
  const _ComposerVoiceNote({
    required this.beginHold,
    required this.onKeyboardHidden,
    required this.onDraftIdentityChanged,
    required ValueNotifier<bool> isPreparing,
    required ValueNotifier<bool> isRecording,
    required VoidCallback onCancel,
    required ValueChanged<VoiceNoteRecording> onRecorded,
    required ValueChanged<String> onError,
  }) : _isPreparing = isPreparing,
       _isRecording = isRecording,
       _onCancel = onCancel,
       _onRecorded = onRecorded,
       _onError = onError;

  /// Starts recording from the mic slot. With a [pointer] the finger is held
  /// and tracked; without one the recording is hands free (tap, screen
  /// reader, or keyboard path).
  final void Function({int? pointer, Offset origin}) beginHold;
  final VoidCallback onKeyboardHidden;
  final VoidCallback onDraftIdentityChanged;
  final ValueNotifier<bool> _isPreparing;
  final ValueNotifier<bool> _isRecording;
  final VoidCallback _onCancel;
  final ValueChanged<VoiceNoteRecording> _onRecorded;
  final ValueChanged<String> _onError;

  bool get isPreparing => _isPreparing.value;
  bool get isRecording => _isRecording.value;
  Widget? get recorder => isRecording
      ? VoiceNoteComposerRecorder(
          onCancel: _onCancel,
          onRecorded: _onRecorded,
          onError: _onError,
        )
      : null;
}

bool _voiceNoteFullWidth(
  _ComposerVoiceNote voiceNote,
  List<_PendingAttachment> attachments,
) =>
    voiceNote.isPreparing ||
    voiceNote.isRecording ||
    attachments.any((item) => item.kind == _PendingAttachmentKind.voiceNote);

_ComposerVoiceNote _useComposerVoiceNote({
  required BuildContext context,
  required WidgetRef ref,
  required FocusNode focusNode,
  required ValueNotifier<bool> isComposerExpanded,
  required ValueNotifier<bool> showFormatting,
  required ValueNotifier<_AttachmentSurface> attachmentSurface,
  required ValueNotifier<String?> uploadError,
  required ObjectRef<int> draftRevision,
  required ValueNotifier<List<_PendingAttachment>> attachments,
}) {
  final isPreparing = useState(false);
  final isRecording = useState(false);
  final phaseNotifier = ref.read(voiceNoteRecorderPhaseProvider.notifier);

  final resetForDraftIdentityChange = useCallback(() {
    isPreparing.value = false;
    isRecording.value = false;
    // Draft identity changes are observed during build; the provider may
    // only be written once the frame is out of it.
    scheduleMicrotask(phaseNotifier.reset);
  }, [isPreparing, isRecording, phaseNotifier]);

  // Every exit from the phase machine (slide-to-cancel, failure, finish)
  // clears the composer's own flags here so no path can leave `isRecording`
  // stuck behind a phase that already returned to idle.
  ref.listen<VoiceNoteRecorderState>(voiceNoteRecorderPhaseProvider, (
    previous,
    next,
  ) {
    switch (next.phase) {
      case VoiceNoteRecorderPhase.idle:
        isPreparing.value = false;
        isRecording.value = false;
      case VoiceNoteRecorderPhase.finishing when !isRecording.value:
        // Released before the recorder mounted: nothing was captured.
        phaseNotifier.reset();
      case VoiceNoteRecorderPhase.holding:
      case VoiceNoteRecorderPhase.locked:
      case VoiceNoteRecorderPhase.paused:
      case VoiceNoteRecorderPhase.finishing:
      case VoiceNoteRecorderPhase.reviewing:
        break;
    }
  });

  void beginRecording() {
    if (!isPreparing.value) return;
    isPreparing.value = false;
    isRecording.value = true;
  }

  bool start() {
    if (attachments.value.isNotEmpty) {
      uploadError.value = 'A voice note must be the only attachment.';
      return false;
    }
    attachmentSurface.value = _AttachmentSurface.closed;
    showFormatting.value = false;
    isComposerExpanded.value = false;
    _dismissComposerKeyboard(focusNode);
    if (ref.read(huddleSessionProvider).isInSession) {
      uploadError.value = 'Leave the Huddle before recording a voice note.';
      return false;
    }
    uploadError.value = null;
    draftRevision.value += 1;
    isPreparing.value = true;
    if (View.of(context).viewInsets.bottom == 0) beginRecording();
    return true;
  }

  void beginHold({int? pointer, Offset origin = Offset.zero}) {
    if (ref.read(voiceNoteRecorderPhaseProvider).isActive) return;
    if (!start()) return;
    phaseNotifier.begin(pointer: pointer, origin: origin);
  }

  void cancel() {
    isPreparing.value = false;
    isRecording.value = false;
    phaseNotifier.reset();
  }

  void fail(String message) {
    uploadError.value = message;
    cancel();
  }

  void complete(VoiceNoteRecording recording) {
    draftRevision.value += 1;
    uploadError.value = null;
    attachments.value = [
      ...attachments.value,
      _PendingAttachment(
        file: recording.file,
        kind: _PendingAttachmentKind.voiceNote,
        deleteAfterUse: true,
        duration: recording.duration,
        waveform: recording.waveform,
      ),
    ];
    isRecording.value = false;
    phaseNotifier.reset();
  }

  return _ComposerVoiceNote(
    beginHold: beginHold,
    onKeyboardHidden: beginRecording,
    onDraftIdentityChanged: resetForDraftIdentityChange,
    isPreparing: isPreparing,
    isRecording: isRecording,
    onCancel: cancel,
    onRecorded: complete,
    onError: fail,
  );
}
