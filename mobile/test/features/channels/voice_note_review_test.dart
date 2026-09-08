import 'dart:async';

import 'package:buzz/features/channels/voice_note_recorder_phase.dart';
import 'package:buzz/shared/relay/relay.dart';
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
    List<FakeVoiceNoteRecorder>? fakes,
    AppLifecycleNotifier Function()? appLifecycle,
  }) async {
    final resolved =
        fakes ??
        [
          for (var index = 0; index < recorders; index++)
            FakeVoiceNoteRecorder(path: '/tmp/take-$index.m4a'),
        ];
    final queue = [...resolved];
    await tester.pumpWidget(
      buildVoiceNoteComposeBar(
        prefs: prefs,
        onSend: noSend,
        voiceNoteRecorderFactory: () => queue.removeAt(0),
        voiceNotePlayerFactory: FakeVoiceNotePlayer.new,
        appLifecycle: appLifecycle,
      ),
    );
    await tester.pumpAndSettle();
    return resolved;
  }

  /// Records a take hands free and sends it into the review step.
  Future<void> reviewTake(WidgetTester tester) async {
    await tapMic(tester);
    await tester.tap(find.byKey(const ValueKey('voice-note-recorder-send')));
    await tester.pumpAndSettle();
    expect(reviewFinder, findsOneWidget);
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
    // The compact Preview row: time, "Tap to review", and an X.
    expect(find.text('0:03 · Tap to review'), findsOneWidget);
    expect(
      find.byKey(const ValueKey('voice-note-review-dismiss')),
      findsOneWidget,
    );
    expect(
      tester.getSize(find.byKey(const ValueKey('voice-note-recorder-discard'))),
      const Size(52, 52),
    );
  });

  testWidgets('tapping the review label plays the take', (tester) async {
    final prefs = await testPrefs(reviewBeforeSending: true);
    await pumpComposer(tester, prefs: prefs);
    await reviewTake(tester);

    await tester.tap(find.byKey(const ValueKey('voice-note-idle-label-tap')));
    await tester.pump();
    expect(find.byTooltip('Pause voice note'), findsOneWidget);
    expect(find.text('0:03 · Tap to review'), findsNothing);
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

  testWidgets('the review survives backgrounding and a pushed route', (
    tester,
  ) async {
    final prefs = await testPrefs(reviewBeforeSending: true);
    final lifecycle = FakeAppLifecycleNotifier();
    final recorders = await pumpComposer(
      tester,
      prefs: prefs,
      appLifecycle: () => lifecycle,
    );
    await reviewTake(tester);

    lifecycle.setLifecycle(AppLifecycleState.paused);
    await tester.pumpAndSettle();
    lifecycle.setLifecycle(AppLifecycleState.resumed);
    await tester.pumpAndSettle();
    expect(reviewFinder, findsOneWidget);
    expect(phase(tester), VoiceNoteRecorderPhase.reviewing);

    final context = tester.element(reviewFinder);
    unawaited(
      Navigator.of(
        context,
      ).push(MaterialPageRoute<void>(builder: (_) => const Scaffold())),
    );
    await tester.pumpAndSettle();
    Navigator.of(context).pop();
    await tester.pumpAndSettle();
    expect(reviewFinder, findsOneWidget);
    expect(phase(tester), VoiceNoteRecorderPhase.reviewing);
    expect(recorders.single.cancelled, isFalse);

    await tester.tap(find.byKey(const ValueKey('voice-note-recorder-send')));
    await tester.pumpAndSettle();
    expect(pendingFinder, findsOneWidget);
  });

  testWidgets('a community switch during review deletes the reviewed file', (
    tester,
  ) async {
    final prefs = await testPrefs(reviewBeforeSending: true);
    final file = await createRecordingFile(tester, 'reviewed.m4a');
    final recorder = FakeVoiceNoteRecorder(path: file.path);
    await pumpComposer(tester, prefs: prefs, fakes: [recorder]);
    await reviewTake(tester);
    expect(await recordingFileExists(tester, file), isTrue);

    switchCommunity(tester);
    await tester.pumpAndSettle();

    expect(reviewFinder, findsNothing);
    expect(await recordingFileExists(tester, file), isFalse);
    expect(phase(tester), VoiceNoteRecorderPhase.idle);
    expect(micFinder, findsOneWidget);
  });

  testWidgets('the X, the trash, and Record again each delete the take', (
    tester,
  ) async {
    final prefs = await testPrefs(reviewBeforeSending: true);
    final files = [
      for (final name in ['x.m4a', 'trash.m4a', 'again.m4a', 'next.m4a'])
        await createRecordingFile(tester, name),
    ];
    await pumpComposer(
      tester,
      prefs: prefs,
      fakes: [for (final file in files) FakeVoiceNoteRecorder(path: file.path)],
    );

    await reviewTake(tester);
    await tester.tap(find.byKey(const ValueKey('voice-note-review-dismiss')));
    await tester.pumpAndSettle();
    expect(await recordingFileExists(tester, files[0]), isFalse);
    expect(phase(tester), VoiceNoteRecorderPhase.idle);

    await reviewTake(tester);
    await tester.tap(find.byKey(const ValueKey('voice-note-recorder-discard')));
    await tester.pumpAndSettle();
    expect(await recordingFileExists(tester, files[1]), isFalse);
    expect(phase(tester), VoiceNoteRecorderPhase.idle);

    await reviewTake(tester);
    await tester.tap(
      find.byKey(const ValueKey('voice-note-recorder-record-again')),
    );
    await tester.pumpAndSettle();
    expect(await recordingFileExists(tester, files[2]), isFalse);
    expect(await recordingFileExists(tester, files[3]), isTrue);
    expect(phase(tester), VoiceNoteRecorderPhase.locked);
  });

  testWidgets('the review panel owns each actionable label once', (
    tester,
  ) async {
    final handle = tester.ensureSemantics();
    final prefs = await testPrefs(reviewBeforeSending: true);
    await pumpComposer(tester, prefs: prefs);
    await reviewTake(tester);

    expect(semanticsNodeCount(tester, 'Play voice note'), 1);
    expect(semanticsNodeCount(tester, 'Cancel recording'), 1);
    expect(semanticsNodeCount(tester, 'Discard voice note'), 1);
    expect(semanticsNodeCount(tester, 'Record again'), 1);
    expect(semanticsNodeCount(tester, 'Send'), 1);
    expect(semanticsNodeCount(tester, 'Send voice note'), 0);
    expect(semanticsNodeCount(tester, 'Record voice note'), 0);
    expect(
      find.bySemanticsLabel(RegExp('Voice note waveform')).evaluate().length,
      1,
    );
    handle.dispose();
  });
}
