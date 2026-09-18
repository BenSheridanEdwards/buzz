import 'dart:async';
import 'dart:io';

import 'package:buzz/shared/relay/relay_http_query_client.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';

/// A pooled transport that has gone stale across a background: its first
/// request fails at the socket, exactly as a keep-alive connection the OS
/// killed while the app was paused.
MockClient _deadThenAlive(List<String> log, String name) {
  var calls = 0;
  return MockClient((request) async {
    calls++;
    log.add('$name#$calls');
    if (name == 'gen1' && calls == 1) {
      throw const SocketException('Connection reset by peer');
    }
    return http.Response('[]', 200);
  });
}

void main() {
  final url = Uri.parse('https://relay.test/query');
  const headers = {'Content-Type': 'application/json'};
  const timeout = Duration(seconds: 8);

  test(
    'a socket failure retires the pooled client so the next query is fresh',
    () async {
      final log = <String>[];
      var generation = 0;
      final client = RelayHttpQueryClient(
        clientFactory: () => _deadThenAlive(log, 'gen${++generation}'),
      );
      addTearDown(client.close);

      // First request after resume: the dead pool fails at the socket.
      await expectLater(
        client.post(url, headers: headers, body: const [], timeout: timeout),
        throwsA(isA<SocketException>()),
      );

      // The next request must not be handed the same dead transport.
      final response = await client.post(
        url,
        headers: headers,
        body: const [],
        timeout: timeout,
      );
      expect(response.statusCode, 200);
      expect(
        log,
        ['gen1#1', 'gen2#1'],
        reason: 'the second query must run on a new generation, not retry gen1',
      );
    },
  );

  test('a timeout still rotates, as before', () async {
    final log = <String>[];
    var generation = 0;
    final client = RelayHttpQueryClient(
      clientFactory: () {
        final name = 'gen${++generation}';
        return MockClient((request) async {
          log.add(name);
          if (name == 'gen1') {
            await Future<void>.delayed(const Duration(milliseconds: 50));
          }
          return http.Response('[]', 200);
        });
      },
    );
    addTearDown(client.close);

    await expectLater(
      client.post(
        url,
        headers: headers,
        body: const [],
        timeout: const Duration(milliseconds: 1),
      ),
      throwsA(isA<TimeoutException>()),
    );
    await client.post(url, headers: headers, body: const [], timeout: timeout);
    expect(log, ['gen1', 'gen2']);
  });

  test('a relay-level error does not rotate the transport', () async {
    // A 4xx/5xx is the relay answering; the socket is fine and the pool
    // must be kept, or every rate limit would churn connections.
    final log = <String>[];
    var generation = 0;
    final client = RelayHttpQueryClient(
      clientFactory: () {
        final name = 'gen${++generation}';
        return MockClient((request) async {
          log.add(name);
          return http.Response('{"error":"rate limited"}', 429);
        });
      },
    );
    addTearDown(client.close);

    await client.post(url, headers: headers, body: const [], timeout: timeout);
    await client.post(url, headers: headers, body: const [], timeout: timeout);
    expect(log, ['gen1', 'gen1']);
  });
}
