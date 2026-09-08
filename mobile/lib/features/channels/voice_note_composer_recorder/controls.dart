part of '../voice_note_composer_recorder.dart';

/// Diameter of the round controls inside the pill (Send while hands free).
const _recorderControlSize = 44.0;

/// Diameter of the round controls in the locked and review panels, and the
/// height of their wide Send and Record again pills (RecordingLocked).
const _recorderPanelControlSize = 52.0;

/// Diameter of the mic while it is held (RecordingHold).
const _heldMicSize = 64.0;

/// Halo drawn outside the held mic.
const _heldMicHalo = 10.0;

class _RecordingDot extends StatelessWidget {
  const _RecordingDot({required this.isLive});

  final bool isLive;

  @override
  Widget build(BuildContext context) {
    final color = isLive
        ? context.colors.error
        : context.colors.onSurfaceVariant;
    return ExcludeSemantics(
      child: Container(
        key: const ValueKey('voice-note-recorder-dot'),
        width: 10,
        height: 10,
        decoration: BoxDecoration(
          color: color,
          shape: BoxShape.circle,
          boxShadow: isLive
              ? [
                  BoxShadow(
                    color: color.withValues(alpha: 0.25),
                    spreadRadius: Grid.half,
                  ),
                ]
              : null,
        ),
      ),
    );
  }
}

/// Elapsed-time readout. The visible text ticks five times a second; the
/// node a screen reader hears is a live region whose label only changes
/// every [voiceNoteTimerAnnounceInterval], and names the cap when it is
/// [voiceNoteCapWarning] away, so the timer never re-reads itself per tick.
class _RecorderTimer extends StatelessWidget {
  const _RecorderTimer({required this.elapsed, required this.announced});

  final Duration elapsed;
  final Duration announced;

  @override
  Widget build(BuildContext context) {
    final nearCap = voiceNoteMaxDuration - elapsed <= voiceNoteCapWarning;
    final remaining = voiceNoteMaxDuration - announced;
    final label = announced == Duration.zero
        ? 'Recording'
        : remaining <= voiceNoteCapWarning
        ? 'Recording, ${formatVoiceNoteDuration(announced)}, '
              '${remaining.inSeconds} seconds before the limit'
        : 'Recording, ${formatVoiceNoteDuration(announced)}';
    return Semantics(
      key: const ValueKey('voice-note-recorder-timer-semantics'),
      liveRegion: true,
      label: label,
      excludeSemantics: true,
      child: Text(
        formatVoiceNoteDuration(elapsed),
        key: const ValueKey('voice-note-recorder-duration'),
        style: context.textTheme.titleSmall?.copyWith(
          color: nearCap ? context.colors.error : context.colors.onSurface,
          fontWeight: FontWeight.w600,
          fontFeatures: const [FontFeature.tabularFigures()],
        ),
      ),
    );
  }
}

class _LiveWaveform extends StatelessWidget {
  const _LiveWaveform({required this.samples, required this.sampleSequence});

  final List<double> samples;
  final int sampleSequence;

  @override
  Widget build(BuildContext context) {
    final reducedMotion = MediaQuery.disableAnimationsOf(context);
    return LayoutBuilder(
      builder: (context, constraints) {
        final barCount = ((constraints.maxWidth + 2) / 5).floor().clamp(
          1,
          1024,
        );
        final recentSamples = samples.length <= barCount
            ? samples
            : samples.sublist(samples.length - barCount);
        final waveform = [
          ...List<double>.filled(barCount - recentSamples.length, 0),
          ...recentSamples,
        ];
        return ClipRect(
          child: TweenAnimationBuilder<double>(
            key: ValueKey(sampleSequence),
            tween: Tween(begin: reducedMotion ? 0 : 5, end: 0),
            duration: reducedMotion
                ? Duration.zero
                : const Duration(milliseconds: 90),
            curve: Curves.linear,
            builder: (context, offset, child) =>
                Transform.translate(offset: Offset(offset, 0), child: child),
            child: VoiceNoteWaveform(
              samples: waveform,
              progress: 1,
              fadeEdges: true,
              height: 24,
              minimumBarHeight: 3,
              maximumBarHeight: 20,
              colorOpacity: 0.75,
            ),
          ),
        );
      },
    );
  }
}

/// Round control with exactly one semantics owner for its label.
class _RecorderRoundButton extends StatelessWidget {
  const _RecorderRoundButton({
    super.key,
    required this.label,
    required this.icon,
    required this.foreground,
    required this.background,
    required this.onPressed,
    this.size = _recorderControlSize,
  });

  final String label;
  final IconData icon;
  final Color foreground;
  final Color background;
  final VoidCallback? onPressed;
  final double size;

  @override
  Widget build(BuildContext context) {
    final enabled = onPressed != null;
    return Semantics(
      container: true,
      button: true,
      enabled: enabled,
      label: label,
      onTap: onPressed,
      excludeSemantics: true,
      child: Tooltip(
        message: label,
        child: SizedBox.square(
          dimension: size,
          child: Material(
            color: enabled ? background : background.withValues(alpha: 0.5),
            shape: const CircleBorder(),
            child: InkWell(
              customBorder: const CircleBorder(),
              onTap: onPressed == null
                  ? null
                  : () {
                      unawaited(HapticFeedback.selectionClick());
                      onPressed!();
                    },
              child: Center(child: Icon(icon, size: 20, color: foreground)),
            ),
          ),
        ),
      ),
    );
  }
}

/// Full-width pill action used for Send and Record again.
class _RecorderWideButton extends StatelessWidget {
  const _RecorderWideButton({
    super.key,
    required this.label,
    required this.icon,
    required this.onPressed,
    this.emphasized = true,
    this.height = _recorderControlSize,
  });

  final String label;
  final IconData icon;
  final VoidCallback? onPressed;
  final bool emphasized;
  final double height;

  @override
  Widget build(BuildContext context) {
    final enabled = onPressed != null;
    final background = emphasized
        ? context.colors.primary
        : context.colors.surface;
    final foreground = emphasized
        ? context.colors.onPrimary
        : context.colors.onSurface;
    return Semantics(
      container: true,
      button: true,
      enabled: enabled,
      label: label,
      onTap: onPressed,
      excludeSemantics: true,
      child: SizedBox(
        height: height,
        child: Material(
          color: enabled ? background : background.withValues(alpha: 0.5),
          borderRadius: BorderRadius.circular(Radii.full),
          child: InkWell(
            borderRadius: BorderRadius.circular(Radii.full),
            onTap: onPressed == null
                ? null
                : () {
                    unawaited(HapticFeedback.selectionClick());
                    onPressed!();
                  },
            child: Row(
              mainAxisAlignment: MainAxisAlignment.center,
              children: [
                Icon(icon, size: 18, color: foreground),
                const SizedBox(width: Grid.xxs),
                Text(
                  label,
                  style: context.textTheme.titleSmall?.copyWith(
                    color: foreground,
                    fontWeight: emphasized ? FontWeight.w600 : FontWeight.w500,
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
