import 'package:buzz/features/channels/voice_note_recorder_phase.dart';
import 'package:buzz/features/channels/voice_note_recording.dart';
import 'package:buzz/shared/relay/relay.dart';
import 'package:buzz/shared/theme/theme.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart' show SemanticsNode;
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
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
    expect(semanticsNodeCount(tester, 'Record voice note'), 0);

    await gesture.moveBy(const Offset(0, -voiceNoteLockSlideDistance - 8));
    await tester.pumpAndSettle();
    await gesture.up();
    await tester.pumpAndSettle();

    expect(semanticsNodeCount(tester, 'Discard voice note'), 1);
    expect(semanticsNodeCount(tester, 'Pause recording'), 1);
    expect(semanticsNodeCount(tester, 'Send'), 1);
    expect(semanticsNodeCount(tester, 'Lock recording'), 0);
    expect(semanticsNodeCount(tester, 'Recording voice note'), 0);
    expect(
      find.bySemanticsLabel(RegExp('Voice note waveform')).evaluate().length,
      1,
    );
    handle.dispose();
  });

  testWidgets('the hands-free hold state owns Send and Cancel once each', (
    tester,
  ) async {
    final handle = tester.ensureSemantics();
    await pumpComposer(tester);
    await tapMic(tester);

    expect(semanticsNodeCount(tester, 'Send voice note'), 1);
    expect(semanticsNodeCount(tester, 'Cancel recording'), 1);
    expect(semanticsNodeCount(tester, 'Lock recording'), 1);
    expect(semanticsNodeCount(tester, 'Recording voice note'), 0);
    expect(semanticsNodeCount(tester, 'Record voice note'), 0);
    expect(semanticsNodeCount(tester, 'Send'), 0);
    handle.dispose();
  });

  testWidgets('the held mic is 64 px with its halo outside the pill', (
    tester,
  ) async {
    await pumpComposer(tester);
    final gesture = await holdMic(tester);
    await tester.pumpAndSettle();

    final heldMic = find.byKey(const ValueKey('voice-note-recorder-held-mic'));
    expect(tester.getSize(heldMic), const Size(64, 64));
    final halo =
        (tester.widget<Container>(heldMic).decoration! as BoxDecoration)
            .boxShadow!
            .single;
    expect(halo.spreadRadius, 10);
    // Centred on the pill's trailing slot, which stays 44 px.
    final anchor = find.byKey(
      const ValueKey('voice-note-recorder-held-mic-anchor'),
    );
    expect(tester.getSize(anchor), const Size(44, 44));
    expect(
      tester.getCenter(heldMic),
      offsetMoreOrLessEquals(tester.getCenter(anchor), epsilon: 0.5),
    );

    await gesture.up();
    await tester.pumpAndSettle();
  });

  testWidgets('locked controls are 52 px with a red trash', (tester) async {
    await pumpComposer(tester);
    await tapMic(tester);
    await tester.tap(find.byKey(const ValueKey('voice-note-lock-chip')));
    await tester.pumpAndSettle();

    final discard = find.byKey(const ValueKey('voice-note-recorder-discard'));
    expect(tester.getSize(discard), const Size(52, 52));
    expect(
      tester.getSize(find.byKey(const ValueKey('voice-note-recorder-pause'))),
      const Size(52, 52),
    );
    expect(
      tester
          .getSize(find.byKey(const ValueKey('voice-note-recorder-send')))
          .height,
      52,
    );
    final trashIcon = tester.widget<Icon>(
      find.descendant(of: discard, matching: find.byType(Icon)),
    );
    expect(trashIcon.color, AppTheme.light().colorScheme.error);
  });

  testWidgets('a pointer cancelled by the OS keeps recording hands free', (
    tester,
  ) async {
    final recorder = await pumpComposer(tester);
    final gesture = await holdMic(tester);
    expect(phase(tester).hasPointer, isTrue);

    await gesture.cancel();
    await tester.pumpAndSettle();

    expect(phase(tester).phase, VoiceNoteRecorderPhase.holding);
    expect(phase(tester).hasPointer, isFalse);
    expect(recorder.stopped, isFalse);
    expect(recorder.cancelled, isFalse);
    expect(find.text('Cancel'), findsOneWidget);

    await tester.tap(find.byKey(const ValueKey('voice-note-recorder-send')));
    await tester.pumpAndSettle();
    expect(recorder.stopped, isTrue);
    expect(attachmentFinder, findsOneWidget);
  });

  testWidgets('a second finger during a hold neither cancels nor sends', (
    tester,
  ) async {
    final recorder = await pumpComposer(tester);
    final first = await holdMic(tester);
    final firstPointer = phase(tester).pointer;

    final second = await tester.startGesture(
      tester.getCenter(find.byKey(const ValueKey('voice-note-recorder-dot'))),
      pointer: 7,
    );
    await tester.pump();
    await second.moveBy(const Offset(-voiceNoteCancelSlideDistance - 20, 0));
    await tester.pump();
    expect(phase(tester).phase, VoiceNoteRecorderPhase.holding);
    expect(phase(tester).pointer, firstPointer);
    expect(phase(tester).cancelProgress, 0);

    await second.up();
    await tester.pump();
    expect(phase(tester).phase, VoiceNoteRecorderPhase.holding);
    expect(recorder.stopped, isFalse);

    await first.up();
    await tester.pumpAndSettle();
    expect(recorder.stopped, isTrue);
    expect(attachmentFinder, findsOneWidget);
  });

  testWidgets('only a primary-button press starts a hold', (tester) async {
    final recorder = await pumpComposer(tester);

    final rightClick = await tester.startGesture(
      tester.getCenter(micFinder),
      kind: PointerDeviceKind.mouse,
      buttons: kSecondaryButton,
    );
    await tester.pump(voiceNoteTapHoldThreshold * 2);
    expect(recorderFinder, findsNothing);
    expect(recorder.started, isFalse);
    await rightClick.up();
    await tester.pumpAndSettle();

    final barrelPress = await tester.startGesture(
      tester.getCenter(micFinder),
      kind: PointerDeviceKind.stylus,
      buttons: kPrimaryButton | kPrimaryStylusButton,
    );
    await tester.pump(voiceNoteTapHoldThreshold * 2);
    expect(recorderFinder, findsNothing);
    await barrelPress.up();
    await tester.pumpAndSettle();

    final leftClick = await tester.startGesture(
      tester.getCenter(micFinder),
      kind: PointerDeviceKind.mouse,
      buttons: kPrimaryButton,
    );
    await tester.pump();
    await tester.pump(voiceNoteTapHoldThreshold * 2);
    expect(recorderFinder, findsOneWidget);
    await leftClick.up();
    await tester.pumpAndSettle();
  });

  testWidgets('under RTL the cancel slide travels right', (tester) async {
    final recorder = FakeVoiceNoteRecorder();
    await tester.pumpWidget(
      buildVoiceNoteComposeBar(
        prefs: prefs,
        onSend: noSend,
        voiceNoteRecorderFactory: () => recorder,
        voiceNotePlayerFactory: FakeVoiceNotePlayer.new,
        textDirection: TextDirection.rtl,
      ),
    );
    await tester.pumpAndSettle();
    final gesture = await holdMic(tester);

    final chevron = tester.widget<Icon>(
      find.byKey(const ValueKey('voice-note-recorder-cancel-chevron')),
    );
    expect(chevron.icon, LucideIcons.chevronRight);

    await gesture.moveBy(const Offset(-voiceNoteCancelSlideDistance - 20, 0));
    await tester.pump();
    expect(phase(tester).phase, VoiceNoteRecorderPhase.holding);
    expect(phase(tester).cancelProgress, 0);

    await gesture.moveBy(
      const Offset(2 * voiceNoteCancelSlideDistance + 40, 0),
    );
    await tester.pump();
    await gesture.up();
    await tester.pumpAndSettle();
    expect(recorder.cancelled, isTrue);
    expect(recorder.stopped, isFalse);
    expect(phase(tester).phase, VoiceNoteRecorderPhase.idle);
  });

  testWidgets('a release while the keyboard is still hiding shows the hint', (
    tester,
  ) async {
    final recorder = await pumpComposer(tester);
    tester.view.viewInsets = const FakeViewPadding(bottom: 300);
    addTearDown(tester.view.resetViewInsets);
    await tester.pumpAndSettle();

    final gesture = await holdMic(tester);
    // Preparing, but the recorder waits for the keyboard to hide.
    expect(phase(tester).phase, VoiceNoteRecorderPhase.holding);
    expect(recorderFinder, findsNothing);
    expect(recorder.started, isFalse);

    await gesture.up();
    await tester.pumpAndSettle();

    expect(find.text(voiceNoteHoldToRecordHint), findsOneWidget);
    expect(recorderFinder, findsNothing);
    expect(micFinder, findsOneWidget);
    expect(phase(tester).phase, VoiceNoteRecorderPhase.idle);
  });

  testWidgets('status is a live region and the timer is announced on tens', (
    tester,
  ) async {
    final handle = tester.ensureSemantics();
    await pumpComposer(tester);
    await tapMic(tester);
    await tester.tap(find.byKey(const ValueKey('voice-note-lock-chip')));
    await tester.pumpAndSettle();

    SemanticsNode status() => tester.getSemantics(
      find.byKey(const ValueKey('voice-note-recorder-status')),
    );
    expect(status().label, 'Locked');
    expect(status().flagsCollection.isLiveRegion, isTrue);

    SemanticsNode timer() => tester.getSemantics(
      find.byKey(const ValueKey('voice-note-recorder-timer-semantics')),
    );
    String shown() => tester
        .widget<Text>(
          find.byKey(const ValueKey('voice-note-recorder-duration')),
        )
        .data!;
    expect(timer().flagsCollection.isLiveRegion, isTrue);
    expect(timer().label, 'Recording');

    Future<void> pumpUntilShown(String text) async {
      for (var step = 0; step < 20 && shown() != text; step++) {
        await tester.pump(const Duration(milliseconds: 500));
      }
      expect(shown(), text);
    }

    await tester.pump(const Duration(seconds: 9));
    await pumpUntilShown('0:10');
    expect(timer().label, 'Recording, 0:10');

    // Between tens the visible text ticks but the announced label holds,
    // and the ticking text itself never reaches the screen reader.
    await tester.pump(const Duration(seconds: 3));
    expect(shown(), '0:13');
    expect(timer().label, 'Recording, 0:10');
    expect(find.bySemanticsLabel('0:13'), findsNothing);

    await tester.pump(voiceNoteMaxDuration - const Duration(seconds: 24));
    await pumpUntilShown('4:50');
    expect(timer().label, 'Recording, 4:50, 10 seconds before the limit');

    await tester.tap(find.byKey(const ValueKey('voice-note-recorder-pause')));
    await tester.pumpAndSettle();
    expect(status().label, 'Paused');
    expect(status().flagsCollection.isLiveRegion, isTrue);
    handle.dispose();
  });
}
