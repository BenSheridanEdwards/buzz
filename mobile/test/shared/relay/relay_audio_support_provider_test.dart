import 'dart:async';

import 'package:buzz/shared/relay/relay_audio_support_provider.dart';
import 'package:buzz/shared/relay/relay_info_document.dart';
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

const _relay = 'https://relay.example.com';

/// Reads the verdict inside [fakeAsync], where `await` is unavailable.
bool? _readVerdict(FakeAsync async, ProviderContainer container) {
  bool? verdict;
  unawaited(
    container
        .read(relayAudioSupportProvider(_relay).future)
        .then((value) => verdict = value),
  );
  async.flushMicrotasks();
  return verdict;
}

void main() {
  test('reads the NIP-11 document over the relay HTTP URL', () async {
    late http.BaseRequest capturedRequest;
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

    // A second read for the same relay inside the TTL is served from cache.
    expect(
      await container.read(
        relayAudioSupportProvider('wss://relay.example.com').future,
      ),
      isTrue,
    );
    expect(requestCount, 1);
  });

  test('every verdict expires after five minutes, matching the desktop', () {
    for (final answered in ['["buzz-audio"]', '["nip-29"]']) {
      fakeAsync((async) {
        var requestCount = 0;
        final client = http_testing.MockClient((_) async {
          requestCount++;
          return http.Response('{"supported_extensions":$answered}', 200);
        });
        final container = _container(client);
        final expected = answered.contains('buzz-audio');

        expect(_readVerdict(async, container), expected, reason: answered);
        async.elapse(relayAudioSupportTtl - const Duration(seconds: 1));
        expect(_readVerdict(async, container), expected, reason: answered);
        expect(requestCount, 1, reason: 'cached inside the TTL');

        async.elapse(const Duration(seconds: 1));
        expect(requestCount, 1, reason: 'no read, so no refetch yet');
        expect(_readVerdict(async, container), expected, reason: answered);
        expect(requestCount, 2, reason: 'the TTL forces a fresh NIP-11 read');
      });
    }
  });

  test('an explicit invalidation drops a cached verdict immediately', () {
    fakeAsync((async) {
      var requestCount = 0;
      final client = http_testing.MockClient((_) async {
        requestCount++;
        return http.Response(
          '{"supported_extensions":[${requestCount == 1 ? '"buzz-audio"' : ''}]}',
          200,
        );
      });
      final container = _container(client);

      expect(_readVerdict(async, container), isTrue);
      container.invalidate(relayAudioSupportProvider(_relay));
      expect(_readVerdict(async, container), isFalse);
      expect(requestCount, 2);
    });
  });

  test('is false when the extension is absent', () async {
    final client = http_testing.MockClient(
      (_) async => http.Response('{"supported_extensions":["nip-29"]}', 200),
    );
    final container = _container(client);

    expect(
      await container.read(relayAudioSupportProvider(_relay).future),
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
        await container.read(relayAudioSupportProvider(_relay).future),
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
      await container.read(relayAudioSupportProvider(_relay).future),
      isFalse,
    );
    expect(
      await container.read(relayAudioSupportProvider('ftp://relay').future),
      isFalse,
    );
  });

  test('is false for a document larger than the NIP-11 body bound', () async {
    final padding = 'x' * (relayInfoMaxBodyBytes + 1);
    final client = http_testing.MockClient(
      (_) async => http.Response(
        '{"supported_extensions":["buzz-audio"],"description":"$padding"}',
        200,
      ),
    );
    final container = _container(client);

    expect(
      await container.read(relayAudioSupportProvider(_relay).future),
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
            .read(relayAudioSupportProvider(_relay).future)
            .then((value) => verdict = value),
      );

      async.elapse(const Duration(seconds: 4));
      expect(verdict, isNull);
      async.elapse(const Duration(seconds: 2));
      expect(verdict, isFalse);
      expect(requestCount, 1);

      // A failed verdict is retried after the TTL, not per send.
      expect(_readVerdict(async, container), isFalse);
      expect(requestCount, 1);

      async.elapse(relayAudioSupportTtl);
      expect(requestCount, 1, reason: 'no read, so no refetch yet');
      _readVerdict(async, container);
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
