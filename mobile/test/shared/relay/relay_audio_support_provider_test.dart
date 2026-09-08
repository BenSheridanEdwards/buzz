import 'dart:async';

import 'package:buzz/shared/relay/relay_audio_support_provider.dart';
import 'package:fake_async/fake_async.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart' as http_testing;

ProviderContainer _container(http.Client client) {
  final container = ProviderContainer(
    overrides: [relayAudioSupportHttpClientProvider.overrideWithValue(client)],
  );
  addTearDown(container.dispose);
  return container;
}

void main() {
  test('reads the NIP-11 document over the relay HTTP URL', () async {
    late http.Request capturedRequest;
    var requestCount = 0;
    final client = http_testing.MockClient((request) async {
      capturedRequest = request;
      requestCount++;
      return http.Response(
        '{"name":"buzz","supported_extensions":["nip-29","buzz-audio"]}',
        200,
      );
    });
    final container = _container(client);

    final supported = await container.read(
      relayAudioSupportProvider('wss://relay.example.com').future,
    );

    expect(supported, isTrue);
    expect(capturedRequest.url, Uri.parse('https://relay.example.com'));
    expect(capturedRequest.headers['Accept'], 'application/nostr+json');

    // A second read for the same relay is served from the session cache.
    expect(
      await container.read(
        relayAudioSupportProvider('wss://relay.example.com').future,
      ),
      isTrue,
    );
    expect(requestCount, 1);
  });

  test('is false when the extension is absent', () async {
    final client = http_testing.MockClient(
      (_) async => http.Response('{"supported_extensions":["nip-29"]}', 200),
    );
    final container = _container(client);

    expect(
      await container.read(
        relayAudioSupportProvider('https://relay.example.com').future,
      ),
      isFalse,
    );
  });

  test('is false for malformed documents', () async {
    for (final body in [
      'not json',
      '[]',
      '{"supported_extensions":"buzz-audio"}',
      '{"supported_extensions":[{"name":"buzz-audio"}]}',
      '{}',
    ]) {
      final client = http_testing.MockClient(
        (_) async => http.Response(body, 200),
      );
      final container = _container(client);
      expect(
        await container.read(
          relayAudioSupportProvider('https://relay.example.com').future,
        ),
        isFalse,
        reason: body,
      );
    }
  });

  test('is false for non-2xx responses and unusable relay URLs', () async {
    final client = http_testing.MockClient(
      (_) async => http.Response('unavailable', 503),
    );
    final container = _container(client);

    expect(
      await container.read(
        relayAudioSupportProvider('https://relay.example.com').future,
      ),
      isFalse,
    );
    expect(
      await container.read(relayAudioSupportProvider('ftp://relay').future),
      isFalse,
    );
  });

  test('is false when the relay does not answer within five seconds', () {
    fakeAsync((async) {
      var requestCount = 0;
      final client = http_testing.MockClient((_) {
        requestCount++;
        return Completer<http.Response>().future;
      });
      final container = _container(client);
      bool? verdict;
      unawaited(
        container
            .read(relayAudioSupportProvider('https://relay.example.com').future)
            .then((value) => verdict = value),
      );

      async.elapse(const Duration(seconds: 4));
      expect(verdict, isNull);
      async.elapse(const Duration(seconds: 2));
      expect(verdict, isFalse);
      expect(requestCount, 1);

      // A failed verdict is retried after the bounded delay, not per send.
      verdict = null;
      unawaited(
        container
            .read(relayAudioSupportProvider('https://relay.example.com').future)
            .then((value) => verdict = value),
      );
      async.flushMicrotasks();
      expect(verdict, isFalse);
      expect(requestCount, 1);

      async.elapse(relayAudioSupportRetryDelay);
      async.flushMicrotasks();
      expect(requestCount, 1, reason: 'no read, so no refetch yet');
      unawaited(
        container
            .read(relayAudioSupportProvider('https://relay.example.com').future)
            .then((value) => verdict = value),
      );
      async.flushMicrotasks();
      expect(requestCount, 2);
    });
  });

  test('relayInfoAdvertisesAudio matches the extension exactly', () {
    expect(
      relayInfoAdvertisesAudio({
        'supported_extensions': ['buzz-audio'],
      }),
      isTrue,
    );
    expect(
      relayInfoAdvertisesAudio({
        'supported_extensions': ['buzz-audio-v2', 'BUZZ-AUDIO'],
      }),
      isFalse,
    );
    expect(relayInfoAdvertisesAudio({'supported_extensions': null}), isFalse);
    expect(relayInfoAdvertisesAudio(const {}), isFalse);
  });
}
