part of '../voice_note_attachment.dart';

/// Play, pause, cancel-loading, or retry control with one semantics owner.
class _VoiceNotePlaybackButton extends StatelessWidget {
  const _VoiceNotePlaybackButton({
    required this.state,
    required this.isRemote,
    required this.player,
  });

  final VoiceNotePlaybackState state;
  final bool isRemote;
  final VoiceNotePlayerController player;

  @override
  Widget build(BuildContext context) {
    final canCancelLoading = state.isLoading && state.canCancelLoading;
    final onPlaybackPressed = state.isLoading && !canCancelLoading
        ? null
        : state.hasError && !isRemote
        ? null
        : () {
            unawaited(HapticFeedback.selectionClick());
            unawaited(player.toggle());
          };
    final playbackControlLabel = state.isLoading
        ? state.isPlaying
              ? 'Pause voice note'
              : canCancelLoading
              ? 'Cancel voice note loading'
              : 'Loading voice note'
        : state.hasError && isRemote
        ? 'Retry voice note'
        : state.isPlaying
        ? 'Pause voice note'
        : 'Play voice note';
    return SizedBox.square(
      dimension: 44,
      child: Semantics(
        container: true,
        button: true,
        label: playbackControlLabel,
        onTap: onPlaybackPressed,
        excludeSemantics: true,
        child: ExcludeSemantics(
          child: IconButton.filledTonal(
            key: const ValueKey('voice-note-play-pause'),
            tooltip: playbackControlLabel,
            onPressed: onPlaybackPressed,
            style: IconButton.styleFrom(
              minimumSize: const Size.square(44),
              maximumSize: const Size.square(44),
              padding: EdgeInsets.zero,
              tapTargetSize: MaterialTapTargetSize.shrinkWrap,
            ),
            icon: state.isLoading
                ? ExcludeSemantics(
                    child: BuzzLoadingIndicator(
                      size: 18,
                      color: context.colors.onSecondaryContainer,
                    ),
                  )
                : state.hasError && isRemote
                ? Icon(
                    LucideIcons.refreshCcw,
                    key: const ValueKey('voice-note-retry-icon'),
                    size: 18,
                    color: context.colors.onSecondaryContainer,
                  )
                : VoiceNotePlayPauseIcon(
                    isPlaying: state.isPlaying,
                    color: context.colors.onSecondaryContainer,
                  ),
          ),
        ),
      ),
    );
  }
}

class _VoiceNotePlaybackRateButton extends StatelessWidget {
  const _VoiceNotePlaybackRateButton({
    super.key,
    required this.rate,
    required this.onPressed,
  });

  final double rate;
  final VoidCallback onPressed;

  @override
  Widget build(BuildContext context) {
    final next = nextVoiceNotePlaybackRate(
      rate,
      rates: voiceNoteMobilePlaybackRates,
    );
    return Semantics(
      container: true,
      button: true,
      label: 'Playback speed ${formatVoiceNotePlaybackRate(rate)}',
      hint: 'Double tap to change to ${formatVoiceNotePlaybackRate(next)}.',
      onTap: onPressed,
      excludeSemantics: true,
      child: Tooltip(
        message: 'Playback speed',
        child: Material(
          color: Colors.transparent,
          shape: StadiumBorder(
            side: BorderSide(color: context.colors.outlineVariant),
          ),
          child: InkWell(
            customBorder: const StadiumBorder(),
            onTap: onPressed,
            child: Padding(
              padding: const EdgeInsets.symmetric(
                horizontal: Grid.xxs,
                vertical: Grid.quarter,
              ),
              child: Stack(
                alignment: Alignment.center,
                children: [
                  ExcludeSemantics(
                    child: Opacity(
                      opacity: 0,
                      child: Text('1.5×', style: _rateStyle(context)),
                    ),
                  ),
                  Positioned.fill(
                    child: Center(
                      child: Text(
                        formatVoiceNotePlaybackRate(rate),
                        key: const ValueKey('voice-note-playback-rate-value'),
                        textAlign: TextAlign.center,
                        style: _rateStyle(context),
                      ),
                    ),
                  ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }

  TextStyle? _rateStyle(BuildContext context) =>
      context.textTheme.labelSmall?.copyWith(
        color: context.colors.onSurface,
        fontWeight: FontWeight.w700,
        fontFeatures: const [FontFeature.tabularFigures()],
      );
}
