/// Resolves the HTTP(S) URL that serves a relay's NIP-11 information document.
///
/// Communities persist either an `https://` or a `wss://` relay URL depending
/// on how they were joined; NIP-11 is always served over HTTP on the same
/// origin. Returns `null` for anything that is not a websocket or HTTP URL.
Uri? relayInfoUri(String relayUrl) {
  try {
    final uri = Uri.parse(relayUrl.trim());
    final scheme = switch (uri.scheme) {
      'wss' => 'https',
      'ws' => 'http',
      'https' || 'http' => uri.scheme,
      _ => null,
    };
    return scheme == null ? null : uri.replace(scheme: scheme);
  } on FormatException {
    return null;
  }
}
