import 'package:hooks_riverpod/hooks_riverpod.dart';

import '../theme/theme_provider.dart';

/// Shared-preferences key remembering whether transcripts are unfolded.
const voiceNoteTranscriptOpenPrefsKey = 'voice_note_transcript_open';

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

/// Last transcript fold choice on this device, or `null` before any choice
/// so cards can fall back to their channel default.
class VoiceNoteTranscriptOpenNotifier extends Notifier<bool?> {
  @override
  bool? build() =>
      ref.read(savedPrefsProvider).getBool(voiceNoteTranscriptOpenPrefsKey);

  /// Remembers [open] for every voice-note card on this device.
  void set(bool open) {
    state = open;
    ref.read(savedPrefsProvider).setBool(voiceNoteTranscriptOpenPrefsKey, open);
  }
}

/// Provides the remembered transcript fold choice.
final voiceNoteTranscriptOpenProvider =
    NotifierProvider<VoiceNoteTranscriptOpenNotifier, bool?>(
      VoiceNoteTranscriptOpenNotifier.new,
    );
