import 'dart:async';
import 'dart:convert';
import 'dart:typed_data';

import 'package:http/http.dart' as http;

/// How long a NIP-11 read may take before the request is aborted.
const relayInfoTimeout = Duration(seconds: 5);

/// Largest NIP-11 body that is read. A real document is a few KB; anything
/// past this is a hostile or broken relay and reads as unavailable rather
/// than being buffered whole and decoded in memory.
const relayInfoMaxBodyBytes = 256 * 1024;

/// Reads the relay's NIP-11 information document at [uri].
///
/// Returns the decoded JSON object, or `null` when the relay does not answer
/// within [timeout], answers non-2xx, sends more than [maxBodyBytes], or
/// sends anything other than a JSON object. The socket is aborted on timeout
/// and once the body limit is crossed, so a slow or oversized response
/// cannot hold the connection or memory open behind the caller's back.
Future<Map<String, dynamic>?> readRelayInfoDocument(
  http.Client client,
  Uri uri, {
  Duration timeout = relayInfoTimeout,
  int maxBodyBytes = relayInfoMaxBodyBytes,
}) async {
  final abort = Completer<void>();
  try {
    return await _readRelayInfoDocument(
      client,
      uri,
      abort.future,
      maxBodyBytes,
    ).timeout(timeout);
  } catch (_) {
    return null;
  } finally {
    if (!abort.isCompleted) abort.complete();
  }
}

Future<Map<String, dynamic>?> _readRelayInfoDocument(
  http.Client client,
  Uri uri,
  Future<void> abortTrigger,
  int maxBodyBytes,
) async {
  final request = http.AbortableRequest('GET', uri, abortTrigger: abortTrigger)
    ..headers['Accept'] = 'application/nostr+json';
  final response = await client.send(request);
  if (response.statusCode < 200 || response.statusCode >= 300) return null;
  final declaredLength = response.contentLength;
  if (declaredLength != null && declaredLength > maxBodyBytes) return null;

  final body = BytesBuilder(copy: false);
  await for (final chunk in response.stream) {
    body.add(chunk);
    if (body.length > maxBodyBytes) return null;
  }
  final document = jsonDecode(utf8.decode(body.takeBytes()));
  return document is Map<String, dynamic> ? document : null;
}
