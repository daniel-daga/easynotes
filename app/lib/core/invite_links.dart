import 'package:flutter/foundation.dart' show kIsWeb;

/// The invite format understood by the core, regardless of how it arrived.
/// Web links carry the same secret in the final fragment component.
String? inviteLinkFrom(String value) {
  final uri = Uri.tryParse(value.trim());
  if (uri == null) return null;
  if (uri.scheme == 'easynotes' && uri.host == 'join' && uri.fragment.isNotEmpty) {
    return 'easynotes://join#${uri.fragment}';
  }
  if ((uri.scheme == 'http' || uri.scheme == 'https') && uri.fragment.isNotEmpty) {
    final fragment = uri.fragment;
    final secret = fragment.substring(fragment.lastIndexOf('#') + 1);
    if (secret.isNotEmpty) return 'easynotes://join#$secret';
  }
  return null;
}

/// On web, make a link that opens this deployed app; native apps use the
/// registered custom scheme.
String inviteLinkForSharing(String link, {String? webOrigin}) {
  if (!kIsWeb) return link;
  final secret = inviteLinkFrom(link)?.split('#').last;
  final origin = webOrigin;
  if (secret == null || origin == null || origin.isEmpty) return link;
  final current = Uri.tryParse(origin);
  if (current == null) return link;
  return current.replace(fragment: 'join#$secret').toString();
}

