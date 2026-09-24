import 'package:flutter/foundation.dart' show kIsWeb;

/// The invite format understood by the core, regardless of how it arrived.
/// Web links carry the same secret in the final fragment component.
String? inviteLinkFrom(String value) {
  final uri = Uri.tryParse(value.trim());
  if (uri == null) return null;
  if (uri.scheme == 'easynotes' &&
      uri.host == 'join' &&
      uri.fragment.isNotEmpty) {
    return 'easynotes://join#${uri.fragment}';
  }
  if ((uri.scheme == 'http' || uri.scheme == 'https') &&
      uri.fragment.isNotEmpty) {
    // `fragment` is still percent-encoded, and a `#` inside a fragment is not
    // a URI character, so the one in `#join#secret` arrives as `%23`.
    final fragment = Uri.decodeComponent(uri.fragment);
    final secret = fragment.substring(fragment.lastIndexOf('#') + 1);
    if (secret.isNotEmpty) return 'easynotes://join#$secret';
  }
  return null;
}

/// On web, make a link that opens this deployed app; native apps use the
/// registered custom scheme. [web] is for tests, which do not run on one.
String inviteLinkForSharing(
  String link, {
  String? webOrigin,
  bool web = kIsWeb,
}) {
  if (!web) return link;
  final secret = inviteLinkFrom(link)?.split('#').last;
  final origin = webOrigin;
  if (secret == null || origin == null || origin.isEmpty) return link;
  final current = Uri.tryParse(origin);
  if (current == null) return link;
  // Put together by hand: `Uri.replace` would encode the second `#` as `%23`,
  // and a link pasted rather than opened reaches the core as it is, which
  // reads the secret after the last `#`. Nor is the page's query carried over.
  final page = Uri(
    scheme: current.scheme,
    host: current.host,
    port: current.hasPort ? current.port : null,
    path: current.path,
  );
  return '$page#join#$secret';
}
