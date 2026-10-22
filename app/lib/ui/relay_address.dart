/// The two reasons this build cannot reach a relay, and the checks a relay
/// address and access key have to pass before either is tried.
///
/// First-run setup and Settings ask for the same address and key, and refuse
/// them for the same reasons; keeping the rules in one place means an address
/// rejected on one path cannot be accepted on the other.
library;

import 'package:flutter/foundation.dart'
    show TargetPlatform, defaultTargetPlatform, kIsWeb, kReleaseMode;

import '../core/relay_profiles.dart';

/// Whether the page this app was served from is HTTPS, in which case a browser
/// will refuse it a plain `http://` or `ws://` relay (mixed content).
bool get platformServedOverHttps => kIsWeb && Uri.base.scheme == 'https';

/// Whether this is a release build for a phone, whose operating system refuses
/// plain `http://` and `ws://` outright: Android since API 28 unless the
/// manifest allows it (only the debug one does), iOS through App Transport
/// Security. Debug builds keep plain http for a relay on the local network.
bool get platformMobileRelease =>
    kReleaseMode &&
    !kIsWeb &&
    (defaultTargetPlatform == TargetPlatform.android ||
        defaultTargetPlatform == TargetPlatform.iOS);

/// Why [input] cannot be a relay this build can reach, or null if it can.
String? relayAddressError(
  String? input, {
  required bool servedOverHttps,
  required bool mobileRelease,
}) {
  final String url;
  try {
    url = parseRelayLink(input ?? '').url;
  } on FormatException catch (error) {
    return error.message;
  }
  if (servedOverHttps && url.startsWith('http://')) {
    return 'This page is served over HTTPS, so the browser will only reach '
        'a sync server whose address starts with https://.';
  }
  if (mobileRelease && url.startsWith('http://')) {
    return 'Notes only sync over an encrypted connection. Use a sync '
        'server address that starts with https:// or wss://.';
  }
  return null;
}

/// The access key in [input] — a key on its own, or a relay link carrying one
/// for [relay] — or why it is not one.
///
/// An empty input is no key rather than an error, for a relay anyone may use.
/// A link for another relay is an error: its key would be handed to the wrong
/// one.
({String? key, String? error}) readAccessKey(
  String input, {
  required String? relay,
}) {
  final raw = input.trim();
  if (raw.isEmpty) return (key: null, error: null);
  final plain = normalizeAccessKey(raw);
  if (plain != null) return (key: plain, error: null);
  try {
    final link = parseRelayLink(raw);
    if (link.access == null) {
      return (key: null, error: 'That link has no access key in it.');
    }
    if (relay != null && link.url != relay) {
      return (
        key: null,
        error:
            'That link is for another sync server: '
            '${link.url}.',
      );
    }
    return (key: link.access, error: null);
  } on FormatException {
    return (key: null, error: 'That is not an access key or server link.');
  }
}
