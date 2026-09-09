part of '../compose_bar.dart';

class _SendButton extends StatelessWidget {
  final bool isSending;
  final bool isDisabled;
  final VoidCallback onTap;

  const _SendButton({
    super.key,
    required this.isSending,
    required this.onTap,
    this.isDisabled = false,
  });

  @override
  Widget build(BuildContext context) {
    return SizedBox(
      width: _composerTrailingSlotSize,
      height: _composerTrailingSlotSize,
      child: IconButton(
        onPressed: (isSending || isDisabled)
            ? null
            : () => _runComposerAction(onTap),
        style: IconButton.styleFrom(
          backgroundColor: context.colors.primary,
          disabledBackgroundColor: context.colors.primary.withValues(
            alpha: 0.5,
          ),
          shape: const CircleBorder(),
        ),
        padding: EdgeInsets.zero,
        tooltip: 'Send message',
        icon: isSending
            ? BuzzLoadingIndicator(
                size: 18,
                color: context.colors.onPrimary,
                semanticLabel: 'Sending message',
              )
            : Icon(
                LucideIcons.arrowUp,
                size: 18,
                color: context.colors.onPrimary,
              ),
      ),
    );
  }
}

String _formatUploadError(Object error) {
  return error.toString().replaceFirst('Exception: ', '');
}
