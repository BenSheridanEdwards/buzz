import 'package:buzz/features/channels/message_content.dart';
import 'package:buzz/features/channels/voice_note_recording.dart';
import 'package:buzz/features/channels/voice_note_waveform.dart';
import 'package:buzz/shared/theme/theme.dart';
import 'package:buzz/shared/voice_notes/voice_note_preferences.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:hooks_riverpod/misc.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'voice_note_test_support.dart';

const _url = 'https://example.com/media/voice-note.mp4';
const _otherUrl = 'https://example.com/media/other-note.mp4';
const _transcript = 'Chief, it is Neo. Status is green on the Studio.';

void main() {
  late SharedPreferences prefs;

  setUp(() async {
    prefs = await testPrefs();
  });

  var scopeSerial = 0;
  Widget scope({
    required Widget child,
    Key? key,
    List<Override> overrides = const [],
  }) => ProviderScope(
    // A fresh container per pump, like a newly opened channel, unless a
    // key pins the container across pumps.
    key: key ?? ValueKey('card-scope-${scopeSerial++}'),
    overrides: [savedPrefsProvider.overrideWithValue(prefs), ...overrides],
    child: MaterialApp(
      theme: AppTheme.light(),
      home: Scaffold(body: child),
    ),
  );

  Widget message({
    String url = _url,
    String? sender = 'Neo',
    String? transcript = _transcript,
    bool transcriptOpenByDefault = false,
    String messageId = 'message-1',
  }) => MessageContent(
    content: '![audio]($url)',
    tags: [
      [
        'imeta',
        'url $url',
        'm audio/mp4',
        'duration 24.0',
        'filename voice-note.m4a',
        if (transcript != null) 'alt $transcript',
      ],
    ],
    voiceNoteSenderName: sender,
    voiceNoteTranscriptOpenByDefault: transcriptOpenByDefault,
    voiceNoteMessageId: messageId,
  );

  Widget card({
    required FakeVoiceNotePlayer player,
    String? sender = 'Neo',
    String? transcript = _transcript,
    bool transcriptOpenByDefault = false,
    String messageId = 'message-1',
  }) => scope(
    overrides: [voiceNotePlayerFactoryProvider.overrideWithValue(() => player)],
    child: message(
      sender: sender,
      transcript: transcript,
      transcriptOpenByDefault: transcriptOpenByDefault,
      messageId: messageId,
    ),
  );

  final toggleFinder = find.byKey(
    const ValueKey('voice-note-transcript-toggle'),
  );
  final bodyFinder = find.byKey(const ValueKey('voice-note-transcript-body'));
  final playFinder = find.byKey(const ValueKey('voice-note-play-pause'));

  Future<void> tapWaveformAt(WidgetTester tester, double fraction) async {
    final rect = tester.getRect(
      find.byKey(const ValueKey('voice-note-waveform')),
    );
    await tester.tapAt(
      Offset(rect.left + rect.width * fraction, rect.center.dy),
    );
    await tester.pump();
  }

  testWidgets(
    'time row reads total at idle and current, sender, total while playing',
    (tester) async {
      final player = FakeVoiceNotePlayer();
      await tester.pumpWidget(card(player: player));
      await tester.pump();

      expect(find.text('0:24 · Voice note'), findsOneWidget);
      expect(find.byKey(const ValueKey('voice-note-sender')), findsNothing);

      await tester.tap(playFinder);
      await tester.pump();
      expect(find.text('0:00 · Neo · 0:24'), findsOneWidget);

      player.setPosition(const Duration(seconds: 16));
      await tester.pump();
      expect(find.text('0:16 · Neo · 0:24'), findsOneWidget);

      await tapWaveformAt(tester, 0.5);
      expect(player.state.position.inMilliseconds, closeTo(12000, 200));
      expect(find.text('0:12 · Neo · 0:24'), findsOneWidget);
    },
  );

  testWidgets('a scrub before the source has loaded is not applied', (
    tester,
  ) async {
    final player = FakeVoiceNotePlayer(loadsLazily: true);
    await tester.pumpWidget(card(player: player));
    await tester.pump();
    expect(player.state.canSeek, isFalse);

    await tapWaveformAt(tester, 0.5);
    expect(player.seeks, isEmpty);
    expect(find.text('0:24 · Voice note'), findsOneWidget);

    // Playing loads the source; the fill starts from the real position, not
    // from the ignored scrub.
    await tester.tap(playFinder);
    await tester.pump();
    expect(player.state.canSeek, isTrue);
    double fill() => tester
        .widget<VoiceNoteWaveform>(find.byType(VoiceNoteWaveform))
        .progress;
    expect(fill(), closeTo(0, 0.01));
    await tester.pump(const Duration(seconds: 6));
    expect(fill(), closeTo(0.25, 0.02));

    await tapWaveformAt(tester, 0.5);
    expect(player.seeks.single.inMilliseconds, closeTo(12000, 200));
    expect(fill(), closeTo(0.5, 0.02));
    await tester.pump(const Duration(seconds: 12));
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

  testWidgets('the speed choice survives the card remounting', (tester) async {
    const scopeKey = ValueKey('pinned-scope');
    final first = FakeVoiceNotePlayer();
    final second = FakeVoiceNotePlayer();
    final players = [first, second];
    final overrides = [
      voiceNotePlayerFactoryProvider.overrideWithValue(
        () => players.removeAt(0),
      ),
    ];
    await tester.pumpWidget(
      scope(key: scopeKey, overrides: overrides, child: message()),
    );
    await tester.pump();
    await tester.tap(find.byKey(const ValueKey('voice-note-playback-rate')));
    await tester.pump();

    await tester.pumpWidget(
      scope(key: scopeKey, overrides: overrides, child: const SizedBox()),
    );
    await tester.pumpWidget(
      scope(key: scopeKey, overrides: overrides, child: message()),
    );
    await tester.pump();

    expect(
      tester
          .widget<Text>(
            find.byKey(const ValueKey('voice-note-playback-rate-value')),
          )
          .data,
      '1.5×',
    );
    expect(players, isEmpty);
    expect(first.speeds, [1.5]);
    // The fresh player was told the remembered rate.
    expect(second.speeds, [1.5]);
  });

  testWidgets('transcript starts folded and unfolds on playback only in DMs', (
    tester,
  ) async {
    final dmPlayer = FakeVoiceNotePlayer();
    await tester.pumpWidget(
      card(player: dmPlayer, transcriptOpenByDefault: true),
    );
    await tester.pump();
    expect(find.text('Show transcript'), findsOneWidget);
    expect(bodyFinder, findsNothing);

    await tester.tap(playFinder);
    await tester.pumpAndSettle();
    expect(find.text('Transcript'), findsOneWidget);
    expect(bodyFinder, findsOneWidget);
    expect(find.text(_transcript), findsOneWidget);

    final channelPlayer = FakeVoiceNotePlayer();
    await tester.pumpWidget(
      card(player: channelPlayer, transcriptOpenByDefault: false),
    );
    await tester.pump();
    await tester.tap(playFinder);
    await tester.pumpAndSettle();
    expect(find.text('Transcript'), findsOneWidget);
    expect(bodyFinder, findsNothing);
  });

  testWidgets('the fold choice is remembered per message, not globally', (
    tester,
  ) async {
    const scopeKey = ValueKey('pinned-scope');
    final overrides = [
      voiceNotePlayerFactoryProvider.overrideWithValue(FakeVoiceNotePlayer.new),
    ];
    Widget both() => Column(
      children: [
        message(url: _url, messageId: 'message-1'),
        message(url: _otherUrl, messageId: 'message-2'),
      ],
    );
    await tester.pumpWidget(
      scope(key: scopeKey, overrides: overrides, child: both()),
    );
    await tester.pump();
    expect(bodyFinder, findsNothing);

    await tester.tap(toggleFinder.first);
    await tester.pumpAndSettle();
    expect(bodyFinder, findsOneWidget);
    expect(prefs.getStringList(voiceNoteTranscriptChoicesPrefsKey), [
      'message-1=1',
    ]);

    // A remount keeps the first open and the second folded.
    await tester.pumpWidget(
      scope(key: scopeKey, overrides: overrides, child: const SizedBox()),
    );
    await tester.pumpWidget(
      scope(key: scopeKey, overrides: overrides, child: both()),
    );
    await tester.pump();
    expect(bodyFinder, findsOneWidget);
    expect(
      find.descendant(
        of: find.byKey(const ValueKey('voice-note-attachment:$_url')),
        matching: bodyFinder,
      ),
      findsOneWidget,
    );

    // Folding a DM card that auto-opened is remembered for that message
    // only; a fresh container (new install) reads the same list back.
    await tester.pumpWidget(
      card(
        player: FakeVoiceNotePlayer(),
        messageId: 'dm-1',
        transcriptOpenByDefault: true,
      ),
    );
    await tester.pump();
    await tester.tap(playFinder);
    await tester.pumpAndSettle();
    expect(bodyFinder, findsOneWidget);
    await tester.tap(toggleFinder);
    await tester.pumpAndSettle();
    expect(bodyFinder, findsNothing);
    expect(prefs.getStringList(voiceNoteTranscriptChoicesPrefsKey), [
      'message-1=1',
      'dm-1=0',
    ]);

    await tester.pumpWidget(
      card(
        player: FakeVoiceNotePlayer(),
        messageId: 'dm-2',
        transcriptOpenByDefault: true,
      ),
    );
    await tester.pump();
    await tester.tap(playFinder);
    await tester.pumpAndSettle();
    expect(bodyFinder, findsOneWidget);
  });

  test('remembered transcript choices stay bounded', () async {
    final container = ProviderContainer(
      overrides: [savedPrefsProvider.overrideWithValue(prefs)],
    );
    addTearDown(container.dispose);
    final notifier = container.read(
      voiceNoteTranscriptChoicesProvider.notifier,
    );
    for (var index = 0; index < voiceNoteTranscriptChoicesLimit + 5; index++) {
      notifier.set('message-$index', open: true);
    }
    final saved = prefs.getStringList(voiceNoteTranscriptChoicesPrefsKey)!;
    expect(saved, hasLength(voiceNoteTranscriptChoicesLimit));
    expect(saved.first, 'message-5=1');
    expect(notifier.choiceFor('message-0'), isNull);
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
    expect(
      find
          .bySemanticsLabel(RegExp('Voice note from Neo, 0:24'))
          .evaluate()
          .length,
      1,
    );
    handle.dispose();
  });
}
