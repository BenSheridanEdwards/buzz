import 'package:buzz/features/channels/voice_note_recorder_phase.dart';
import 'package:buzz/shared/relay/relay.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'voice_note_test_support.dart';

/// A take is one press-to-note cycle, identified by the phase generation.
/// The composer swaps the recorder through a fade, so two takes can overlap
/// on screen; these tests pin that only the live one owns the mic (rule 2),
/// and that a take deferred behind the keyboard never opens the mic in the
/// background (rule 4).
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

  Future<List<FakeVoiceNoteRecorder>> pumpComposer(
    WidgetTester tester, {
    int recorders = 4,
    AppLifecycleNotifier Function()? appLifecycle,
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
        // Every take that mounts a recorder takes the next fake, so an extra
        // recorder built by a stale element is visible as a consumed fake.
        voiceNoteRecorderFactory: () => queue.removeAt(0),
        voiceNotePlayerFactory: FakeVoiceNotePlayer.new,
        appLifecycle: appLifecycle,
      ),
    );
    await tester.pumpAndSettle();
    return fakes;
  }

  VoiceNoteRecorderState phase(WidgetTester tester) =>
      ProviderScope.containerOf(
        tester.element(find.byKey(const ValueKey('compose-bar'))),
      ).read(voiceNoteRecorderPhaseProvider);

  testWidgets(
    'a mic press inside the cancel fade starts one recorder and keeps the hold',
    (tester) async {
      final handle = tester.ensureSemantics();
      final recorders = await pumpComposer(tester);

      await tapMic(tester);
      expect(recorders[0].started, isTrue);
      await tester.tap(
        find.byKey(const ValueKey('voice-note-recorder-cancel')),
      );
      await tester.pump();
      expect(phase(tester).phase, VoiceNoteRecorderPhase.idle);

      // Inside the 140 ms fade the cancelled take is still mounted and still
      // watching the phase. Pressing the mic now must not hand it the take.
      await tester.pump(const Duration(milliseconds: 30));
      final gesture = await tester.startGesture(tester.getCenter(micFinder));
      await tester.pump();
      await tester.pump(
        voiceNoteTapHoldThreshold + const Duration(milliseconds: 50),
      );

      expect(phase(tester).phase, VoiceNoteRecorderPhase.holding);
      expect(
        [for (final recorder in recorders) recorder.started],
        [true, true, false, false],
      );
      // The fade ends: the outgoing element goes away without releasing the
      // phase the new take owns.
      await tester.pump(const Duration(milliseconds: 200));
      expect(phase(tester).phase, VoiceNoteRecorderPhase.holding);
      expect(recorders[1].cancelled, isFalse);

      await gesture.up();
      await tester.pumpAndSettle();
      expect(
        find.byKey(const ValueKey('voice-note-attachment:/tmp/take-1.m4a')),
        findsOneWidget,
      );
      // The cancelled take stayed cancelled: it never stopped its recorder
      // for a note it does not own.
      expect(recorders[0].stopped, isFalse);
      expect(
        find.byKey(const ValueKey('voice-note-attachment:/tmp/take-0.m4a')),
        findsNothing,
      );
      expect(phase(tester).phase, VoiceNoteRecorderPhase.idle);
      handle.dispose();
    },
  );

  testWidgets('a send inside the cancel fade stops only the live take', (
    tester,
  ) async {
    final handle = tester.ensureSemantics();
    final recorders = await pumpComposer(tester);

    await tapMic(tester);
    await tester.tap(find.byKey(const ValueKey('voice-note-recorder-cancel')));
    await tester.pump();

    // A fresh hands-free take begins and is sent while the cancelled one is
    // still on screen, so the stale element sees the whole take go by.
    await tester.pump(const Duration(milliseconds: 30));
    final gesture = await tester.startGesture(tester.getCenter(micFinder));
    await tester.pump(const Duration(milliseconds: 20));
    await gesture.up();
    await tester.pump();
    expect(phase(tester).phase, VoiceNoteRecorderPhase.holding);

    // Both takes are laid out for the rest of the fade, but the one on its
    // way out adds no second screen-reader stop for a control the live take
    // owns (rule 7).
    await tester.pump(const Duration(milliseconds: 30));
    expect(recorderFinder, findsNWidgets(2));
    expect(semanticsTreeLabelCount(tester, 'Send voice note'), 1);
    expect(semanticsTreeLabelCount(tester, 'Cancel recording'), 1);

    // Send, through the notifier its button calls: the live take's Send is
    // still growing into the pill and cannot be tapped this early in the
    // fade, and the point here is that both elements see the transition.
    ProviderScope.containerOf(
      tester.element(find.byKey(const ValueKey('compose-bar'))),
    ).read(voiceNoteRecorderPhaseProvider.notifier).finish();
    await tester.pumpAndSettle();

    expect(recorders[1].stopped, isTrue);
    expect(recorders[0].stopped, isFalse);
    expect(
      find.byKey(const ValueKey('voice-note-attachment:/tmp/take-1.m4a')),
      findsOneWidget,
    );
    expect(
      find.byKey(const ValueKey('voice-note-attachment:/tmp/take-0.m4a')),
      findsNothing,
    );
    handle.dispose();
  });

  testWidgets('backgrounding while the keyboard hides abandons the take', (
    tester,
  ) async {
    final lifecycle = FakeAppLifecycleNotifier();
    final recorders = await pumpComposer(tester, appLifecycle: () => lifecycle);
    tester.view.viewInsets = const FakeViewPadding(bottom: 300);
    addTearDown(tester.view.resetViewInsets);
    await tester.pumpAndSettle();

    await tapMic(tester);
    expect(phase(tester).phase, VoiceNoteRecorderPhase.holding);
    expect(recorderFinder, findsNothing);

    lifecycle.setLifecycle(AppLifecycleState.paused);
    await tester.pump();
    expect(phase(tester).phase, VoiceNoteRecorderPhase.idle);

    // The keyboard finishes hiding with the app away, and the app comes back.
    tester.view.resetViewInsets();
    await tester.pumpAndSettle();
    lifecycle.setLifecycle(AppLifecycleState.resumed);
    await tester.pumpAndSettle();

    expect(recorderFinder, findsNothing);
    expect(recorders[0].started, isFalse);
    expect(phase(tester).phase, VoiceNoteRecorderPhase.idle);
    expect(micFinder, findsOneWidget);
  });

  testWidgets('a keyboard-deferred start refuses to open the mic in the '
      'background', (tester) async {
    // Already backgrounded when the press lands, so no transition fires and
    // the deferred start is the only thing left to refuse.
    final recorders = await pumpComposer(
      tester,
      appLifecycle: () => FakeAppLifecycleNotifier(AppLifecycleState.paused),
    );
    tester.view.viewInsets = const FakeViewPadding(bottom: 300);
    addTearDown(tester.view.resetViewInsets);
    await tester.pumpAndSettle();

    await tapMic(tester);
    expect(phase(tester).phase, VoiceNoteRecorderPhase.holding);
    expect(recorderFinder, findsNothing);

    tester.view.resetViewInsets();
    await tester.pumpAndSettle();

    expect(recorderFinder, findsNothing);
    expect(recorders[0].started, isFalse);
    expect(phase(tester).phase, VoiceNoteRecorderPhase.idle);
    expect(micFinder, findsOneWidget);
  });
}
