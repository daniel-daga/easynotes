/// Relay addresses and the locally selected notes profile.
library;

import 'dart:convert';
import 'dart:math';

import 'package:flutter_secure_storage/flutter_secure_storage.dart';

class RelayProfile {
  const RelayProfile({required this.id, required this.url});

  final String id;
  final String? url;

  String get label => url ?? 'On this device';
}

class RelayProfiles {
  RelayProfiles({FlutterSecureStorage? storage})
    : _storage = storage ?? const FlutterSecureStorage();

  static const _key = 'easynotes.relay-profiles';

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

  List<RelayProfile> get profiles => List.unmodifiable(_profiles);
  RelayProfile get selected =>
      _profiles.singleWhere((p) => p.id == _selectedId);

  Future<void> load() async {
    final value = await _storage.read(key: _key);
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
          RelayProfile(id: item['id'] as String, url: item['url'] as String?),
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

  Future<RelayProfile> select(String? address) async {
    final target = await prepare(address);
    await activate(target);
    return target;
  }

  Future<RelayProfile> prepare(String? address) async {
    final url = address == null ? null : canonicalRelayUrl(address);
    final target =
        _profiles.where((p) => p.url == url).firstOrNull ??
        RelayProfile(id: _newId(), url: url);
    final next = [target, ..._profiles.where((p) => p.id != target.id)];
    if (!_profiles.any((p) => p.id == target.id)) {
      await _write(next, _selectedId!);
    }
    return target;
  }

  Future<void> activate(RelayProfile profile) => _write(_profiles, profile.id);

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
