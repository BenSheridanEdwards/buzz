import 'dart:async';
import 'dart:convert';

import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:http/http.dart' as http;

import 'relay_info_uri.dart';

/// NIP-11 `supported_extensions` entry a relay advertises when it accepts
/// metadata-free `audio/mpeg` and `audio/mp4` uploads and serves them inline.
const relayAudioExtension = 'buzz-audio';

/// How long a failed NIP-11 read is trusted before the relay is asked again.
///
/// A relay that answered is cached for the session; only failures (timeout,
/// non-2xx, malformed document) are retried, so a transient outage does not
/// pin the session to the MP4 envelope. Mirrors the desktop's bounded cache.
const relayAudioSupportRetryDelay = Duration(minutes: 5);

/// Supplies the HTTP client used for NIP-11 audio capability reads.
///
/// Tests can override this provider to return deterministic relay responses.
final relayAudioSupportHttpClientProvider = Provider<http.Client>((ref) {
  final client = http.Client();
  ref.onDispose(client.close);
  return client;
});

/// Whether the relay at the given URL advertises [relayAudioExtension].
///
/// Reads the relay's public NIP-11 document once per relay URL per session.
/// Any fetch or parse failure resolves to `false` so callers fall back to the
/// MP4 voice-note envelope every relay accepts; the failed verdict is retried
/// after [relayAudioSupportRetryDelay].
final relayAudioSupportProvider = FutureProvider.family<bool, String>((
  ref,
  relayUrl,
) async {
  final uri = relayInfoUri(relayUrl);
  if (uri == null) return false;

  final verdict = await _readRelayAudioSupport(
    ref.read(relayAudioSupportHttpClientProvider),
    uri,
  );
  if (verdict == null) {
    final retry = Timer(relayAudioSupportRetryDelay, ref.invalidateSelf);
    ref.onDispose(retry.cancel);
    return false;
  }
  return verdict;
});

/// Returns the relay's verdict, or `null` when it could not be read.
Future<bool?> _readRelayAudioSupport(http.Client client, Uri uri) async {
  try {
    final response = await client
        .get(uri, headers: const {'Accept': 'application/nostr+json'})
        .timeout(const Duration(seconds: 5));
    if (response.statusCode < 200 || response.statusCode >= 300) {
      return null;
    }
    final document = jsonDecode(response.body);
    if (document is! Map<String, dynamic>) return null;
    return relayInfoAdvertisesAudio(document);
  } catch (_) {
    return null;
  }
}

/// Whether a decoded NIP-11 document lists [relayAudioExtension] in
/// `supported_extensions`. A missing or malformed list reads as `false`.
bool relayInfoAdvertisesAudio(Map<String, dynamic> document) {
  final extensions = document['supported_extensions'];
  if (extensions is! List) return false;
  return extensions.any((extension) => extension == relayAudioExtension);
}
