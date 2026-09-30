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

  group('relay links', () {
    final key = base64Url.encode(List<int>.filled(32, 1)).replaceAll('=', '');

    test('an address alone has no key', () {
      final read = parseRelayLink('wss://Relay.Example/');
      expect(read.url, 'https://relay.example');
      expect(read.access, isNull);
    });

    test('a link carries the key in its fragment, and round-trips', () {
      final link = relayLinkFor('https://relay.example', key);
      expect(link, 'https://relay.example/#access=$key');
      final read = parseRelayLink(link);
      expect(read.url, 'https://relay.example');
      expect(read.access, key);
    });

    test('a link with anything else after the # is not a relay link', () {
      expect(
        () => parseRelayLink('https://relay.example/#join#abc'),
        throwsFormatException,
      );
      expect(
        () => parseRelayLink('https://relay.example/#access=short'),
        throwsFormatException,
      );
    });

    test('a key is 32 bytes of base64url, padding forgiven', () {
      expect(normalizeAccessKey(' $key= '), key);
      expect(normalizeAccessKey(key.substring(1)), isNull);
      expect(normalizeAccessKey('${key.substring(1)}!'), isNull);
    });
  });

  group('RelayProfiles', () {
    setUp(() => FlutterSecureStorage.setMockInitialValues({}));

    Future<Map<String, dynamic>> stored() async =>
        jsonDecode((await const FlutterSecureStorage().read(key: _key))!)
            as Map<String, dynamic>;

    test("a relay's access key is kept apart from the settings, per "
        "profile", () async {
      final key = base64Url.encode(List<int>.filled(32, 2)).replaceAll('=', '');
      final profiles = RelayProfiles();
      await profiles.load();
      final relay = await profiles.prepare(
        'https://relay.example',
        access: key,
      );
      expect(relay.access, key);
      expect(relay.relayLink, 'https://relay.example/#access=$key');
      // Not in the settings, which an older build could rewrite without it.
      expect(jsonEncode(await stored()), isNot(contains(key)));

      final again = RelayProfiles();
      await again.load();
      final found = again.profiles.singleWhere((p) => p.id == relay.id);
      expect(found.access, key);

      final forgotten = await again.setAccess(found, null);
      expect(forgotten.access, isNull);
      final third = RelayProfiles();
      await third.load();
      expect(
        third.profiles.singleWhere((p) => p.id == relay.id).access,
        isNull,
      );
    });

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
