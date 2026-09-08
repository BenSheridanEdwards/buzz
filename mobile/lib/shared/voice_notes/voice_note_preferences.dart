import 'package:hooks_riverpod/hooks_riverpod.dart';

import '../theme/theme_provider.dart';

/// Shared-preferences key remembering, per message, whether the user last
/// left its transcript open or folded.
const voiceNoteTranscriptChoicesPrefsKey = 'voice_note_transcript_choices';

/// Most remembered transcript choices kept on a device; the oldest fall off
/// so the list stays bounded however many notes a user opens.
const voiceNoteTranscriptChoicesLimit = 200;

/// Shared-preferences key for the review-before-sending composer setting.
const voiceNoteReviewBeforeSendPrefsKey = 'voice_note_review_before_send';

/// Device-local setting that shows a playback step before a voice note is
/// attached. Off by default so release still sends straight away.
class VoiceNoteReviewSettingNotifier extends Notifier<bool> {
  @override
  bool build() =>
      ref.read(savedPrefsProvider).getBool(voiceNoteReviewBeforeSendPrefsKey) ??
      false;

  /// Persists [enabled] as one atomic write under a single key.
  void set(bool enabled) {
    state = enabled;
    ref
        .read(savedPrefsProvider)
        .setBool(voiceNoteReviewBeforeSendPrefsKey, enabled);
  }
}

/// Provides the review-before-sending voice-note setting.
final voiceNoteReviewSettingProvider =
    NotifierProvider<VoiceNoteReviewSettingNotifier, bool>(
      VoiceNoteReviewSettingNotifier.new,
    );

/// Transcript fold choices remembered per message id on this device, in
/// the order they were made. A message without an entry falls back to its
/// channel default, so one toggle never folds or unfolds every other card.
class VoiceNoteTranscriptChoicesNotifier extends Notifier<Map<String, bool>> {
  @override
  Map<String, bool> build() {
    final saved =
        ref
            .read(savedPrefsProvider)
            .getStringList(voiceNoteTranscriptChoicesPrefsKey) ??
        const [];
    return {
      for (final entry in saved)
        if (entry.split('=') case [final id, final open] when id.isNotEmpty)
          id: open == '1',
    };
  }

  /// Remembered choice for [messageId], or `null` when none was made.
  bool? choiceFor(String messageId) => state[messageId];

  /// Remembers [open] for [messageId] as one atomic write of the whole list,
  /// dropping the oldest entries past [voiceNoteTranscriptChoicesLimit].
  void set(String messageId, {required bool open}) {
    final next = {...state}
      ..remove(messageId)
      ..[messageId] = open;
    while (next.length > voiceNoteTranscriptChoicesLimit) {
      next.remove(next.keys.first);
    }
    state = next;
    ref.read(savedPrefsProvider).setStringList(
      voiceNoteTranscriptChoicesPrefsKey,
      [
        for (final entry in next.entries)
          '${entry.key}=${entry.value ? '1' : '0'}',
      ],
    );
  }
}

/// Provides the remembered transcript fold choices keyed by message id.
final voiceNoteTranscriptChoicesProvider =
    NotifierProvider<VoiceNoteTranscriptChoicesNotifier, Map<String, bool>>(
      VoiceNoteTranscriptChoicesNotifier.new,
    );

/// Playback rate chosen on each voice-note card, keyed by its source, so
/// scrolling a card off screen and back does not reset the speed pill.
class VoiceNotePlaybackRatesNotifier extends Notifier<Map<String, double>> {
  @override
  Map<String, double> build() => const {};

  /// Remembers [rate] for the card playing [source].
  void set(String source, double rate) {
    state = {...state, source: rate};
  }
}

/// Provides the per-card playback rates for this session.
final voiceNotePlaybackRatesProvider =
    NotifierProvider<VoiceNotePlaybackRatesNotifier, Map<String, double>>(
      VoiceNotePlaybackRatesNotifier.new,
    );
