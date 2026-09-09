import 'dart:async';

import 'package:buzz/features/channels/voice_note_recorder_phase.dart';
import 'package:buzz/features/channels/voice_note_recording.dart';
import 'package:buzz/shared/relay/relay.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'voice_note_test_support.dart';

/// Finishing is the window between Send (or release) and the recorder's
/// stop result. These tests pin what happens when something else moves the
/// composer inside that window (rule 2) and what a release that captured
/// nothing usable does (finding 3).
void main() {
  late SharedPreferences prefs;

  setUp(() async {
    prefs = await testPrefs();
  });

  Future<void> noSend(
    String content,
    List<String> mentionPubkeys, {
    List<List<String>> mediaTags = const [],
  }) async {}

  Future<FakeVoiceNoteRecorder> pumpComposer(
    WidgetTester tester, {
    FakeVoiceNoteRecorder? recorder,
    AppLifecycleNotifier Function()? appLifecycle,
  }) async {
    final fake = recorder ?? FakeVoiceNoteRecorder();
    await tester.pumpWidget(
      buildVoiceNoteComposeBar(
        prefs: prefs,
        onSend: noSend,
        voiceNoteRecorderFactory: () => fake,
        voiceNotePlayerFactory: FakeVoiceNotePlayer.new,
        appLifecycle: appLifecycle,
      ),
    );
    await tester.pumpAndSettle();
    return fake;
  }

  ProviderContainer container(WidgetTester tester) => ProviderScope.containerOf(
    tester.element(find.byKey(const ValueKey('compose-bar'))),
  );

  VoiceNoteRecorderState phase(WidgetTester tester) =>
      container(tester).read(voiceNoteRecorderPhaseProvider);

  Finder attachmentFor(String path) =>
      find.byKey(ValueKey('voice-note-attachment:$path'));

  Finder finishingSpinner() => find.bySemanticsLabel('Finishing voice note');

  testWidgets('Escape during finishing is ignored and the note still lands', (
    tester,
  ) async {
    final recorder = FakeVoiceNoteRecorder()..pendingStop = Completer<void>();
    await pumpComposer(tester, recorder: recorder);

    final gesture = await holdMic(tester);
    await gesture.up();
    await tester.pump();
    expect(phase(tester).phase, VoiceNoteRecorderPhase.finishing);
    expect(recorder.stopped, isTrue);

    await tester.sendKeyEvent(LogicalKeyboardKey.escape);
    await tester.pump();
    expect(phase(tester).phase, VoiceNoteRecorderPhase.finishing);
    expect(recorderFinder, findsOneWidget);
    expect(recorder.cancelled, isFalse);

    recorder.pendingStop!.complete();
    await tester.pumpAndSettle();
    expect(attachmentFor(recorder.path), findsOneWidget);
    expect(phase(tester).phase, VoiceNoteRecorderPhase.idle);
  });

  testWidgets(
    'a community switch during finishing drops the stop result and its file',
    (tester) async {
      final file = await createRecordingFile(tester, 'switched.m4a');
      final recorder = FakeVoiceNoteRecorder(path: file.path)
        ..pendingStop = Completer<void>();
      await pumpComposer(tester, recorder: recorder);

      final gesture = await holdMic(tester);
      await gesture.up();
      await tester.pump();
      expect(phase(tester).phase, VoiceNoteRecorderPhase.finishing);

      // Switch community while the recorder is still fading out, so the
      // late stop result arrives with the widget mounted but the phase
      // reset for the new draft.
      switchCommunity(tester);
      await tester.pump();
      await tester.pump();
      expect(phase(tester).phase, VoiceNoteRecorderPhase.idle);
      expect(recorderFinder, findsOneWidget);

      recorder.pendingStop!.complete();
      await tester.pumpAndSettle();

      expect(attachmentFor(file.path), findsNothing);
      expect(await recordingFileExists(tester, file), isFalse);
      expect(micFinder, findsOneWidget);
      expect(phase(tester).phase, VoiceNoteRecorderPhase.idle);
    },
  );

  testWidgets('backgrounding during finishing lets the sent note complete', (
    tester,
  ) async {
    final lifecycle = FakeAppLifecycleNotifier();
    final recorder = FakeVoiceNoteRecorder()..pendingStop = Completer<void>();
    await pumpComposer(
      tester,
      recorder: recorder,
      appLifecycle: () => lifecycle,
    );

    final gesture = await holdMic(tester);
    await gesture.up();
    await tester.pump();
    expect(phase(tester).phase, VoiceNoteRecorderPhase.finishing);

    lifecycle.setLifecycle(AppLifecycleState.paused);
    await tester.pump();
    expect(recorder.cancelled, isFalse);
    expect(phase(tester).phase, VoiceNoteRecorderPhase.finishing);

    recorder.pendingStop!.complete();
    await tester.pumpAndSettle();
    expect(attachmentFor(recorder.path), findsOneWidget);
    expect(phase(tester).phase, VoiceNoteRecorderPhase.idle);
  });

  testWidgets('a release before start waits for start, then stops', (
    tester,
  ) async {
    final recorder = FakeVoiceNoteRecorder()..pendingStart = Completer<void>();
    await pumpComposer(tester, recorder: recorder);

    final gesture = await holdMic(tester);
    await gesture.up();
    await tester.pump();
    expect(phase(tester).phase, VoiceNoteRecorderPhase.finishing);
    expect(recorder.started, isFalse);
    expect(recorder.stopped, isFalse);
    expect(finishingSpinner(), findsOneWidget);

    recorder.pendingStart!.complete();
    await tester.pumpAndSettle();

    expect(recorder.started, isTrue);
    expect(recorder.stopped, isTrue);
    expect(attachmentFor(recorder.path), findsOneWidget);
    expect(phase(tester).phase, VoiceNoteRecorderPhase.idle);
  });

  testWidgets('a release before a failed start shows the start error only', (
    tester,
  ) async {
    final recorder = FakeVoiceNoteRecorder()
      ..pendingStart = Completer<void>()
      ..startError = StateError(
        'Microphone access is required to record a voice note.',
      );
    await pumpComposer(tester, recorder: recorder);

    final gesture = await holdMic(tester);
    await gesture.up();
    await tester.pump();
    recorder.pendingStart!.complete();
    await tester.pumpAndSettle();

    expect(recorder.stopped, isFalse);
    expect(
      find.text('Microphone access is required to record a voice note.'),
      findsOneWidget,
    );
    expect(find.text(voiceNoteHoldToRecordHint), findsNothing);
    expect(phase(tester).phase, VoiceNoteRecorderPhase.idle);
  });

  testWidgets('a capture under one second is discarded with the hint', (
    tester,
  ) async {
    final file = await createRecordingFile(tester, 'short.m4a');
    final recorder = FakeVoiceNoteRecorder(
      path: file.path,
      recordedDuration: const Duration(milliseconds: 400),
    );
    await pumpComposer(tester, recorder: recorder);

    final gesture = await holdMic(tester);
    await gesture.up();
    await tester.pumpAndSettle();

    expect(recorder.stopped, isTrue);
    expect(attachmentFor(file.path), findsNothing);
    expect(await recordingFileExists(tester, file), isFalse);
    expect(find.text(voiceNoteHoldToRecordHint), findsOneWidget);
    expect(micFinder, findsOneWidget);
    expect(phase(tester).phase, VoiceNoteRecorderPhase.idle);
  });

  testWidgets('a press over the minimum keeps a capture the slow start cut '
      'short', (tester) async {
    // The mic opens late (permission, temp dir, native start), so the audio
    // is shorter than the press. The press is what the user can judge.
    final recorder = FakeVoiceNoteRecorder(
      recordedDuration: const Duration(milliseconds: 800),
    )..pendingStart = Completer<void>();
    await pumpComposer(tester, recorder: recorder);

    final gesture = await holdMic(tester);
    await tester.pump(const Duration(milliseconds: 400));
    recorder.pendingStart!.complete();
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 800));
    await gesture.up();
    await tester.pumpAndSettle();

    expect(attachmentFor(recorder.path), findsOneWidget);
    expect(find.text(voiceNoteHoldToRecordHint), findsNothing);
    expect(phase(tester).phase, VoiceNoteRecorderPhase.idle);
  });

  testWidgets('a capture of exactly the minimum is kept', (tester) async {
    final recorder = FakeVoiceNoteRecorder(
      recordedDuration: voiceNoteMinDuration,
    );
    await pumpComposer(tester, recorder: recorder);

    final gesture = await holdMic(tester);
    await gesture.up();
    await tester.pumpAndSettle();

    expect(attachmentFor(recorder.path), findsOneWidget);
    expect(find.text(voiceNoteHoldToRecordHint), findsNothing);
  });

  testWidgets('a stop failure surfaces on the error line and frees the pill', (
    tester,
  ) async {
    final recorder = FakeVoiceNoteRecorder()
      ..stopError = TimeoutException('native stop stalled');
    await pumpComposer(tester, recorder: recorder);

    final gesture = await holdMic(tester);
    await gesture.up();
    await tester.pumpAndSettle();

    expect(find.text('Buzz could not finish the voice note.'), findsOneWidget);
    expect(attachmentFor(recorder.path), findsNothing);
    expect(recorderFinder, findsNothing);
    expect(micFinder, findsOneWidget);
    expect(phase(tester).phase, VoiceNoteRecorderPhase.idle);
  });

  testWidgets('popping the page mid-recording releases the phase', (
    tester,
  ) async {
    final recorder = await pumpComposer(tester);
    await tapMic(tester);
    await tester.tap(find.byKey(const ValueKey('voice-note-lock-chip')));
    await tester.pumpAndSettle();
    final scope = container(tester);
    expect(
      scope.read(voiceNoteRecorderPhaseProvider).phase,
      VoiceNoteRecorderPhase.locked,
    );

    final context = tester.element(recorderFinder);
    unawaited(
      Navigator.of(context).pushReplacement(
        MaterialPageRoute<void>(builder: (_) => const Scaffold()),
      ),
    );
    await tester.pumpAndSettle();

    expect(recorderFinder, findsNothing);
    expect(recorder.cancelled, isTrue);
    expect(
      scope.read(voiceNoteRecorderPhaseProvider).phase,
      VoiceNoteRecorderPhase.idle,
    );
  });
}
