import 'package:buzz/features/channels/message_content.dart';
import 'package:buzz/features/channels/voice_note_recording.dart';
import 'package:buzz/shared/theme/theme.dart';
import 'package:buzz/shared/voice_notes/voice_note_preferences.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'voice_note_test_support.dart';

const _url = 'https://example.com/media/voice-note.mp4';
const _transcript = 'Chief, it is Neo. Status is green on the Studio.';

void main() {
  late SharedPreferences prefs;

  setUp(() async {
    prefs = await testPrefs();
  });

  var scopeSerial = 0;
  Widget card({
    required FakeVoiceNotePlayer player,
    String? sender = 'Neo',
    String? transcript = _transcript,
    bool transcriptOpenByDefault = false,
  }) => ProviderScope(
    // A fresh container per pump, like a newly opened channel.
    key: ValueKey('card-scope-${scopeSerial++}'),
    overrides: [
      savedPrefsProvider.overrideWithValue(prefs),
      voiceNotePlayerFactoryProvider.overrideWithValue(() => player),
    ],
    child: MaterialApp(
      theme: AppTheme.light(),
      home: Scaffold(
        body: MessageContent(
          content: '![audio]($_url)',
          tags: [
            [
              'imeta',
              'url $_url',
              'm audio/mp4',
              'duration 24.0',
              'filename voice-note.m4a',
              if (transcript != null) 'alt $transcript',
            ],
          ],
          voiceNoteSenderName: sender,
          voiceNoteTranscriptOpenByDefault: transcriptOpenByDefault,
        ),
      ),
    ),
  );

  final toggleFinder = find.byKey(
    const ValueKey('voice-note-transcript-toggle'),
  );
  final bodyFinder = find.byKey(const ValueKey('voice-note-transcript-body'));

  testWidgets('titles the card with the sender and shows current / total', (
    tester,
  ) async {
    final player = FakeVoiceNotePlayer();
    await tester.pumpWidget(card(player: player));
    await tester.pump();

    expect(find.byKey(const ValueKey('voice-note-sender')), findsOneWidget);
    expect(find.text('Neo'), findsOneWidget);
    expect(find.text('0:00 / 0:24'), findsOneWidget);

    player.setPosition(const Duration(seconds: 16));
    await tester.pump();
    expect(find.text('0:16 / 0:24'), findsOneWidget);

    final waveformRect = tester.getRect(
      find.byKey(const ValueKey('voice-note-waveform')),
    );
    await tester.tapAt(
      Offset(
        waveformRect.left + waveformRect.width * 0.5,
        waveformRect.center.dy,
      ),
    );
    await tester.pump();
    expect(player.state.position.inMilliseconds, closeTo(12000, 200));
    expect(find.text('0:12 / 0:24'), findsOneWidget);
  });

  testWidgets('speed pill cycles 1x, 1.5x, 2x and back to 1x', (tester) async {
    final player = FakeVoiceNotePlayer();
    await tester.pumpWidget(card(player: player));
    await tester.pump();

    final rate = find.byKey(const ValueKey('voice-note-playback-rate'));
    final value = find.byKey(const ValueKey('voice-note-playback-rate-value'));
    expect(tester.widget<Text>(value).data, '1×');

    for (final expected in ['1.5×', '2×', '1×']) {
      await tester.tap(rate);
      await tester.pump();
      expect(tester.widget<Text>(value).data, expected);
    }
    expect(player.speeds, [1.5, 2, 1]);
    expect(find.text('.5×'), findsNothing);
  });

  testWidgets('transcript starts open in DMs and folded in channels', (
    tester,
  ) async {
    await tester.pumpWidget(
      card(player: FakeVoiceNotePlayer(), transcriptOpenByDefault: true),
    );
    await tester.pump();
    expect(find.text('Transcript'), findsOneWidget);
    expect(bodyFinder, findsOneWidget);
    expect(find.text(_transcript), findsOneWidget);

    await tester.pumpWidget(
      card(player: FakeVoiceNotePlayer(), transcriptOpenByDefault: false),
    );
    await tester.pump();
    expect(find.text('Show transcript'), findsOneWidget);
    expect(bodyFinder, findsNothing);
  });

  testWidgets('the fold choice persists per device across rebuilds', (
    tester,
  ) async {
    await tester.pumpWidget(card(player: FakeVoiceNotePlayer()));
    await tester.pump();
    expect(bodyFinder, findsNothing);

    await tester.tap(toggleFinder);
    await tester.pumpAndSettle();
    expect(bodyFinder, findsOneWidget);
    expect(prefs.getBool(voiceNoteTranscriptOpenPrefsKey), isTrue);

    // A fresh tree with the channel default still honours the remembered
    // choice, and a DM card folds once the user has closed one elsewhere.
    await tester.pumpWidget(card(player: FakeVoiceNotePlayer()));
    await tester.pump();
    expect(bodyFinder, findsOneWidget);

    await tester.tap(toggleFinder);
    await tester.pumpAndSettle();
    expect(prefs.getBool(voiceNoteTranscriptOpenPrefsKey), isFalse);
    await tester.pumpWidget(
      card(player: FakeVoiceNotePlayer(), transcriptOpenByDefault: true),
    );
    await tester.pump();
    expect(bodyFinder, findsNothing);
  });

  testWidgets('no transcript row when the imeta carries no alt text', (
    tester,
  ) async {
    await tester.pumpWidget(
      card(player: FakeVoiceNotePlayer(), transcript: null),
    );
    await tester.pump();
    expect(toggleFinder, findsNothing);
    expect(find.text('Show transcript'), findsNothing);
    expect(
      find.byKey(const ValueKey('voice-note-attachment:$_url')),
      findsOneWidget,
    );
  });

  testWidgets('each control owns exactly one semantics label', (tester) async {
    final handle = tester.ensureSemantics();
    await tester.pumpWidget(card(player: FakeVoiceNotePlayer()));
    await tester.pump();

    expect(semanticsNodeCount(tester, 'Play voice note'), 1);
    expect(semanticsNodeCount(tester, 'Playback speed 1×'), 1);
    expect(semanticsNodeCount(tester, 'Transcript'), 1);
    expect(
      find.bySemanticsLabel(RegExp('Voice note waveform')).evaluate().length,
      1,
    );
    expect(find.bySemanticsLabel(RegExp('0:00 of 0:24')).evaluate().length, 1);
    handle.dispose();
  });
}
