import 'dart:async';
import 'dart:io';

import 'package:clock/clock.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:record/record.dart';
import 'package:buzz/features/channels/voice_note_composer_recorder.dart';
import 'package:buzz/features/channels/voice_note_recording.dart';

class _DelayedRecorderBackend implements VoiceNoteRecorderBackend {
  final permission = Completer<bool>();
  final nativeStart = Completer<void>();
  final nativeStop = Completer<String?>();
  final amplitudes = StreamController<Amplitude>.broadcast();
  bool startCalled = false;
  bool pauseCalled = false;
  bool resumeCalled = false;
  bool stopCalled = false;
  bool stopCompleted = false;
  bool cancelCalled = false;
  bool disposeCalled = false;
  bool terminalOverlap = false;

  @override
  Future<bool> hasPermission() => permission.future;

  /// Path the native recorder was told to write, so a test can stand in for
  /// the file it leaves behind.
  String? startedPath;

  @override
  Future<void> start(RecordConfig config, {required String path}) {
    startCalled = true;
    startedPath = path;
    return nativeStart.future;
  }

  @override
  Stream<Amplitude> onAmplitudeChanged(Duration interval) => amplitudes.stream;

  @override
  Future<void> pause() async {
    pauseCalled = true;
  }

  @override
  Future<void> resume() async {
    resumeCalled = true;
  }

  @override
  Future<String?> stop() async {
    stopCalled = true;
    final path = await nativeStop.future;
    stopCompleted = true;
    return path;
  }

  @override
  Future<void> cancel() async {
    if (stopCalled && !stopCompleted) terminalOverlap = true;
    cancelCalled = true;
  }

  @override
  Future<void> dispose() async {
    if (stopCalled && !stopCompleted) terminalOverlap = true;
    disposeCalled = true;
    await amplitudes.close();
  }
}

/// Capture side of voice notes: the device recorder's cancellation fences,
/// pause accounting, stop timeout, and the files it must not leave behind.
/// Playback lives in `voice_note_player_test.dart`.
void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  test(
    'dropped finalized recordings are deleted without surfacing errors',
    () async {
      final directory = await Directory.systemTemp.createTemp(
        'voice-note-dropped-recording-test',
      );
      addTearDown(() => directory.delete(recursive: true));
      final recording = File('${directory.path}/recording.m4a');
      await recording.writeAsBytes([1, 2, 3]);

      await deleteDroppedVoiceNoteRecording(recording.path);
      await deleteDroppedVoiceNoteRecording(recording.path);

      expect(await recording.exists(), isFalse);
    },
  );

  test('cancellation fences delayed permission before native start', () async {
    final backend = _DelayedRecorderBackend();
    final directory = await Directory.systemTemp.createTemp('voice-note-test');
    addTearDown(() => directory.delete(recursive: true));
    final recorder = DeviceVoiceNoteRecorder(
      backend: backend,
      temporaryDirectory: () async => directory,
    );

    final startup = recorder.start();
    final startupExpectation = expectLater(startup, throwsStateError);
    final cancellation = recorder.cancel();
    backend.permission.complete(true);

    await cancellation;
    await startupExpectation;
    expect(backend.startCalled, isFalse);
    await recorder.dispose();
    expect(backend.disposeCalled, isTrue);
  });

  test('cancellation ends native recording when start resolves late', () async {
    final backend = _DelayedRecorderBackend();
    final directory = await Directory.systemTemp.createTemp('voice-note-test');
    addTearDown(() => directory.delete(recursive: true));
    final recorder = DeviceVoiceNoteRecorder(
      backend: backend,
      temporaryDirectory: () async => directory,
    );

    final startup = recorder.start();
    final startupExpectation = expectLater(startup, throwsStateError);
    backend.permission.complete(true);
    await Future<void>.delayed(Duration.zero);
    expect(backend.startCalled, isTrue);

    final cancellation = recorder.cancel();
    backend.nativeStart.complete();
    await cancellation;
    await startupExpectation;

    expect(backend.cancelCalled, isTrue);
    await recorder.dispose();
  });

  test('pause and resume forward to the backend and gate samples', () async {
    final backend = _DelayedRecorderBackend();
    final directory = await Directory.systemTemp.createTemp('voice-note-test');
    addTearDown(() => directory.delete(recursive: true));
    final recorder = DeviceVoiceNoteRecorder(
      backend: backend,
      temporaryDirectory: () async => directory,
    );
    final levels = <double>[];
    recorder.levels.listen(levels.add);
    backend.permission.complete(true);
    backend.nativeStart.complete();
    await recorder.start();

    backend.amplitudes.add(Amplitude(current: -6, max: 0));
    await Future<void>.delayed(Duration.zero);
    expect(levels, hasLength(1));

    await recorder.pause();
    expect(backend.pauseCalled, isTrue);
    backend.amplitudes.add(Amplitude(current: -6, max: 0));
    await Future<void>.delayed(Duration.zero);
    expect(levels, hasLength(1));

    // Pausing twice is idempotent and resuming re-enables levels.
    await recorder.pause();
    await recorder.resume();
    expect(backend.resumeCalled, isTrue);
    backend.amplitudes.add(Amplitude(current: -6, max: 0));
    await Future<void>.delayed(Duration.zero);
    expect(levels, hasLength(2));

    final stopping = recorder.stop();
    backend.nativeStop.complete('/tmp/voice-note-test.m4a');
    final recording = await stopping;
    expect(recording.waveform, hasLength(2));
    expect(recording.duration, greaterThanOrEqualTo(Duration.zero));
    await recorder.dispose();
  });

  test('paused time is excluded from the recorded duration', () async {
    var now = DateTime(2026, 9, 8, 9);
    await withClock(Clock(() => now), () async {
      final backend = _DelayedRecorderBackend();
      final directory = await Directory.systemTemp.createTemp(
        'voice-note-test',
      );
      addTearDown(() => directory.delete(recursive: true));
      final recorder = DeviceVoiceNoteRecorder(
        backend: backend,
        temporaryDirectory: () async => directory,
      );
      backend.permission.complete(true);
      backend.nativeStart.complete();
      await recorder.start();

      now = now.add(const Duration(seconds: 20));
      await recorder.pause();
      now = now.add(const Duration(minutes: 3));
      await recorder.resume();
      now = now.add(const Duration(seconds: 10));
      // A second pause that is still open when stop is called also counts.
      await recorder.pause();
      now = now.add(const Duration(seconds: 45));

      final stopping = recorder.stop();
      backend.nativeStop.complete('/tmp/voice-note-test.m4a');
      final recording = await stopping;
      expect(recording.duration, const Duration(seconds: 30));
      await recorder.dispose();
    });
  });

  test('a stalled native stop times out as a failure', () async {
    final backend = _DelayedRecorderBackend();
    final directory = await Directory.systemTemp.createTemp('voice-note-test');
    addTearDown(() => directory.delete(recursive: true));
    final recorder = DeviceVoiceNoteRecorder(
      backend: backend,
      temporaryDirectory: () async => directory,
      stopTimeout: const Duration(milliseconds: 50),
    );
    backend.permission.complete(true);
    backend.nativeStart.complete();
    await recorder.start();

    await expectLater(recorder.stop(), throwsA(isA<TimeoutException>()));
    expect(backend.stopCalled, isTrue);
    expect(backend.stopCompleted, isFalse);

    // Disposal still ends the native recording the stop never confirmed.
    await recorder.dispose();
    expect(backend.cancelCalled, isTrue);
    expect(backend.disposeCalled, isTrue);
  });

  test('a timed-out stop leaves no capture file behind', () async {
    final backend = _DelayedRecorderBackend();
    final directory = await Directory.systemTemp.createTemp('voice-note-test');
    addTearDown(() => directory.delete(recursive: true));
    final recorder = DeviceVoiceNoteRecorder(
      backend: backend,
      temporaryDirectory: () async => directory,
      stopTimeout: const Duration(milliseconds: 50),
    );
    backend.permission.complete(true);
    backend.nativeStart.complete();
    await recorder.start();
    final file = File(backend.startedPath!);
    await file.writeAsBytes(const [1, 2, 3]);

    await expectLater(recorder.stop(), throwsA(isA<TimeoutException>()));
    expect(await file.exists(), isFalse);

    // The stop that timed out finishes on its own and writes the take again;
    // disposal owns the file the composer was never handed.
    await file.writeAsBytes(const [1, 2, 3]);
    await recorder.dispose();
    expect(await file.exists(), isFalse);
  });

  test(
    'dispose waits for an in-flight stop before releasing backend',
    () async {
      final backend = _DelayedRecorderBackend();
      final directory = await Directory.systemTemp.createTemp(
        'voice-note-test',
      );
      addTearDown(() => directory.delete(recursive: true));
      final recorder = DeviceVoiceNoteRecorder(
        backend: backend,
        temporaryDirectory: () async => directory,
      );
      backend.permission.complete(true);
      backend.nativeStart.complete();
      await recorder.start();

      final stopping = recorder.stop();
      final disposing = recorder.dispose();
      await Future<void>.delayed(Duration.zero);

      expect(backend.stopCalled, isTrue);
      expect(backend.cancelCalled, isFalse);
      expect(backend.disposeCalled, isFalse);
      expect(backend.terminalOverlap, isFalse);

      backend.nativeStop.complete('/tmp/voice-note-test.m4a');
      final recording = await stopping;
      await disposing;

      expect(recording.file.path, '/tmp/voice-note-test.m4a');
      expect(backend.stopCompleted, isTrue);
      expect(backend.cancelCalled, isFalse);
      expect(backend.disposeCalled, isTrue);
      expect(backend.terminalOverlap, isFalse);
    },
  );
}
