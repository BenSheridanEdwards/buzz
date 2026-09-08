import 'package:buzz/features/channels/voice_note_recorder_phase.dart';
import 'package:buzz/features/channels/voice_note_recording.dart';
import 'package:buzz/shared/relay/relay.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'voice_note_test_support.dart';

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

  VoiceNoteRecorderState phase(WidgetTester tester) =>
      ProviderScope.containerOf(
        tester.element(find.byKey(const ValueKey('compose-bar'))),
      ).read(voiceNoteRecorderPhaseProvider);

  final slotFinder = find
      .byKey(const ValueKey('composer-trailing-slot'))
      .hitTestable();
  final attachmentFinder = find.byKey(
    const ValueKey('voice-note-attachment:/tmp/voice-note-test.m4a'),
  );

  testWidgets('mic becomes Send with text and back, in the same 44 px slot', (
    tester,
  ) async {
    await pumpComposer(tester);

    expect(micFinder, findsOneWidget);
    expect(sendSlotFinder, findsNothing);
    expect(tester.getSize(slotFinder), const Size(44, 44));

    await tester.tap(find.text('Message…'));
    await tester.pumpAndSettle();
    expect(micFinder, findsOneWidget);
    final emptyRect = tester.getRect(slotFinder);
    expect(emptyRect.size, const Size(44, 44));

    await tester.enterText(find.byType(TextField), 'hello');
    await tester.pumpAndSettle();

    expect(micFinder, findsNothing);
    expect(sendSlotFinder, findsOneWidget);
    expect(tester.getSize(sendSlotFinder), const Size(44, 44));
    expect(tester.getRect(slotFinder), emptyRect);

    await tester.enterText(find.byType(TextField), '');
    await tester.pumpAndSettle();

    expect(micFinder, findsOneWidget);
    expect(sendSlotFinder, findsNothing);
    expect(tester.getRect(slotFinder), emptyRect);
  });

  testWidgets('holding the mic records and releasing sends', (tester) async {
    final recorder = await pumpComposer(tester);

    final gesture = await holdMic(tester);

    expect(recorder.started, isTrue);
    expect(recorderFinder, findsOneWidget);
    expect(
      find.byKey(const ValueKey('voice-note-recorder-held-mic')),
      findsOneWidget,
    );
    expect(find.text('Slide to cancel'), findsOneWidget);
    expect(find.byKey(const ValueKey('voice-note-lock-chip')), findsOneWidget);
    expect(phase(tester).phase, VoiceNoteRecorderPhase.holding);
    expect(phase(tester).hasPointer, isTrue);

    await gesture.up();
    await tester.pumpAndSettle();

    expect(recorder.stopped, isTrue);
    expect(recorderFinder, findsNothing);
    expect(attachmentFinder, findsOneWidget);
    expect(phase(tester).phase, VoiceNoteRecorderPhase.idle);
  });

  testWidgets('sliding left past the threshold discards the recording', (
    tester,
  ) async {
    final recorder = await pumpComposer(tester);
    final gesture = await holdMic(tester);

    await gesture.moveBy(const Offset(-40, 0));
    await tester.pump();
    expect(recorderFinder, findsOneWidget);
    expect(phase(tester).cancelProgress, greaterThan(0));

    await gesture.moveBy(const Offset(-voiceNoteCancelSlideDistance, 0));
    await tester.pump();
    await gesture.up();
    await tester.pumpAndSettle();

    expect(recorder.cancelled, isTrue);
    expect(recorder.disposed, isTrue);
    expect(recorder.stopped, isFalse);
    expect(recorderFinder, findsNothing);
    expect(attachmentFinder, findsNothing);
    expect(micFinder, findsOneWidget);
    expect(phase(tester).phase, VoiceNoteRecorderPhase.idle);
  });

  testWidgets('sliding up locks the recording hands free', (tester) async {
    final recorder = await pumpComposer(tester);
    final gesture = await holdMic(tester);

    await gesture.moveBy(const Offset(0, -voiceNoteLockSlideDistance - 8));
    await tester.pumpAndSettle();
    await gesture.up();
    await tester.pumpAndSettle();

    expect(phase(tester).phase, VoiceNoteRecorderPhase.locked);
    expect(
      find.byKey(const ValueKey('voice-note-recorder-locked')),
      findsOneWidget,
    );
    expect(find.text('Locked'), findsOneWidget);
    expect(find.byKey(const ValueKey('voice-note-lock-chip')), findsNothing);
    expect(recorder.stopped, isFalse);
    expect(recorder.cancelled, isFalse);
  });

  testWidgets('locked controls pause, resume, discard, and send', (
    tester,
  ) async {
    final recorder = await pumpComposer(tester);
    await tapMic(tester);
    await tester.tap(find.byKey(const ValueKey('voice-note-lock-chip')));
    await tester.pumpAndSettle();
    expect(phase(tester).phase, VoiceNoteRecorderPhase.locked);

    await tester.tap(find.byKey(const ValueKey('voice-note-recorder-pause')));
    await tester.pumpAndSettle();
    expect(recorder.pauseCalls, 1);
    expect(phase(tester).phase, VoiceNoteRecorderPhase.paused);
    expect(find.text('Paused'), findsOneWidget);

    await tester.tap(find.byKey(const ValueKey('voice-note-recorder-resume')));
    await tester.pumpAndSettle();
    expect(recorder.resumeCalls, 1);
    expect(phase(tester).phase, VoiceNoteRecorderPhase.locked);

    await tester.tap(find.byKey(const ValueKey('voice-note-recorder-send')));
    await tester.pumpAndSettle();
    expect(recorder.stopped, isTrue);
    expect(attachmentFinder, findsOneWidget);
    expect(phase(tester).phase, VoiceNoteRecorderPhase.idle);
  });

  testWidgets('locked trash discards without a note', (tester) async {
    final recorder = await pumpComposer(tester);
    await tapMic(tester);
    await tester.tap(find.byKey(const ValueKey('voice-note-lock-chip')));
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const ValueKey('voice-note-recorder-discard')));
    await tester.pumpAndSettle();

    expect(recorder.cancelled, isTrue);
    expect(recorder.stopped, isFalse);
    expect(attachmentFinder, findsNothing);
    expect(micFinder, findsOneWidget);
    expect(phase(tester).phase, VoiceNoteRecorderPhase.idle);
  });

  testWidgets('paused time is excluded and the 5:00 cap finalises', (
    tester,
  ) async {
    final recorder = await pumpComposer(tester);
    await tapMic(tester);
    await tester.tap(find.byKey(const ValueKey('voice-note-lock-chip')));
    await tester.pumpAndSettle();

    final timer = find.byKey(const ValueKey('voice-note-recorder-duration'));
    Duration shown() {
      final parts = tester.widget<Text>(timer).data!.split(':');
      return Duration(
        minutes: int.parse(parts[0]),
        seconds: int.parse(parts[1]),
      );
    }

    await tester.pump(const Duration(minutes: 1));
    final beforePause = shown();
    expect(beforePause, greaterThanOrEqualTo(const Duration(minutes: 1)));
    expect(beforePause, lessThan(const Duration(minutes: 1, seconds: 5)));

    await tester.tap(find.byKey(const ValueKey('voice-note-recorder-pause')));
    await tester.pump(const Duration(minutes: 3));
    expect(shown(), beforePause);
    expect(recorder.pauseCalls, 1);

    await tester.tap(find.byKey(const ValueKey('voice-note-recorder-resume')));
    await tester.pump(const Duration(minutes: 3));
    expect(shown(), beforePause + const Duration(minutes: 3));
    expect(recorder.stopped, isFalse);

    await tester.pump(voiceNoteMaxDuration - beforePause);
    await tester.pumpAndSettle();

    expect(recorder.stopped, isTrue);
    expect(recorderFinder, findsNothing);
    expect(attachmentFinder, findsOneWidget);
    expect(phase(tester).phase, VoiceNoteRecorderPhase.idle);
  });

  testWidgets('tap paths: mic starts hands free, hint cancels', (tester) async {
    final recorder = await pumpComposer(tester);
    await tapMic(tester);

    expect(recorder.started, isTrue);
    expect(phase(tester).phase, VoiceNoteRecorderPhase.holding);
    expect(phase(tester).hasPointer, isFalse);
    expect(find.text('Cancel'), findsOneWidget);
    expect(
      find.byKey(const ValueKey('voice-note-recorder-send')),
      findsOneWidget,
    );

    await tester.tap(find.byKey(const ValueKey('voice-note-recorder-cancel')));
    await tester.pumpAndSettle();

    expect(recorder.cancelled, isTrue);
    expect(recorderFinder, findsNothing);
    expect(phase(tester).phase, VoiceNoteRecorderPhase.idle);
  });

  testWidgets('Escape cancels and Enter sends when locked', (tester) async {
    final recorder = await pumpComposer(tester);
    await tapMic(tester);
    await tester.sendKeyEvent(LogicalKeyboardKey.escape);
    await tester.pumpAndSettle();
    expect(recorder.cancelled, isTrue);
    expect(recorderFinder, findsNothing);
    expect(phase(tester).phase, VoiceNoteRecorderPhase.idle);

    final second = FakeVoiceNoteRecorder(path: '/tmp/voice-note-test.m4a');
    await pumpComposer(tester, recorder: second);
    await tapMic(tester);
    await tester.tap(find.byKey(const ValueKey('voice-note-lock-chip')));
    await tester.pumpAndSettle();
    await tester.sendKeyEvent(LogicalKeyboardKey.enter);
    await tester.pumpAndSettle();

    expect(second.stopped, isTrue);
    expect(attachmentFinder, findsOneWidget);
    expect(phase(tester).phase, VoiceNoteRecorderPhase.idle);
  });

  testWidgets('permission failure returns the pill to idle with the error', (
    tester,
  ) async {
    final recorder = FakeVoiceNoteRecorder()
      ..startError = StateError(
        'Microphone access is required to record a voice note.',
      );
    await pumpComposer(tester, recorder: recorder);

    final gesture = await holdMic(tester);
    await tester.pumpAndSettle();

    expect(recorderFinder, findsNothing);
    expect(
      find.text('Microphone access is required to record a voice note.'),
      findsOneWidget,
    );
    expect(micFinder, findsOneWidget);
    expect(phase(tester).phase, VoiceNoteRecorderPhase.idle);
    expect(recorder.disposed, isTrue);

    // The finger is still down; lifting it must not resurrect anything.
    await gesture.up();
    await tester.pumpAndSettle();
    expect(recorderFinder, findsNothing);
    expect(phase(tester).phase, VoiceNoteRecorderPhase.idle);
  });

  testWidgets('backgrounding mid-hold clears the phase and the pointer', (
    tester,
  ) async {
    final lifecycle = FakeAppLifecycleNotifier();
    final recorder = await pumpComposer(tester, appLifecycle: () => lifecycle);
    final gesture = await holdMic(tester);

    lifecycle.setLifecycle(AppLifecycleState.paused);
    await tester.pumpAndSettle();

    expect(recorder.cancelled, isTrue);
    expect(recorderFinder, findsNothing);
    expect(phase(tester).phase, VoiceNoteRecorderPhase.idle);

    await gesture.up();
    await tester.pumpAndSettle();
    expect(attachmentFinder, findsNothing);
    expect(micFinder, findsOneWidget);
  });

  testWidgets('every actionable label has exactly one semantics owner', (
    tester,
  ) async {
    final handle = tester.ensureSemantics();
    await pumpComposer(tester);

    expect(semanticsNodeCount(tester, 'Record voice note'), 1);

    final gesture = await holdMic(tester);
    expect(semanticsNodeCount(tester, 'Lock recording'), 1);
    expect(semanticsNodeCount(tester, 'Recording voice note'), 1);
    expect(semanticsNodeCount(tester, 'Cancel recording'), 0);

    await gesture.moveBy(const Offset(0, -voiceNoteLockSlideDistance - 8));
    await tester.pumpAndSettle();
    await gesture.up();
    await tester.pumpAndSettle();

    expect(semanticsNodeCount(tester, 'Discard voice note'), 1);
    expect(semanticsNodeCount(tester, 'Pause recording'), 1);
    expect(semanticsNodeCount(tester, 'Send'), 1);
    expect(semanticsNodeCount(tester, 'Lock recording'), 0);
    expect(
      find.bySemanticsLabel(RegExp('Voice note waveform')).evaluate().length,
      1,
    );
    handle.dispose();
  });
}
