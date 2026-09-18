/// Fold a published voice-note transcript onto the note it describes.
///
/// A voice note only carries a transcript when its author put one in the
/// imeta `alt` field. An agent does that for its own replies as it publishes.
/// A person recording in the composer cannot: there is no speech-to-text on
/// the device, and the credential that would do it lives with the agents.
/// Nothing can add the tag afterwards either, because the author signed the
/// event.
///
/// So the agent that transcribed the clip publishes the words as their own
/// event (`EventKind.voiceNoteTranscript`) tagged `e` with the note's id, and
/// this folds them onto the note's imeta `alt`. Overlaying there rather than
/// plumbing a second transcript source through the UI means the card and its
/// transcript row keep working untouched. Mirrors the desktop's
/// `voiceNoteTranscriptOverlay.mjs`.
library;

import 'package:buzz/shared/relay/nostr_models.dart';

/// Hard ceiling on a transcript the client will show, in characters.
///
/// The publisher caps its own output, but a publisher is exactly who this
/// defends against, so the cap is re-applied on the way in.
const int maxTranscriptChars = 1000;

/// C0 and C1 controls, plus the bidirectional marks and overrides that can
/// visually reverse a sentence even in a plain text widget.
final RegExp _controlOrBidi = RegExp(r'[\u0000-\u001f\u007f-\u009f\u200e\u200f\u202a-\u202e\u2066-\u2069]');

/// Make a third-party transcript safe to show on the author's card.
///
/// The transcript row renders plain text, so markdown cannot fire here and
/// nothing is escaped. Control characters and bidi overrides are stripped,
/// whitespace collapses to one line, and the length is re-capped. Returns
/// `null` for whitespace-only input so the row is hidden rather than blank.
String? sanitizePublishedTranscript(String? text) {
  if (text == null) return null;
  final flat = text
      .replaceAll(_controlOrBidi, ' ')
      .split(RegExp(r'\s+'))
      .where((w) => w.isNotEmpty)
      .join(' ');
  if (flat.isEmpty) return null;
  if (flat.length <= maxTranscriptChars) return flat;
  return flat.substring(0, maxTranscriptChars).trimRight();
}

String? _targetIdOf(List<List<String>> tags) {
  for (final tag in tags) {
    if (tag.length >= 2 && tag[0] == 'e' && tag[1].isNotEmpty) return tag[1];
  }
  return null;
}

/// Index voice-note transcripts by the message they describe.
///
/// **First writer wins.** Any relay member can publish one of these, so
/// keeping the latest would let a later event silently rewrite what someone
/// is shown to have said. Keeping the earliest means a spoof cannot overwrite
/// the genuine transcript, only lose a race to it.
Map<String, String> indexVoiceNoteTranscripts(
  Iterable<NostrEvent> events, {
  Set<String> deletedEventIds = const {},
}) {
  final byTarget = <String, ({String text, int createdAt})>{};
  for (final event in events) {
    if (event.kind != EventKind.voiceNoteTranscript) continue;
    if (deletedEventIds.contains(event.id)) continue;
    final text = sanitizePublishedTranscript(event.content);
    if (text == null) continue;
    final targetId = _targetIdOf(event.tags);
    if (targetId == null || deletedEventIds.contains(targetId)) continue;
    final existing = byTarget[targetId];
    if (existing == null || event.createdAt < existing.createdAt) {
      byTarget[targetId] = (text: text, createdAt: event.createdAt);
    }
  }
  return {for (final e in byTarget.entries) e.key: e.value.text};
}

/// Write `transcript` into the message's audio imeta tag as `alt`.
///
/// An imeta that already carries `alt` is left alone: the author's own
/// transcript is authoritative and must never be replaced by a published one.
/// Returns the same list when nothing changed.
List<List<String>> applyTranscriptToTags(
  List<List<String>> tags,
  String? transcript,
) {
  if (transcript == null || transcript.isEmpty) return tags;
  var changed = false;
  final next = tags.map((tag) {
    if (tag.isEmpty || tag[0] != 'imeta') return tag;
    final isAudio = tag.any(
      (f) => f.startsWith('m ') && f.substring(2).startsWith('audio/'),
    );
    if (!isAudio) return tag;
    if (tag.any((f) => f.startsWith('alt '))) return tag;
    changed = true;
    return [...tag, 'alt $transcript'];
  }).toList();
  return changed ? next : tags;
}
