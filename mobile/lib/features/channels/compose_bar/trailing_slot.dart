part of '../compose_bar.dart';

/// Edge of the mic and Send controls that share the pill's trailing slot.
const _composerTrailingSlotSize = 44.0;

/// One 44 px slot at the end of the pill: the mic while the draft is empty,
/// Send once there is text or a pending attachment. Both live in the same
/// box so typing or clearing never shifts the pill.
class _ComposerTrailingSlot extends StatelessWidget {
  const _ComposerTrailingSlot({
    required this.showMic,
    required this.isSending,
    required this.isSendDisabled,
    required this.onSend,
    required this.onMicPointerDown,
    required this.onMicActivate,
  });

  final bool showMic;
  final bool isSending;
  final bool isSendDisabled;
  final VoidCallback onSend;
  final void Function(int pointer, Offset origin) onMicPointerDown;
  final VoidCallback onMicActivate;

  @override
  Widget build(BuildContext context) {
    final reducedMotion = MediaQuery.disableAnimationsOf(context);
    return SizedBox.square(
      key: const ValueKey('composer-trailing-slot'),
      dimension: _composerTrailingSlotSize,
      child: AnimatedSwitcher(
        duration: reducedMotion
            ? Duration.zero
            : const Duration(milliseconds: 120),
        switchInCurve: Curves.easeOutCubic,
        switchOutCurve: Curves.easeInCubic,
        transitionBuilder: (child, animation) => FadeTransition(
          opacity: animation,
          child: ScaleTransition(
            scale: Tween<double>(begin: 0.85, end: 1).animate(animation),
            child: child,
          ),
        ),
        child: showMic
            ? _VoiceNoteMicButton(
                key: const ValueKey('composer-mic'),
                onPointerDown: onMicPointerDown,
                onActivate: onMicActivate,
              )
            : _SendButton(
                key: const ValueKey('composer-send'),
                isDisabled: isSendDisabled,
                isSending: isSending,
                onTap: onSend,
              ),
      ),
    );
  }
}

/// Mic control in the pill. A raw [Listener] reports the press so the hold
/// gesture can be tracked above the pill even after this button is replaced
/// by the recorder; the semantics action is the non-pointer way to start.
class _VoiceNoteMicButton extends StatelessWidget {
  const _VoiceNoteMicButton({
    super.key,
    required this.onPointerDown,
    required this.onActivate,
  });

  final void Function(int pointer, Offset origin) onPointerDown;
  final VoidCallback onActivate;

  @override
  Widget build(BuildContext context) => Semantics(
    container: true,
    button: true,
    label: 'Record voice note',
    hint: 'Hold to record, release to send. Double tap to record hands free.',
    onTap: onActivate,
    excludeSemantics: true,
    child: Tooltip(
      message: 'Record voice note',
      child: Listener(
        behavior: HitTestBehavior.opaque,
        onPointerDown: (event) {
          // Only a plain primary press is a hold: a right click, a stylus
          // barrel press, or a trackpad gesture must not start recording.
          if (event.buttons != kPrimaryButton) return;
          if (event.kind == PointerDeviceKind.trackpad ||
              event.kind == PointerDeviceKind.unknown) {
            return;
          }
          onPointerDown(event.pointer, event.position);
        },
        child: Container(
          width: _composerTrailingSlotSize,
          height: _composerTrailingSlotSize,
          decoration: BoxDecoration(
            color: context.colors.surfaceContainerHigh,
            shape: BoxShape.circle,
          ),
          child: Icon(
            LucideIcons.mic,
            size: 20,
            color: context.colors.onSurface,
          ),
        ),
      ),
    ),
  );
}

/// Feeds pointer travel into the recorder phase while the mic is held.
///
/// Lives above the pill, so the finger keeps being tracked while the mic
/// morphs into the recorder underneath it. A [Listener] only observes and
/// never competes with the text field's gesture arena.
class _VoiceNoteGestureTracker extends ConsumerWidget {
  const _VoiceNoteGestureTracker({required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final notifier = ref.read(voiceNoteRecorderPhaseProvider.notifier);
    return Listener(
      behavior: HitTestBehavior.translucent,
      onPointerMove: (event) =>
          notifier.pointerMoved(event.pointer, event.position),
      onPointerUp: (event) => notifier.pointerReleased(event.pointer),
      onPointerCancel: (event) => notifier.pointerLost(event.pointer),
      child: child,
    );
  }
}
