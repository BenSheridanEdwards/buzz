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
    bool ownNote = false,
    String? playbackSpeed,
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
        if (playbackSpeed != null) 'playback_speed $playbackSpeed',
      ],
    ],
    voiceNoteSenderName: sender,
    voiceNoteIsOwn: ownNote,
    voiceNoteMessageId: messageId,
  );

  Widget card({
    required FakeVoiceNotePlayer player,
    String? sender = 'Neo',
    String? transcript = _transcript,
    bool ownNote = false,
    String? playbackSpeed,
    String messageId = 'message-1',
  }) => scope(
    overrides: [voiceNotePlayerFactoryProvider.overrideWithValue(() => player)],
    child: message(
      sender: sender,
      transcript: transcript,
      ownNote: ownNote,
      playbackSpeed: playbackSpeed,
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

  testWidgets('a received note steps by a tenth and wraps to 1x', (
    tester,
  ) async {
    final player = FakeVoiceNotePlayer();
    await tester.pumpWidget(card(player: player));
    await tester.pump();

    final rate = find.byKey(const ValueKey('voice-note-playback-rate'));
    final value = find.byKey(const ValueKey('voice-note-playback-rate-value'));
    expect(tester.widget<Text>(value).data, '1×');

    final expected = [
      for (var tenths = 11; tenths <= 20; tenths++) tenths / 10,
      1.0,
    ];
    for (final next in expected) {
      await tester.tap(rate);
      await tester.pump();
      expect(
        tester.widget<Text>(value).data,
        formatVoiceNotePlaybackRate(next),
      );
    }
    expect(player.speeds, expected);
    expect(find.text('1.2000000000000002×'), findsNothing);
  });

  testWidgets('your own note steps by a quarter to 2x and back to 1x', (
    tester,
  ) async {
    final player = FakeVoiceNotePlayer();
    await tester.pumpWidget(card(player: player, ownNote: true));
    await tester.pump();

    final rate = find.byKey(const ValueKey('voice-note-playback-rate'));
    final value = find.byKey(const ValueKey('voice-note-playback-rate-value'));
    expect(tester.widget<Text>(value).data, '1×');

    for (final expected in ['1.25×', '1.5×', '1.75×', '2×', '1×']) {
      await tester.tap(rate);
      await tester.pump();
      expect(tester.widget<Text>(value).data, expected);
    }
    expect(player.speeds, [1.25, 1.5, 1.75, 2, 1]);
  });

  testWidgets("a voice's hinted speed is where its note starts and wraps", (
    tester,
  ) async {
    final player = FakeVoiceNotePlayer();
    await tester.pumpWidget(card(player: player, playbackSpeed: '1.1'));
    await tester.pump();

    final value = find.byKey(const ValueKey('voice-note-playback-rate-value'));
    expect(tester.widget<Text>(value).data, '1.1×');
    // The player itself was told the hinted rate before any tap.
    expect(player.speeds, [1.1]);

    final rate = find.byKey(const ValueKey('voice-note-playback-rate'));
    for (var taps = 0; taps < 9; taps++) {
      await tester.tap(rate);
      await tester.pump();
    }
    expect(tester.widget<Text>(value).data, '2×');
    await tester.tap(rate);
    await tester.pump();
    expect(tester.widget<Text>(value).data, '1.1×');
  });

  testWidgets('a hint outside the playable range is ignored', (tester) async {
    for (final hint in ['4', '0.1', 'fast', 'NaN']) {
      final player = FakeVoiceNotePlayer();
      await tester.pumpWidget(card(player: player, playbackSpeed: hint));
      await tester.pump();
      expect(
        tester
            .widget<Text>(
              find.byKey(const ValueKey('voice-note-playback-rate-value')),
            )
            .data,
        '1×',
        reason: 'hint $hint',
      );
      expect(player.speeds, isEmpty, reason: 'hint $hint');
    }
    // Your own note ignores even a sane hint: it always starts at 1x.
    final own = FakeVoiceNotePlayer();
    await tester.pumpWidget(
      card(player: own, ownNote: true, playbackSpeed: '1.5'),
    );
    await tester.pump();
    expect(own.speeds, isEmpty);
  });

  test('the rate table is the same on both clients', () {
    expect(voiceNoteMaxPlaybackRate, 2);
    expect(voiceNoteMinPlaybackRate, 0.5);
    expect(ownVoiceNoteRateStep, 0.25);
    expect(receivedVoiceNoteRateStep, 0.1);
    expect(nextVoiceNotePlaybackRate(1.9, ownNote: false, defaultRate: 1), 2);
    expect(nextVoiceNotePlaybackRate(2, ownNote: false, defaultRate: 1.1), 1.1);
    expect(
      nextVoiceNotePlaybackRate(0.8, ownNote: false, defaultRate: 0.8),
      0.9,
    );
    expect(
      nextVoiceNotePlaybackRate(double.nan, ownNote: true, defaultRate: 1),
      1,
    );
    expect(nextVoiceNotePlaybackRate(0.25, ownNote: true, defaultRate: 1), 1);
    expect(voiceNoteDefaultPlaybackRate(1.1000000001, ownNote: false), 1.1);
    expect(formatVoiceNotePlaybackRate(1.1 + 0.1), '1.2×');
    expect(formatVoiceNotePlaybackRate(1.25), '1.25×');
    expect(formatVoiceNotePlaybackRate(2), '2×');
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
      '1.1×',
    );
    expect(players, isEmpty);
    expect(first.speeds, [1.1]);
    // The fresh player was told the remembered rate.
    expect(second.speeds, [1.1]);
  });

  testWidgets('transcript starts folded everywhere, even in a DM', (
    tester,
  ) async {
    final player = FakeVoiceNotePlayer();
    await tester.pumpWidget(card(player: player));
    await tester.pump();
    expect(find.text('Show transcript'), findsOneWidget);
    expect(bodyFinder, findsNothing);

    // Playing does not unfold it; the words wait to be asked for.
    await tester.tap(playFinder);
    await tester.pumpAndSettle();
    expect(find.text('Transcript'), findsOneWidget);
    expect(bodyFinder, findsNothing);

    await tester.tap(toggleFinder);
    await tester.pumpAndSettle();
    expect(bodyFinder, findsOneWidget);
    expect(find.text(_transcript), findsOneWidget);
  });

  testWidgets('opening one transcript keeps the next ones open', (
    tester,
  ) async {
    await tester.pumpWidget(
      card(player: FakeVoiceNotePlayer(), messageId: 'message-1'),
    );
    await tester.pump();
    expect(bodyFinder, findsNothing);
    await tester.tap(toggleFinder);
    await tester.pumpAndSettle();
    expect(bodyFinder, findsOneWidget);
    expect(prefs.getBool(voiceNoteTranscriptLastChoicePrefsKey), isTrue);

    // A fresh container (a newly opened channel, or the next launch) starts
    // the next card open.
    await tester.pumpWidget(
      card(player: FakeVoiceNotePlayer(), messageId: 'message-2'),
    );
    await tester.pump();
    expect(bodyFinder, findsOneWidget);

    // Folding it folds the ones after it too.
    await tester.tap(toggleFinder);
    await tester.pumpAndSettle();
    expect(bodyFinder, findsNothing);
    expect(prefs.getBool(voiceNoteTranscriptLastChoicePrefsKey), isFalse);
    await tester.pumpWidget(
      card(player: FakeVoiceNotePlayer(), messageId: 'message-3'),
    );
    await tester.pump();
    expect(bodyFinder, findsNothing);
  });

  testWidgets("a message's own fold choice wins over the sticky default", (
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
    final firstBody = find.descendant(
      of: find.byKey(const ValueKey('voice-note-attachment:$_url')),
      matching: bodyFinder,
    );
    final secondBody = find.descendant(
      of: find.byKey(const ValueKey('voice-note-attachment:$_otherUrl')),
      matching: bodyFinder,
    );
    await tester.pumpWidget(
      scope(key: scopeKey, overrides: overrides, child: both()),
    );
    await tester.pump();
    expect(bodyFinder, findsNothing);

    // Opening the first opens the second too: that is the sticky default.
    await tester.tap(toggleFinder.first);
    await tester.pumpAndSettle();
    expect(firstBody, findsOneWidget);
    expect(secondBody, findsOneWidget);
    expect(prefs.getStringList(voiceNoteTranscriptChoicesPrefsKey), [
      'message-1=1',
    ]);

    // Folding the second is remembered for it alone; the first keeps its
    // own choice, and the sticky default is now folded.
    await tester.tap(toggleFinder.last);
    await tester.pumpAndSettle();
    expect(firstBody, findsOneWidget);
    expect(secondBody, findsNothing);
    expect(prefs.getStringList(voiceNoteTranscriptChoicesPrefsKey), [
      'message-1=1',
      'message-2=0',
    ]);
    expect(prefs.getBool(voiceNoteTranscriptLastChoicePrefsKey), isFalse);

    // A remount reads the same choices back: first open, second folded.
    await tester.pumpWidget(
      scope(key: scopeKey, overrides: overrides, child: const SizedBox()),
    );
    await tester.pumpWidget(
      scope(key: scopeKey, overrides: overrides, child: both()),
    );
    await tester.pump();
    expect(firstBody, findsOneWidget);
    expect(secondBody, findsNothing);

    // A third card with no choice of its own follows the sticky default.
    await tester.pumpWidget(
      card(player: FakeVoiceNotePlayer(), messageId: 'message-3'),
    );
    await tester.pump();
    expect(bodyFinder, findsNothing);
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

  test('remembered playback rates stay bounded', () async {
    final container = ProviderContainer(
      overrides: [savedPrefsProvider.overrideWithValue(prefs)],
    );
    addTearDown(container.dispose);
    final notifier = container.read(voiceNotePlaybackRatesProvider.notifier);
    for (var index = 0; index < voiceNotePlaybackRatesLimit + 5; index++) {
      notifier.set('https://example.com/note-$index.mp4', 1.5);
    }
    final rates = container.read(voiceNotePlaybackRatesProvider);
    expect(rates, hasLength(voiceNotePlaybackRatesLimit));
    expect(rates['https://example.com/note-0.mp4'], isNull);
    expect(rates['https://example.com/note-5.mp4'], 1.5);
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
