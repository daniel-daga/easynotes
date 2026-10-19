/// Relay addresses and the locally selected notes profile.
library;

import 'dart:convert';
import 'dart:math';

import 'package:flutter_secure_storage/flutter_secure_storage.dart';

class RelayProfile {
  const RelayProfile({required this.id, required this.url, this.access});

  final String id;
  final String? url;

  /// The relay's access key, base64url, if its operator gave this device one
  /// ([ADR 0028](../../../docs/adr/0028-who-may-write-to-a-relay.md)). Kept in
  /// the keychain beside the profile rather than in its settings: it is not
  /// part of the relay's address, and changing it is the same profile.
  final String? access;

  String get label => url ?? 'On this device';

  /// A link that gives someone this relay and its key at once, or null for a
  /// relay with no key, whose address is all anyone needs.
  String? get relayLink {
    final url = this.url, access = this.access;
    if (url == null || access == null) return null;
    return relayLinkFor(url, access);
  }

  RelayProfile withAccess(String? access) =>
      RelayProfile(id: id, url: url, access: access);
}

class RelayProfiles {
  RelayProfiles({FlutterSecureStorage? storage})
    : _storage = storage ?? const FlutterSecureStorage();

  static const _key = 'easynotes.relay-profiles';
  static const _onboardingKey = 'easynotes.onboarding';

  /// Where a profile's access key is kept: one keychain entry per profile, so
  /// that the settings above stay free of secrets and an older build that
  /// rewrites them cannot drop a key it does not know about.
  static String _accessKey(String profileId) =>
      'easynotes.relay-access.$profileId';

  /// The layout [_write] stores, as its `version` field says (ADR 0029 §5).
  /// One more than the number of [_steps].
  static const version = 1;

  /// Each step takes the stored settings one version on: `_steps[0]` from 1
  /// to 2, and so on. None yet. Never edit one a release has shipped; a
  /// change is a new step at the end, with a test that loads the old layout.
  static final List<Map<String, dynamic> Function(Map<String, dynamic>)>
  _steps = [];
  final FlutterSecureStorage _storage;
  List<RelayProfile> _profiles = const [];
  String? _selectedId;
  bool _shouldShowOnboarding = false;

  List<RelayProfile> get profiles => List.unmodifiable(_profiles);
  bool get shouldShowOnboarding => _shouldShowOnboarding;
  RelayProfile get selected =>
      _profiles.singleWhere((p) => p.id == _selectedId);

  Future<void> load() async {
    final value = await _storage.read(key: _key);
    final onboarding = await _storage.read(key: _onboardingKey);
    _shouldShowOnboarding =
        onboarding == 'pending' || (value == null && onboarding != 'complete');
    if (_shouldShowOnboarding && onboarding != 'pending') {
      await _storage.write(key: _onboardingKey, value: 'pending');
    }
    var upgraded = false;
    if (value != null) {
      var json = jsonDecode(value) as Map<String, dynamic>;
      final stored = json['version'] as int? ?? 1;
      if (stored > version) {
        // Downgrading is not supported, and writing this back in the older
        // layout would drop whatever the newer one added: every profile
        // listed has a store and a key, so losing one hides its notes.
        throw StateError(
          'Relay settings were saved by a newer version of EasyNotes '
          '(settings version $stored, this build understands $version). '
          'Update the app to open them.',
        );
      }
      for (final step in _steps.skip(stored - 1)) {
        json = step(json);
      }
      upgraded = stored < version;
      _profiles = [
        for (final item in json['profiles'] as List<dynamic>)
          RelayProfile(
            id: item['id'] as String,
            url: item['url'] as String?,
            access: await _storage.read(key: _accessKey(item['id'] as String)),
          ),
      ];
      _selectedId = json['selected'] as String?;
    }
    if (_profiles.isEmpty) {
      final local = RelayProfile(id: _newId(), url: null);
      _profiles = [local];
      _selectedId = local.id;
      await _save();
    } else if (upgraded && _profiles.any((p) => p.id == _selectedId)) {
      await _save();
    } else if (!_profiles.any((p) => p.id == _selectedId)) {
      // A selection that names no profile is repaired by choosing one that
      // exists, never by starting over: every profile listed here has a store
      // and a key, and forgetting the list would hide them all.
      _selectedId =
          (_profiles.where((p) => p.url == null).firstOrNull ?? _profiles.first)
              .id;
      await _save();
    }
  }

  /// Remember that this installation has chosen how to start. A pending first
  /// run survives an app restart before the person makes that choice.
  Future<void> completeOnboarding() async {
    await _storage.write(key: _onboardingKey, value: 'complete');
    _shouldShowOnboarding = false;
  }

  Future<RelayProfile> select(String? address) async {
    final target = await prepare(address);
    await activate(target);
    return target;
  }

  /// The profile for [address], made if there is none. [access], when
  /// given, becomes its access key; when not, it keeps the one it has.
  Future<RelayProfile> prepare(String? address, {String? access}) async {
    final url = address == null ? null : canonicalRelayUrl(address);
    var target =
        _profiles.where((p) => p.url == url).firstOrNull ??
        RelayProfile(id: _newId(), url: url);
    if (access != null && url != null) {
      target = await setAccess(target, access);
    }
    final next = [target, ..._profiles.where((p) => p.id != target.id)];
    if (!_profiles.any((p) => p.id == target.id)) {
      await _write(next, _selectedId!);
    }
    return target;
  }

  /// Keep [access] as [profile]'s access key, or forget its key when null.
  /// Returns the profile as it now is.
  Future<RelayProfile> setAccess(RelayProfile profile, String? access) async {
    final key = access == null ? null : normalizeAccessKey(access);
    if (key == null) {
      await _storage.delete(key: _accessKey(profile.id));
    } else {
      await _storage.write(key: _accessKey(profile.id), value: key);
    }
    final updated = profile.withAccess(key);
    _profiles = [
      for (final p in _profiles)
        if (p.id == profile.id) updated else p,
    ];
    return updated;
  }

  Future<void> activate(RelayProfile profile) => _write(_profiles, profile.id);

  /// Give the selected local collection its first relay, preserving its store
  /// ID and keys. The single settings write is the move: before it succeeds
  /// the collection is local; afterwards it belongs to this relay. A fresh
  /// local profile keeps "On this device" separate, with no copied identities.
  /// Existing relay collections are never replaced or merged.
  Future<RelayProfile> moveLocalToRelay(String address, {String? access}) async {
    final local = selected;
    final url = canonicalRelayUrl(address);
    if (local.url != null || _profiles.any((p) => p.url == url)) {
      throw StateError('Only a local collection can move to a new relay.');
    }
    final key = access == null ? null : normalizeAccessKey(access);
    if (access != null && key == null) {
      throw const FormatException('That is not an access key.');
    }
    // Save the key first without mutating the in-memory profile list. If
    // settings refuse the move, both the live and stored selection stay local.
    if (key != null) {
      await _storage.write(key: _accessKey(local.id), value: key);
    } else {
      await _storage.delete(key: _accessKey(local.id));
    }
    final target = RelayProfile(id: local.id, url: url, access: key);
    await _write([
      for (final profile in _profiles)
        if (profile.id == local.id) target else profile,
      RelayProfile(id: _newId(), url: null),
    ], target.id);
    return target;
  }

  Future<void> _save() => _write(_profiles, _selectedId!);

  Future<void> _write(List<RelayProfile> profiles, String selected) async {
    final value = jsonEncode({
      'version': version,
      'selected': selected,
      'profiles': [
        for (final profile in profiles) {'id': profile.id, 'url': profile.url},
      ],
    });
    await _storage.write(key: _key, value: value);
    _profiles = profiles;
    _selectedId = selected;
  }

  static String _newId() {
    final random = Random.secure();
    return base64UrlEncode(
      List<int>.generate(18, (_) => random.nextInt(256)),
    ).replaceAll('=', '');
  }
}

/// What a relay link carries its access key in: `https://relay.example/#access=…`.
/// The fragment, because a browser never sends it to a server, so the link
/// can be pasted into an address bar or a chat without the key landing in
/// anyone's access log.
const _accessFragment = 'access=';

/// A relay link for [url] and [access]: what an operator hands out, and what
/// a person passes on to someone they share a list with.
String relayLinkFor(String url, String access) =>
    '${url.replaceFirst(RegExp(r'/+$'), '')}/#$_accessFragment$access';

/// A relay address or link, read apart: the canonical address, and the access
/// key if the link carried one. Throws [FormatException] for anything that is
/// neither, and for a link whose key is not a key.
({String url, String? access}) parseRelayLink(String input) {
  final raw = input.trim();
  final hash = raw.indexOf('#');
  if (hash < 0) return (url: canonicalRelayUrl(raw), access: null);
  final fragment = raw.substring(hash + 1);
  if (!fragment.startsWith(_accessFragment)) {
    throw FormatException('Enter a relay URL such as https://relay.example.');
  }
  final access = normalizeAccessKey(
    Uri.decodeComponent(fragment.substring(_accessFragment.length)),
  );
  if (access == null) {
    throw FormatException('The access key in that link is not a valid key.');
  }
  return (url: canonicalRelayUrl(raw.substring(0, hash)), access: access);
}

/// [input] as the core reads an access key — 32 bytes, base64url with no
/// padding — or null if it is not one.
String? normalizeAccessKey(String input) {
  final raw = input.trim().replaceAll('=', '');
  if (!RegExp(r'^[A-Za-z0-9_-]{43}$').hasMatch(raw)) return null;
  try {
    final bytes = base64Url.decode('$raw=');
    return bytes.length == 32 ? raw : null;
  } on FormatException {
    return null;
  }
}

/// The address a relay's profile is stored and found under.
///
/// Two spellings of one relay give the same address and two relays never do:
/// the scheme and host are lowercased, `ws` and `wss` become `http` and
/// `https`, a default port and trailing slashes are dropped, and the rest of
/// the path is kept. What `Uri` would quietly rewrite — spaces, `%` escapes,
/// `.` and `..` segments — is refused instead, as are credentials, a query and
/// a fragment. The core's `RelayEndpoint::parse` must agree with this exactly,
/// and both are held to `core/tests/fixtures/relay_addresses.json`.
String canonicalRelayUrl(String input) {
  final raw = input.trim();
  final afterScheme = raw.indexOf('://');
  final rawPath = afterScheme < 0
      ? ''
      : raw.substring(afterScheme + 3).replaceFirst(RegExp(r'^[^/]*'), '');
  if (raw.contains(RegExp(r'[\s%]')) ||
      rawPath.split('/').any((segment) => segment == '.' || segment == '..')) {
    throw FormatException('Enter a relay URL such as https://relay.example.');
  }
  final uri = Uri.tryParse(raw);
  if (uri == null ||
      !const {
        'http',
        'https',
        'ws',
        'wss',
      }.contains(uri.scheme.toLowerCase()) ||
      uri.host.isEmpty ||
      uri.userInfo.isNotEmpty ||
      uri.hasQuery ||
      uri.hasFragment) {
    throw FormatException('Enter a relay URL such as https://relay.example.');
  }
  final scheme = switch (uri.scheme.toLowerCase()) {
    'ws' => 'http',
    'wss' => 'https',
    final value => value,
  };
  final port = uri.hasPort ? uri.port : null;
  if (port != null && (port < 1 || port > 65535)) {
    throw FormatException('The relay URL has an invalid port.');
  }
  final normalizedPort =
      (scheme == 'http' && port == 80) || (scheme == 'https' && port == 443)
      ? null
      : port;
  final path = uri.path.replaceFirst(RegExp(r'/+$'), '');
  return Uri(
    scheme: scheme,
    host: uri.host.toLowerCase(),
    port: normalizedPort,
    path: path,
  ).toString();
}

