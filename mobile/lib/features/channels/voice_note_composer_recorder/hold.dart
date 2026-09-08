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
    required this.timer,
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
  final Widget timer;
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
        timer,
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
            onSend: isStarted && !isFinishing ? onSend : null,
          ),
        ),
      ],
    );
  }
}

/// Trailing 44 px slot of the hold row. While a finger is down it only
/// anchors the floating held mic ([_HeldMicFollower]); hands free it becomes
/// the Send control.
class _HoldMicSlot extends StatelessWidget {
  const _HoldMicSlot({
    required this.holding,
    required this.isFinishing,
    required this.onSend,
  });

  final bool holding;
  final bool isFinishing;
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
    return const SizedBox.square(
      key: ValueKey('voice-note-recorder-held-mic-anchor'),
      dimension: _recorderControlSize,
    );
  }
}

/// The 64 px held mic with its halo, floated above the pill through the
/// recorder's overlay portal so the composer surface does not clip it. It is
/// the single semantics owner of "Recording voice note" and never takes a
/// second finger: the hold is tracked by the listener above the pill.
class _HeldMicFollower extends StatelessWidget {
  const _HeldMicFollower({required this.link, required this.cancelProgress});

  final LayerLink link;
  final double cancelProgress;

  @override
  Widget build(BuildContext context) {
    final background = Color.lerp(
      context.colors.primary,
      context.colors.error,
      cancelProgress,
    )!;
    final reducedMotion = MediaQuery.disableAnimationsOf(context);
    return Positioned(
      left: 0,
      top: 0,
      child: CompositedTransformFollower(
        link: link,
        showWhenUnlinked: false,
        targetAnchor: Alignment.center,
        followerAnchor: Alignment.center,
        child: IgnorePointer(
          child: Semantics(
            container: true,
            label: 'Recording voice note',
            hint: 'Release to send, slide sideways to cancel, slide up to lock',
            excludeSemantics: true,
            child: TweenAnimationBuilder<double>(
              tween: Tween(
                begin: reducedMotion
                    ? 1.0
                    : _recorderControlSize / _heldMicSize,
                end: 1,
              ),
              duration: reducedMotion
                  ? Duration.zero
                  : const Duration(milliseconds: 120),
              curve: Curves.easeOutCubic,
              builder: (context, scale, child) =>
                  Transform.scale(scale: scale, child: child),
              child: Container(
                key: const ValueKey('voice-note-recorder-held-mic'),
                width: _heldMicSize,
                height: _heldMicSize,
                decoration: BoxDecoration(
                  color: background,
                  shape: BoxShape.circle,
                  boxShadow: [
                    BoxShadow(
                      color: background.withValues(alpha: 0.12),
                      spreadRadius: _heldMicHalo,
                    ),
                  ],
                ),
                child: Icon(
                  LucideIcons.mic,
                  size: 24,
                  color: context.colors.onPrimary,
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}
