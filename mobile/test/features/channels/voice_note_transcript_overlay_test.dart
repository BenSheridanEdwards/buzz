import 'package:flutter_test/flutter_test.dart';
import 'package:buzz/features/channels/voice_note_transcript_overlay.dart';
import 'package:buzz/features/channels/timeline_message.dart';
import 'package:buzz/shared/relay/nostr_models.dart';

NostrEvent _transcript(String id, String target, String content, int createdAt) {
  return NostrEvent(
    id: id,
    pubkey: 'b' * 64,
    kind: EventKind.voiceNoteTranscript,
    createdAt: createdAt,
    content: content,
    tags: [
      ['e', target],
    ],
    sig: 'sig',
  );
}

void main() {
  group('indexVoiceNoteTranscripts', () {
    test('maps a transcript onto the note it references', () {
      final map = indexVoiceNoteTranscripts([
        _transcript('t1', 'note1', 'Yes Chief, it came through.', 100),
      ]);
      expect(map['note1'], 'Yes Chief, it came through.');
    });

    test('first writer wins, so a later event cannot rewrite what was said',
        () {
      final map = indexVoiceNoteTranscripts([
        _transcript('t2', 'note1', 'spoofed later', 200),
        _transcript('t1', 'note1', 'the real words', 100),
      ]);
      expect(map['note1'], 'the real words');
    });

    test('ignores an empty transcript rather than mapping a blank', () {
      final map = indexVoiceNoteTranscripts([
        _transcript('t1', 'note1', '   ', 100),
      ]);
      expect(map.containsKey('note1'), isFalse);
    });

    test('ignores a transcript for a deleted note', () {
      final map = indexVoiceNoteTranscripts(
        [_transcript('t1', 'note1', 'words', 100)],
        deletedEventIds: {'note1'},
      );
      expect(map.containsKey('note1'), isFalse);
    });

    test('the kind agrees with the shared constant', () {
      expect(EventKind.voiceNoteTranscript, 40009);
      expect(EventKind.channelAuxEventKinds, contains(40009));
      expect(EventKind.channelTimelineContentKinds, isNot(contains(40009)));
    });
  });

  group('sanitizePublishedTranscript', () {
    test('strips control characters and bidi overrides', () {
      final out = sanitizePublishedTranscript('safe\u202ereversed\u202c\u0007 here');
      expect(out, isNotNull);
      expect(out!.contains('\u202e'), isFalse);
      expect(out.contains('\u0007'), isFalse);
      expect(out.contains('safe'), isTrue);
    });

    test('re-caps length rather than trusting the publisher', () {
      final out = sanitizePublishedTranscript('a' * 5000);
      expect(out!.length, lessThanOrEqualTo(1000));
    });

    test('whitespace-only and null yield no transcript', () {
      expect(sanitizePublishedTranscript('   '), isNull);
      expect(sanitizePublishedTranscript(null), isNull);
    });

    test('ordinary prose is untouched', () {
      const prose = 'Yes Chief, it came through. Can you hear me?';
      expect(sanitizePublishedTranscript(prose), prose);
    });
  });

  group('applyTranscriptToTags', () {
    test('writes alt onto an audio imeta that lacks one', () {
      final out = applyTranscriptToTags([
        ['imeta', 'url https://b/v.mp3', 'm audio/mpeg', 'duration 3'],
      ], 'the words');
      expect(out.first, contains('alt the words'));
    });

    test('never replaces an alt the author already supplied', () {
      final out = applyTranscriptToTags([
        ['imeta', 'url https://b/v.mp3', 'm audio/mpeg', "alt author's own"],
      ], 'published later');
      expect(out.first, contains("alt author's own"));
      expect(out.first, isNot(contains('alt published later')));
    });

    test('leaves a non-audio imeta untouched', () {
      final out = applyTranscriptToTags([
        ['imeta', 'url https://b/p.png', 'm image/png'],
      ], 'the words');
      expect(out.first.any((f) => f.startsWith('alt ')), isFalse);
    });

    test('with no transcript returns the same list', () {
      final tags = [
        ['imeta', 'url https://b/v.mp3', 'm audio/mpeg'],
      ];
      expect(identical(applyTranscriptToTags(tags, null), tags), isTrue);
    });
  });

  group('formatTimeline', () {
    NostrEvent voiceNote({List<List<String>>? tags}) => NostrEvent(
          id: 'a' * 64,
          pubkey: 'c' * 64,
          kind: EventKind.streamMessage,
          createdAt: 1700000000,
          content: '',
          tags: tags ??
              [
                ['h', 'chan'],
                ['imeta', 'url https://b/v.mp3', 'm audio/mpeg', 'duration 3'],
              ],
          sig: 'sig',
        );

    test('a published transcript folds onto the voice note imeta alt', () {
      final out = formatTimeline([
        voiceNote(),
        _transcript('b' * 64, 'a' * 64, 'Yes Chief, it came through.', 1700000100),
      ]);
      expect(out.length, 1, reason: 'the transcript must not render its own row');
      final imeta = out.single.tags.firstWhere((t) => t.first == 'imeta');
      expect(imeta, contains('alt Yes Chief, it came through.'));
    });

    test('a transcript never overwrites an alt the author supplied', () {
      final out = formatTimeline([
        voiceNote(tags: [
          ['h', 'chan'],
          ['imeta', 'url https://b/v.mp3', 'm audio/mpeg', "alt author's own"],
        ]),
        _transcript('b' * 64, 'a' * 64, 'published later', 1700000100),
      ]);
      final imeta = out.single.tags.firstWhere((t) => t.first == 'imeta');
      expect(imeta, contains("alt author's own"));
      expect(imeta, isNot(contains('alt published later')));
    });
  });
}
