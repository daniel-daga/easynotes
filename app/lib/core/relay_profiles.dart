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
  final FlutterSecureStorage _storage;
  List<RelayProfile> _profiles = const [];
  String? _selectedId;

  List<RelayProfile> get profiles => List.unmodifiable(_profiles);
  RelayProfile get selected =>
      _profiles.singleWhere((p) => p.id == _selectedId);

  Future<void> load() async {
    final value = await _storage.read(key: _key);
    if (value != null) {
      final json = jsonDecode(value) as Map<String, dynamic>;
      _profiles = [
        for (final item in json['profiles'] as List<dynamic>)
          RelayProfile(id: item['id'] as String, url: item['url'] as String?),
      ];
      _selectedId = json['selected'] as String?;
    }
    if (_profiles.isEmpty || !_profiles.any((p) => p.id == _selectedId)) {
      final local = RelayProfile(id: _newId(), url: null);
      _profiles = [local];
      _selectedId = local.id;
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
      'version': 1,
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

String canonicalRelayUrl(String input) {
  final raw = input.trim();
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
