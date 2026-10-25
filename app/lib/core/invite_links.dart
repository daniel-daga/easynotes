// The invite format understood by the core, regardless of how it arrived. Web
// links carry the same secret in the final fragment component, and the relay
// they belong to before it.
//
// A link names one note and one sync server, so a person who taps it needs
// nothing typed in first. Everything before the last `#` is optional to the
// core, which reads only what follows it ([core/src/invite.rs]); that is what
// keeps a link made here readable by a build that predates the relay part.
//
// `easynotes://join#[relay=<url>;access=<key>#]<secret>`

import 'package:flutter/foundation.dart' show kIsWeb;

/// What a link is made of before its secret: the scheme and the separator.
const _scheme = 'easynotes://join#';

/// The separator between the relay parameters and the secret.
const _paramsEnd = '#';

/// The one parameter that is not optional, when there is one to name.
const _relayParam = 'relay=';
const _accessParam = 'access=';

/// Fields inside the parameter block.
const _fieldSeparator = ';';

/// What a link says besides its secret: the relay the note lives on, and the
/// key that relay demands, when it demands one ([ADR 0028]).
typedef InviteRelay = ({String url, String? access});

/// The invite, in the one form the core understands:
/// `easynotes://join#[relay=…;access=…#]secret`.
///
/// Returns null for anything that is not an invite: a note's web address, a
/// share blob, a message someone retyped by hand.
String? inviteLinkFrom(String value) {
  final uri = Uri.tryParse(value.trim());
  if (uri == null || uri.fragment.isEmpty) return null;
  final native = uri.scheme == 'easynotes' && uri.host == 'join';
  final web = uri.scheme == 'http' || uri.scheme == 'https';
  if (!native && !web) return null;

  final fragment = _decode(uri.fragment);
  if (fragment.isEmpty) return null;
  // A web address with a fragment is not an invite unless it says so: a page
  // that routes with `#/notes` would otherwise look like one on every load.
  if (web && !fragment.startsWith('join#')) return null;
  final at = fragment.lastIndexOf(_paramsEnd);
  final secret = at < 0 ? fragment : fragment.substring(at + 1);
  if (secret.isEmpty) return null;
  final head = _withoutJoinMarker(at < 0 ? '' : fragment.substring(0, at));
  return head.isEmpty ? '$_scheme$secret' : '$_scheme$head$_paramsEnd$secret';
}

/// The relay a link names, or null for one that names none — every link made
/// before invites carried a relay, and a link pasted as a bare code.
InviteRelay? relayFromInvite(String link) {
  final normalized = inviteLinkFrom(link);
  if (normalized == null) return null;
  final at = normalized.lastIndexOf(_paramsEnd);
  if (at < _scheme.length) return null;
  final head = normalized.substring(_scheme.length, at);
  if (head.isEmpty) return null;
  String? url;
  String? access;
  for (final field in head.split(_fieldSeparator)) {
    if (field.startsWith(_relayParam)) {
      url = field.substring(_relayParam.length);
    } else if (field.startsWith(_accessParam)) {
      access = field.substring(_accessParam.length);
    }
  }
  if (url == null || url.isEmpty) return null;
  return (url: url, access: access == null || access.isEmpty ? null : access);
}

/// [link] with [relay] written into it, or unchanged when it already names one
/// or is not a link at all.
String inviteLinkWithRelay(String link, InviteRelay? relay) {
  final normalized = inviteLinkFrom(link);
  if (normalized == null || relay == null) return normalized ?? link;
  final at = normalized.lastIndexOf(_paramsEnd);
  final secret = normalized.substring(at + 1);
  final access = relay.access;
  final params = access == null
      ? '$_relayParam${relay.url}'
      : '$_relayParam${relay.url}$_fieldSeparator$_accessParam$access';
  return '$_scheme$params$_paramsEnd$secret';
}

/// Make the link a person can tap. On web it opens this deployed app; on
/// native it is the custom scheme by default, unless the caller asks for the
/// https form ([web]) that a messenger will linkify. The relay serves that
/// form's page at its own root, so a native share can name the relay's origin.
/// [web] defaults to [kIsWeb]; a test or a native caller can pass it.
String inviteLinkForSharing(
  String link, {
  String? webOrigin,
  InviteRelay? relay,
  bool web = kIsWeb,
}) {
  final native = inviteLinkWithRelay(link, relay);
  if (!web) return native;
  final normalized = inviteLinkFrom(native);
  if (normalized == null) return native;
  final at = normalized.lastIndexOf(_paramsEnd);
  final secret = normalized.substring(at + 1);
  final head = at > _scheme.length
      ? normalized.substring(_scheme.length, at)
      : '';
  final origin = webOrigin;
  if (origin == null || origin.isEmpty) return native;
  final current = Uri.tryParse(origin);
  if (current == null) return native;
  // Put together by hand: `Uri.replace` would encode the second `#` as `%23`,
  // and a link pasted rather than opened reaches the core as it is, which
  // reads the secret after the last `#`. Nor is the page's query carried over.
  final page = Uri(
    scheme: current.scheme,
    host: current.host,
    port: current.hasPort ? current.port : null,
    path: current.path.isEmpty ? '/' : current.path,
  );
  // The parameter block is encoded so that the shared message holds exactly
  // one `http(s)://`: a messenger that linkifies only those schemes otherwise
  // turns the relay address inside the fragment into a second, dead link
  // (Android did, and the invite part was not a link at all). `inviteLinkFrom`
  // decodes the fragment, so the core still reads the same fields.
  return head.isEmpty
      ? '$page#join$_paramsEnd$secret'
      : '$page#join$_paramsEnd${Uri.encodeComponent(head)}$_paramsEnd$secret';
}

/// Percent-decode a fragment, or hand it back as it came when it is not
/// well-formed: a pasted link is a person's typing, not a promise.
String _decode(String fragment) {
  try {
    return Uri.decodeComponent(fragment);
  } on FormatException {
    return fragment;
  }
}

/// Web links carry a `join` marker before their parameters; native ones do not.
String _withoutJoinMarker(String head) {
  if (head == 'join') return '';
  const marker = 'join#';
  if (head.startsWith(marker)) return head.substring(marker.length);
  return head;
}
