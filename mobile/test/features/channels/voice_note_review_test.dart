import 'package:buzz/features/channels/voice_note_recorder_phase.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'voice_note_test_support.dart';

void main() {
  Future<void> noSend(
    String content,
    List<String> mentionPubkeys, {
    List<List<String>> mediaTags = const [],
  }) async {}

  final reviewFinder = find.byKey(const ValueKey('voice-note-recorder-review'));
  final pendingFinder = find.byKey(
    const ValueKey('composer-voice-note-remove'),
  );

  Future<List<FakeVoiceNoteRecorder>> pumpComposer(
    WidgetTester tester, {
    required SharedPreferences prefs,
    int recorders = 1,
  }) async {
    final fakes = [
      for (var index = 0; index < recorders; index++)
        FakeVoiceNoteRecorder(path: '/tmp/take-$index.m4a'),
    ];
    final queue = [...fakes];
    await tester.pumpWidget(
      buildVoiceNoteComposeBar(
        prefs: prefs,
        onSend: noSend,
        voiceNoteRecorderFactory: () => queue.removeAt(0),
        voiceNotePlayerFactory: FakeVoiceNotePlayer.new,
      ),
    );
    await tester.pumpAndSettle();
    return fakes;
  }

  VoiceNoteRecorderPhase phase(WidgetTester tester) =>
      ProviderScope.containerOf(
        tester.element(find.byKey(const ValueKey('compose-bar'))),
      ).read(voiceNoteRecorderPhaseProvider).phase;

  testWidgets('with the setting off, release attaches without a review', (
    tester,
  ) async {
    final prefs = await testPrefs();
    final recorders = await pumpComposer(tester, prefs: prefs);

    final gesture = await holdMic(tester);
    await gesture.up();
    await tester.pumpAndSettle();

    expect(recorders.single.stopped, isTrue);
    expect(reviewFinder, findsNothing);
    expect(pendingFinder, findsOneWidget);
    expect(phase(tester), VoiceNoteRecorderPhase.idle);
  });

  testWidgets('with the setting on, release shows the preview first', (
    tester,
  ) async {
    final prefs = await testPrefs(reviewBeforeSending: true);
    final recorders = await pumpComposer(tester, prefs: prefs);

    final gesture = await holdMic(tester);
    await gesture.up();
    await tester.pumpAndSettle();

    expect(recorders.single.stopped, isTrue);
    expect(reviewFinder, findsOneWidget);
    expect(phase(tester), VoiceNoteRecorderPhase.reviewing);
    expect(
      find.byKey(const ValueKey('voice-note-attachment:/tmp/take-0.m4a')),
      findsOneWidget,
    );
    expect(find.text('Record again'), findsOneWidget);
    expect(find.text('Send'), findsOneWidget);
    expect(pendingFinder, findsNothing);
    expect(find.byKey(const ValueKey('voice-note-play-pause')), findsOneWidget);
  });

  testWidgets('Record again discards the take and starts a locked one', (
    tester,
  ) async {
    final prefs = await testPrefs(reviewBeforeSending: true);
    final recorders = await pumpComposer(tester, prefs: prefs, recorders: 2);

    await tapMic(tester);
    await tester.tap(find.byKey(const ValueKey('voice-note-recorder-send')));
    await tester.pumpAndSettle();
    expect(reviewFinder, findsOneWidget);

    await tester.tap(
      find.byKey(const ValueKey('voice-note-recorder-record-again')),
    );
    await tester.pumpAndSettle();

    expect(recorders.first.disposed, isTrue);
    expect(recorders.last.started, isTrue);
    expect(phase(tester), VoiceNoteRecorderPhase.locked);
    expect(
      find.byKey(const ValueKey('voice-note-recorder-locked')),
      findsOneWidget,
    );
    expect(reviewFinder, findsNothing);
    expect(
      find.byKey(const ValueKey('voice-note-attachment:/tmp/take-0.m4a')),
      findsNothing,
    );

    await tester.tap(find.byKey(const ValueKey('voice-note-recorder-send')));
    await tester.pumpAndSettle();
    expect(recorders.last.stopped, isTrue);
    expect(reviewFinder, findsOneWidget);
    expect(
      find.byKey(const ValueKey('voice-note-attachment:/tmp/take-1.m4a')),
      findsOneWidget,
    );
  });

  testWidgets('Send from the preview appends the attachment', (tester) async {
    final prefs = await testPrefs(reviewBeforeSending: true);
    await pumpComposer(tester, prefs: prefs);

    await tapMic(tester);
    await tester.tap(find.byKey(const ValueKey('voice-note-recorder-send')));
    await tester.pumpAndSettle();
    expect(reviewFinder, findsOneWidget);

    await tester.tap(find.byKey(const ValueKey('voice-note-recorder-send')));
    await tester.pumpAndSettle();

    expect(reviewFinder, findsNothing);
    expect(pendingFinder, findsOneWidget);
    expect(
      find.byKey(const ValueKey('voice-note-attachment:/tmp/take-0.m4a')),
      findsOneWidget,
    );
    expect(phase(tester), VoiceNoteRecorderPhase.idle);
    expect(micFinder, findsNothing);
  });

  testWidgets('discarding from the preview returns to idle', (tester) async {
    final prefs = await testPrefs(reviewBeforeSending: true);
    await pumpComposer(tester, prefs: prefs);

    await tapMic(tester);
    await tester.tap(find.byKey(const ValueKey('voice-note-recorder-send')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const ValueKey('voice-note-recorder-discard')));
    await tester.pumpAndSettle();

    expect(reviewFinder, findsNothing);
    expect(pendingFinder, findsNothing);
    expect(micFinder, findsOneWidget);
    expect(phase(tester), VoiceNoteRecorderPhase.idle);
  });
}
