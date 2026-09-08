part of '../voice_note_composer_recorder.dart';

/// Pill contents while the mic is held: red dot, timer, live waveform, the
/// tracking cancel hint, and the mic slot the lock chip anchors to.
class _HoldRecorderRow extends StatelessWidget {
  const _HoldRecorderRow({
    super.key,
    required this.state,
    required this.elapsed,
    required this.samples,
    required this.sampleSequence,
    required this.isStarted,
    required this.isFinishing,
    required this.lockChipLink,
    required this.onSend,
    required this.onCancel,
  });

  final VoiceNoteRecorderState state;
  final Duration elapsed;
  final List<double> samples;
  final int sampleSequence;
  final bool isStarted;
  final bool isFinishing;
  final LayerLink lockChipLink;
  final VoidCallback onSend;
  final VoidCallback onCancel;

  @override
  Widget build(BuildContext context) {
    final holding = state.hasPointer && !isFinishing;
    return Row(
      key: const ValueKey('voice-note-recorder-hold-row'),
      children: [
        const SizedBox(width: Grid.half),
        _RecordingDot(isLive: !isFinishing),
        const SizedBox(width: Grid.xxs),
        _RecorderTimer(elapsed: elapsed),
        const SizedBox(width: Grid.xxs),
        Expanded(
          child: _LiveWaveform(
            samples: samples,
            sampleSequence: sampleSequence,
          ),
        ),
        const SizedBox(width: Grid.xxs),
        if (!isFinishing) _SlideToCancelHint(state: state, onCancel: onCancel),
        const SizedBox(width: Grid.half),
        CompositedTransformTarget(
          link: lockChipLink,
          child: _HoldMicSlot(
            holding: holding,
            isFinishing: isFinishing,
            cancelProgress: state.cancelProgress,
            onSend: isStarted && !isFinishing ? onSend : null,
          ),
        ),
      ],
    );
  }
}

/// Trailing 44 px slot of the hold row. While a finger is down it is the
/// held mic (not actionable); hands free it becomes the Send control.
class _HoldMicSlot extends StatelessWidget {
  const _HoldMicSlot({
    required this.holding,
    required this.isFinishing,
    required this.cancelProgress,
    required this.onSend,
  });

  final bool holding;
  final bool isFinishing;
  final double cancelProgress;
  final VoidCallback? onSend;

  @override
  Widget build(BuildContext context) {
    if (isFinishing) {
      return SizedBox.square(
        dimension: _recorderControlSize,
        child: Center(
          child: BuzzLoadingIndicator(
            size: 18,
            color: context.colors.primary,
            semanticLabel: 'Finishing voice note',
          ),
        ),
      );
    }
    if (!holding) {
      return _RecorderRoundButton(
        key: const ValueKey('voice-note-recorder-send'),
        label: 'Send voice note',
        icon: LucideIcons.arrowUp,
        foreground: context.colors.onPrimary,
        background: context.colors.primary,
        onPressed: onSend,
      );
    }
    final background = Color.lerp(
      context.colors.primary,
      context.colors.error,
      cancelProgress,
    )!;
    return Semantics(
      container: true,
      label: 'Recording voice note',
      hint: 'Release to send, slide left to cancel, slide up to lock',
      excludeSemantics: true,
      child: SizedBox.square(
        dimension: _recorderControlSize,
        child: Center(
          child: AnimatedScale(
            key: const ValueKey('voice-note-recorder-held-mic'),
            scale: 1.12,
            duration: const Duration(milliseconds: 120),
            child: Container(
              width: _recorderControlSize,
              height: _recorderControlSize,
              decoration: BoxDecoration(
                color: background,
                shape: BoxShape.circle,
                boxShadow: [
                  BoxShadow(
                    color: background.withValues(alpha: 0.18),
                    spreadRadius: Grid.xxs,
                  ),
                ],
              ),
              child: Icon(
                LucideIcons.mic,
                size: 20,
                color: context.colors.onPrimary,
              ),
            ),
          ),
        ),
      ),
    );
  }
}
