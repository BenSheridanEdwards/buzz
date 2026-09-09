import 'dart:async';
import 'dart:io';

import 'package:buzz/features/channels/channel.dart';
import 'package:buzz/features/channels/channel_management_provider.dart';
import 'package:buzz/features/channels/channels_provider.dart';
import 'package:buzz/features/channels/compose_bar.dart';
import 'package:buzz/features/channels/photo_library.dart';
import 'package:buzz/features/channels/voice_note_recorder_phase.dart';
import 'package:buzz/features/channels/voice_note_recording.dart';
import 'package:buzz/shared/custom_emoji/custom_emoji_provider.dart';
import 'package:buzz/shared/mentions/agent_identity_provider.dart';
import 'package:buzz/shared/relay/relay.dart';
import 'package:buzz/shared/theme/theme.dart';
import 'package:buzz/shared/voice_notes/voice_note_preferences.dart';
import 'package:flutter/material.dart';
import 'package:flutter/semantics.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:image_picker/image_picker.dart';
import 'package:nostr/nostr.dart' as nostr;
import 'package:shared_preferences/shared_preferences.dart';

/// Recorder fake that records every lifecycle call the composer makes.
class FakeVoiceNoteRecorder implements VoiceNoteRecorder {
  FakeVoiceNoteRecorder({
    this.path = '/tmp/voice-note-test.m4a',
    this.recordedDuration = const Duration(seconds: 3),
  });

  final String path;
  final Duration recordedDuration;
  final StreamController<double> _levels = StreamController.broadcast(
    sync: true,
  );
  bool started = false;
  bool cancelled = false;
  bool disposed = false;
  bool stopped = false;
  int pauseCalls = 0;
  int resumeCalls = 0;
  Object? startError;
  Object? stopError;

  /// When set, [start] waits for it, like a slow permission prompt.
  Completer<void>? pendingStart;

  /// When set, [stop] waits for it, like a slow native finalisation.
  Completer<void>? pendingStop;

  @override
  Stream<double> get levels => _levels.stream;

  void emit(double level) => _levels.add(level);

  @override
  Future<void> start() async {
    await pendingStart?.future;
    if (startError case final error?) throw error;
    started = true;
    _levels.add(0.72);
  }

  @override
  Future<void> pause() async {
    pauseCalls += 1;
  }

  @override
  Future<void> resume() async {
    resumeCalls += 1;
  }

  @override
  Future<VoiceNoteRecording> stop() async {
    stopped = true;
    await pendingStop?.future;
    if (stopError case final error?) throw error;
    return VoiceNoteRecording(
      file: XFile(path, mimeType: 'audio/mp4'),
      duration: recordedDuration,
      waveform: const [0.2, 0.7, 0.4, 0.9],
    );
  }

  @override
  Future<void> cancel() async {
    cancelled = true;
  }

  @override
  Future<void> dispose() async {
    disposed = true;
    await _levels.close();
  }
}

/// Player fake whose state tests can drive directly.
class FakeVoiceNotePlayer extends VoiceNotePlayerController {
  FakeVoiceNotePlayer({
    Duration duration = const Duration(seconds: 24),
    this.loadsLazily = false,
  }) : _state = VoiceNotePlaybackState(duration: duration);

  /// Like a remote note before its first play: no seekable source yet.
  final bool loadsLazily;

  VoiceNotePlaybackState _state;
  double speed = 1;
  final List<double> speeds = [];
  final List<Duration> seeks = [];

  @override
  VoiceNotePlaybackState get state => _state;

  void setPosition(Duration position) {
    _state = _state.copyWith(position: position);
    notifyListeners();
  }

  @override
  Future<void> loadLocal(
    String path, {
    required Duration fallbackDuration,
  }) async {
    _state = VoiceNotePlaybackState(
      duration: fallbackDuration,
      canSeek: !loadsLazily,
    );
    notifyListeners();
  }

  @override
  Future<void> loadRemote(
    String url, {
    required Map<String, String> Function() headers,
    required Duration fallbackDuration,
  }) => loadLocal(url, fallbackDuration: fallbackDuration);

  @override
  Future<void> pause() async {
    _state = _state.copyWith(isPlaying: false);
    notifyListeners();
  }

  @override
  Future<void> seek(Duration position) async {
    seeks.add(position);
    if (!_state.canSeek) return;
    _state = _state.copyWith(position: position);
    notifyListeners();
  }

  @override
  Future<void> setSpeed(double value) async {
    speed = value;
    speeds.add(value);
  }

  @override
  Future<void> toggle() async {
    // Playing loads the source, as the device player does lazily.
    _state = _state.copyWith(isPlaying: !_state.isPlaying, canSeek: true);
    notifyListeners();
  }
}

/// Phase notifier fake that starts in a chosen phase for card-level tests.
class FakeVoiceNoteRecorderPhaseNotifier
    extends VoiceNoteRecorderPhaseNotifier {
  FakeVoiceNoteRecorderPhaseNotifier([this.initial]);

  final VoiceNoteRecorderState? initial;

  @override
  VoiceNoteRecorderState build() => initial ?? const VoiceNoteRecorderState();
}

class FakeChannelsNotifier extends ChannelsNotifier {
  FakeChannelsNotifier(this._channels);

  final List<Channel> _channels;

  @override
  List<ChannelMember> cachedMembersForChannel(String channelId) => const [];

  @override
  Future<List<Channel>> build() async => _channels;

  @override
  Future<void> refresh({bool fetchDirectory = false}) async {
    state = AsyncData(_channels);
  }
}

class FakeAppLifecycleNotifier extends AppLifecycleNotifier {
  FakeAppLifecycleNotifier([this.initial = AppLifecycleState.resumed]);

  /// Lifecycle the app is already in when the composer mounts, so a test can
  /// exercise a guard that reads the current state rather than a transition.
  final AppLifecycleState initial;

  @override
  AppLifecycleState build() => initial;

  void setLifecycle(AppLifecycleState value) => state = value;
}

class _FakeRelayConfigNotifier extends RelayConfigNotifier {
  @override
  RelayConfig build() => RelayConfig(
    baseUrl: 'http://localhost:3000',
    nsec: nostr.Keys.generate().nsec,
  );
}

class _EmptyPhotoLibrary implements PhotoLibrary {
  const _EmptyPhotoLibrary();

  @override
  Future<List<RecentPhoto>> loadRecentPhotos() async => const [];

  @override
  Future<List<XFile>> resolveSelectedPhotos(List<RecentPhoto> photos) async =>
      const [];
}

/// Builds a channel fixture with the given type.
Channel testChannel({String id = 'channel-1', String channelType = 'public'}) =>
    Channel(
      id: id,
      name: id,
      channelType: channelType,
      visibility: 'open',
      description: '',
      createdBy: 'creator',
      createdAt: DateTime(2026),
      memberCount: 2,
    );

/// Mounts a [ComposeBar] with fake media, recorder, and prefs seams.
Widget buildVoiceNoteComposeBar({
  required SharedPreferences prefs,
  required ComposeBarOnSend onSend,
  VoiceNoteRecorder Function()? voiceNoteRecorderFactory,
  VoiceNotePlayerController Function()? voiceNotePlayerFactory,
  AppLifecycleNotifier Function()? appLifecycle,
  VoiceNoteRecorderPhaseNotifier Function()? phaseNotifier,
  bool disableAnimations = false,
  TextDirection textDirection = TextDirection.ltr,
}) {
  return ProviderScope(
    overrides: [
      customEmojiListProvider.overrideWithValue(const []),
      mediaUploadServiceProvider.overrideWithValue(
        MediaUploadService(
          baseUrl: 'https://relay.example',
          nsec: nostr.Keys.generate().nsec,
          pickGalleryImage: () async => null,
          pickGalleryVideo: () async => null,
        ),
      ),
      if (voiceNoteRecorderFactory != null)
        voiceNoteRecorderFactoryProvider.overrideWithValue(
          voiceNoteRecorderFactory,
        ),
      if (voiceNotePlayerFactory != null)
        voiceNotePlayerFactoryProvider.overrideWithValue(
          voiceNotePlayerFactory,
        ),
      if (phaseNotifier != null)
        voiceNoteRecorderPhaseProvider.overrideWith(phaseNotifier),
      photoLibraryProvider.overrideWithValue(const _EmptyPhotoLibrary()),
      currentPubkeyProvider.overrideWith((ref) => null),
      channelMembersProvider(
        'channel-1',
      ).overrideWith((ref) => Future.value(const <ChannelMember>[])),
      agentDirectoryProvider.overrideWith(
        (ref) async => const <AgentDirectoryEntry>[],
      ),
      agentOwnersProvider.overrideWith((ref) async => const <String, String>{}),
      relayClientProvider.overrideWithValue(
        RelayClient(baseUrl: 'http://localhost:3000'),
      ),
      relayConfigProvider.overrideWith(_FakeRelayConfigNotifier.new),
      // The real notifier listens to the connectivity plugin, which has no
      // implementation under test and would surface as a stray exception.
      appLifecycleProvider.overrideWith(
        appLifecycle ?? FakeAppLifecycleNotifier.new,
      ),
      savedPrefsProvider.overrideWithValue(prefs),
      channelsProvider.overrideWith(() => FakeChannelsNotifier(const [])),
    ],
    child: MaterialApp(
      navigatorObservers: [voiceNoteRouteObserver],
      theme: AppTheme.light(),
      builder: (context, child) {
        var wrapped = child!;
        if (disableAnimations) {
          wrapped = MediaQuery(
            data: MediaQuery.of(context).copyWith(disableAnimations: true),
            child: wrapped,
          );
        }
        return Directionality(textDirection: textDirection, child: wrapped);
      },
      home: Scaffold(
        body: SafeArea(
          child: Align(
            alignment: Alignment.bottomCenter,
            child: ComposeBar(
              key: const ValueKey('compose-bar'),
              channelId: 'channel-1',
              onSend: onSend,
            ),
          ),
        ),
      ),
    ),
  );
}

/// Seeds mock prefs, optionally turning the review setting on.
Future<SharedPreferences> testPrefs({bool reviewBeforeSending = false}) async {
  SharedPreferences.setMockInitialValues({
    if (reviewBeforeSending) voiceNoteReviewBeforeSendPrefsKey: true,
  });
  return SharedPreferences.getInstance();
}

/// Finder for the mic in the pill's trailing slot.
final micFinder = find.byKey(const ValueKey('composer-mic')).hitTestable();

/// Finder for Send in the pill's trailing slot.
final sendSlotFinder = find
    .byKey(const ValueKey('composer-send'))
    .hitTestable();

/// Finder for the mounted recorder root.
final recorderFinder = find.byKey(const ValueKey('voice-note-recorder'));

/// Presses the mic and holds past the tap threshold; returns the gesture.
Future<TestGesture> holdMic(WidgetTester tester) async {
  await tester.pumpAndSettle();
  final gesture = await tester.startGesture(tester.getCenter(micFinder));
  await tester.pump();
  await tester.pump(
    voiceNoteTapHoldThreshold + const Duration(milliseconds: 50),
  );
  return gesture;
}

/// Taps the mic quickly so recording starts hands free.
Future<void> tapMic(WidgetTester tester) async {
  await tester.pumpAndSettle();
  final gesture = await tester.startGesture(tester.getCenter(micFinder));
  await tester.pump(const Duration(milliseconds: 20));
  await gesture.up();
  await tester.pumpAndSettle();
}

/// Counts semantics nodes carrying exactly [label].
int semanticsNodeCount(WidgetTester tester, String label) =>
    find.bySemanticsLabel(label).evaluate().length;

/// Counts nodes labelled [label] in the semantics tree a screen reader
/// actually walks. Unlike [semanticsNodeCount] this misses nothing and,
/// more importantly, counts nothing that has been excluded from the tree.
int semanticsTreeLabelCount(WidgetTester tester, String label) {
  final root = tester.binding.rootElement?.renderObject?.debugSemantics;
  if (root == null) return 0;
  var count = 0;
  void visit(SemanticsNode node) {
    if (node.label == label) count += 1;
    node.visitChildren((child) {
      visit(child);
      return true;
    });
  }

  visit(root);
  return count;
}

/// Switches the composer to another community, which changes its draft
/// identity exactly as leaving for another relay does in production.
void switchCommunity(WidgetTester tester) {
  final container = ProviderScope.containerOf(
    tester.element(find.byKey(const ValueKey('compose-bar'))),
  );
  final current = container.read(relayConfigProvider);
  container
      .read(relayConfigProvider.notifier)
      .update(baseUrl: 'http://other.example:3000', nsec: current.nsec);
}

/// Creates a real recording file so deletion on a removal path is provable.
/// Real I/O only completes under [WidgetTester.runAsync] in a widget test.
Future<File> createRecordingFile(WidgetTester tester, String name) async {
  final file = await tester.runAsync(() async {
    final directory = await Directory.systemTemp.createTemp('voice-note-ui');
    addTearDown(() async {
      if (await directory.exists()) await directory.delete(recursive: true);
    });
    final file = File('${directory.path}/$name');
    await file.writeAsBytes(const [1, 2, 3]);
    return file;
  });
  return file!;
}

/// Whether [file] still exists, after letting the widget's own best-effort
/// deletion (real I/O too) run to completion.
Future<bool> recordingFileExists(WidgetTester tester, File file) async {
  // Each real I/O hop resumes inside the fake zone, so alternate real waits
  // with pumps until the exists-then-delete chain has had time to finish.
  for (var hop = 0; hop < 6; hop++) {
    await tester.runAsync(
      () => Future<void>.delayed(const Duration(milliseconds: 20)),
    );
    await tester.pump();
  }
  final exists = await tester.runAsync(file.exists);
  return exists!;
}
