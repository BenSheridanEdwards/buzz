import 'dart:async';
import 'dart:io';

import 'package:http/http.dart' as http;

/// Owns the reusable HTTP transport used for relay `/query` requests.
class RelayHttpQueryClient {
  RelayHttpQueryClient({
    http.Client? client,
    http.Client Function()? clientFactory,
  }) : _injectedClient = client,
       _clientFactory = clientFactory ?? (() => http.Client());

  final http.Client? _injectedClient;
  final http.Client Function() _clientFactory;
  _ClientGeneration? _currentGeneration;
  final Set<_ClientGeneration> _generations = {};

  Future<http.Response> post(
    Uri url, {
    required Map<String, String> headers,
    required List<int> body,
    required Duration timeout,
  }) async {
    final generation = _injectedClient == null
        ? (_currentGeneration ??= _createGeneration())
        : null;
    generation?.acquire();
    try {
      return await (_injectedClient ?? generation!.client)
          .post(url, headers: headers, body: body)
          .timeout(timeout);
    } on TimeoutException {
      _retire(generation);
      rethrow;
    } on IOException {
      // The transport itself failed, not the relay: a keep-alive connection
      // the OS killed while the app was paused fails like this on the first
      // request after resume. Handing the next query the same pool would fail
      // it too, which is how an open thread stayed stale until the user's own
      // next send happened to open a fresh connection.
      _retire(generation);
      rethrow;
    } on http.ClientException {
      // package:http wraps some socket failures in its own type.
      _retire(generation);
      rethrow;
    } finally {
      generation?.release();
    }
  }

  /// Stop handing out `generation`; it closes once its in-flight requests
  /// finish. A relay-level error (4xx/5xx) never reaches here: the socket is
  /// fine in that case and the pool is kept.
  void _retire(_ClientGeneration? generation) {
    if (identical(_currentGeneration, generation)) {
      _currentGeneration = null;
    }
    generation?.retire();
  }

  void close() {
    for (final generation in _generations.toList()) {
      generation.close();
    }
    _currentGeneration = null;
    _injectedClient?.close();
  }

  _ClientGeneration _createGeneration() {
    late final _ClientGeneration generation;
    generation = _ClientGeneration(
      _clientFactory(),
      onClosed: () => _generations.remove(generation),
    );
    _generations.add(generation);
    return generation;
  }
}

class _ClientGeneration {
  _ClientGeneration(this.client, {required this.onClosed});

  final http.Client client;
  final void Function() onClosed;
  int _activeRequests = 0;
  bool _retired = false;
  bool _closed = false;

  void acquire() {
    assert(!_closed);
    _activeRequests++;
  }

  void release() {
    assert(_activeRequests > 0);
    _activeRequests--;
    _closeIfIdle();
  }

  void retire() {
    _retired = true;
    _closeIfIdle();
  }

  void close() {
    if (_closed) return;
    _closed = true;
    client.close();
    onClosed();
  }

  void _closeIfIdle() {
    if (_retired && _activeRequests == 0) close();
  }
}
