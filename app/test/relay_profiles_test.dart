/// Relay profiles: what counts as one relay, and what is kept about each.
///
/// Every relay has a profile of its own and finds it again by its canonical
/// address, so the address rules are the isolation boundary: a relay spelt two
/// ways must not get two profiles, and two relays must never share one.
library;

import 'dart:convert';
import 'dart:io';

import 'package:easynotes/core/relay_profiles.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';

const _key = 'easynotes.relay-profiles';

void main() {
  group('canonicalRelayUrl', () {
    // The core's half of this is `core/tests/relay_address.rs`, over the same
    // file. `flutter test` runs from `app/`.
    final cases =
        (jsonDecode(
                  File(
                    '../core/tests/fixtures/relay_addresses.json',
                  ).readAsStringSync(),
                )
                as Map<String, dynamic>)['cases']
            as List<dynamic>;

    test('agrees with the core about every case', () {
      expect(cases, isNotEmpty);
      for (final item in cases.cast<Map<String, dynamic>>()) {
        final input = item['input'] as String;
        final expected = item['canonical'] as String?;
        String? actual;
        try {
          actual = canonicalRelayUrl(input);
        } on FormatException {
          actual = null;
        }
        expect(actual, expected, reason: jsonEncode(input));
      }
    });
  });

  group('RelayProfiles', () {
    setUp(() => FlutterSecureStorage.setMockInitialValues({}));

    Future<Map<String, dynamic>> stored() async =>
        jsonDecode((await const FlutterSecureStorage().read(key: _key))!)
            as Map<String, dynamic>;

    test('a fresh install starts on this device, and remembers it', () async {
      final profiles = RelayProfiles();
      await profiles.load();
      expect(profiles.profiles, hasLength(1));
      expect(profiles.selected.url, isNull);

      final again = RelayProfiles();
      await again.load();
      expect(again.selected.id, profiles.selected.id);
    });

    test('one relay spelt two ways is one profile', () async {
      final profiles = RelayProfiles();
      await profiles.load();
      final first = await profiles.prepare('wss://Relay.Example/');
      final second = await profiles.prepare('https://relay.example:443');
      expect(second.id, first.id);
      expect(first.url, 'https://relay.example');
      expect(profiles.profiles, hasLength(2));
    });

    test('different relays get different profiles', () async {
      final profiles = RelayProfiles();
      await profiles.load();
      final ids = {
        for (final url in [
          'https://relay.example',
          'http://relay.example',
          'https://relay.example:8443',
          'https://relay.example/team',
        ])
          (await profiles.prepare(url)).id,
      };
      expect(ids, hasLength(4));
      expect(ids, isNot(contains(profiles.selected.id)));
    });

    test('preparing a profile does not select it', () async {
      final profiles = RelayProfiles();
      await profiles.load();
      final local = profiles.selected;
      final target = await profiles.prepare('https://relay.example');
      expect(profiles.selected.id, local.id);
      expect((await stored())['selected'], local.id);

      // Only activating does, and it survives a restart.
      await profiles.activate(target);
      final again = RelayProfiles();
      await again.load();
      expect(again.selected.id, target.id);
      expect(again.selected.url, 'https://relay.example');
    });

    test('an address that is not a relay is refused before anything is '
        'kept', () async {
      final profiles = RelayProfiles();
      await profiles.load();
      await expectLater(
        profiles.prepare('https://relay.example?x=1'),
        throwsFormatException,
      );
      expect(profiles.profiles, hasLength(1));
    });

    test('a selection that names no profile keeps every profile', () async {
      // Every profile listed has a store and a key; a broken selection must
      // not make the app forget them and start empty.
      FlutterSecureStorage.setMockInitialValues({
        _key: jsonEncode({
          'version': 1,
          'selected': 'gone',
          'profiles': [
            {'id': 'relay', 'url': 'https://relay.example'},
            {'id': 'local', 'url': null},
          ],
        }),
      });
      final profiles = RelayProfiles();
      await profiles.load();
      expect(profiles.profiles.map((p) => p.id), ['relay', 'local']);
      expect(profiles.selected.id, 'local');
      expect((await stored())['selected'], 'local');
    });

    test(
      'settings from a newer build are refused and left as they are',
      () async {
        // ADR 0029 §5: downgrading is not supported, and rewriting the newer
        // layout in this one would lose whatever it added.
        final newer = jsonEncode({
          'version': RelayProfiles.version + 1,
          'selected': 'relay',
          'profiles': [
            {'id': 'relay', 'url': 'https://relay.example', 'added': 'later'},
          ],
        });
        FlutterSecureStorage.setMockInitialValues({_key: newer});
        await expectLater(RelayProfiles().load(), throwsStateError);
        expect(await const FlutterSecureStorage().read(key: _key), newer);
      },
    );

    test('settings saved before they had a version are version 1', () async {
      FlutterSecureStorage.setMockInitialValues({
        _key: jsonEncode({
          'selected': 'local',
          'profiles': [
            {'id': 'local', 'url': null},
          ],
        }),
      });
      final profiles = RelayProfiles();
      await profiles.load();
      expect(profiles.selected.id, 'local');
    });
  });
}
