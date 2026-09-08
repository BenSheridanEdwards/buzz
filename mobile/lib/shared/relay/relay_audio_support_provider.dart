import 'dart:async';

import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:http/http.dart' as http;

import 'relay_info_document.dart';
import 'relay_info_uri.dart';

/// NIP-11 `supported_extensions` entry a relay advertises when it accepts
/// metadata-free `audio/mpeg` and `audio/mp4` uploads and serves them inline.
const relayAudioExtension = 'buzz-audio';

/// How long any verdict (supported, unsupported, or unreadable) is trusted
/// before the relay's NIP-11 document is read again.
///
/// Matches the desktop's `AUDIO_CAPABILITY_TTL`, so an operator toggling the
/// extension takes effect on both clients within five minutes without a
/// restart. A relay that rejects an `audio/mp4` upload is invalidated at once
/// by the sender (see `MediaUploadService.uploadVoiceNote`).
const relayAudioSupportTtl = Duration(minutes: 5);

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
/// Reads the relay's public NIP-11 document at most once per relay URL per
/// [relayAudioSupportTtl]. Any fetch or parse failure resolves to `false` so
/// callers fall back to the MP4 voice-note envelope every relay accepts.
final relayAudioSupportProvider = FutureProvider.family<bool, String>((
  ref,
  relayUrl,
) async {
  final uri = relayInfoUri(relayUrl);
  if (uri == null) return false;

  final expiry = Timer(relayAudioSupportTtl, ref.invalidateSelf);
  ref.onDispose(expiry.cancel);
  final document = await readRelayInfoDocument(
    ref.read(relayAudioSupportHttpClientProvider),
    uri,
  );
  return document != null && relayInfoAdvertisesAudio(document);
});

/// Whether a decoded NIP-11 document lists [relayAudioExtension] in
/// `supported_extensions`. A missing or malformed list reads as `false`.
bool relayInfoAdvertisesAudio(Map<String, dynamic> document) {
  final extensions = document['supported_extensions'];
  if (extensions is! List) return false;
  return extensions.any((extension) => extension == relayAudioExtension);
}
