import 'package:hooks_riverpod/hooks_riverpod.dart';
import 'package:http/http.dart' as http;

import '../relay/relay_info_document.dart';
import '../relay/relay_info_uri.dart';

/// Supplies the HTTP client used for NIP-11 community icon lookups.
///
/// Tests can override this provider to return deterministic relay responses.
final communityIconHttpClientProvider = Provider<http.Client>((ref) {
  final client = http.Client();
  ref.onDispose(client.close);
  return client;
});

/// Reads a community's icon from its public NIP-11 relay information document.
///
/// The lookup does not depend on the active relay session, so icons can render
/// for every paired community in the switcher. Callers explicitly invalidate
/// this family when opening the switcher so transient failures and relay
/// metadata updates can be retried without background polling.
final communityIconProvider = FutureProvider.autoDispose
    .family<String?, String>((ref, relayUrl) async {
      final uri = relayInfoUri(relayUrl);
      if (uri == null) return null;

      final document = await readRelayInfoDocument(
        ref.read(communityIconHttpClientProvider),
        uri,
      );
      if (document == null) return null;

      final icon = document['icon'];
      if (icon is! String || icon.trim().isEmpty) return null;
      return icon.trim();
    });
