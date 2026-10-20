/// Settings → Relay: choosing a relay, and being told what that does first.
library;

import 'dart:convert';

import 'package:easynotes/core/relay_profiles.dart';
import 'package:easynotes/ui/relay_settings_screen.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

const _local = RelayProfile(id: 'local', url: null);
const _relay = RelayProfile(id: 'relay', url: 'https://relay.example');

void main() {
  late List<String?> switched;
  late List<String?> keys;
  late List<String> moved;
  Object? failWith;

  setUp(() {
    switched = [];
    keys = [];
    moved = [];
    failWith = null;
  });

  Future<void> onSetAccess(String? access) async => keys.add(access);

  Future<void> onSwitch(String? url, {String? access}) async {
    switched.add(access == null ? url : '$url with $access');
    if (failWith != null) throw failWith!;
  }

  /// The screen pushed over a home route, so that a successful switch has
  /// somewhere to pop back to.
  Future<void> pump(
    WidgetTester tester, {
    RelayProfile profile = _local,
    List<RelayProfile> profiles = const [_local, _relay],
    bool servedOverHttps = false,
    bool mobileRelease = false,
    bool offerMove = false,
  }) async {
    await tester.pumpWidget(
      MaterialApp(
        home: Builder(
          builder: (context) => TextButton(
            onPressed: () => Navigator.of(context).push(
              MaterialPageRoute<void>(
                builder: (_) => RelaySettingsScreen(
                  profile: profile,
                  profiles: profiles,
                  onSwitchRelay: onSwitch,
                  onSetAccess: onSetAccess,
                  onMoveLocalToRelay: offerMove ? (url, {access}) async {
                    moved.add(url);
                    if (failWith != null) throw failWith!;
                  } : null,
                  servedOverHttps: servedOverHttps,
                  mobileRelease: mobileRelease,
                ),
              ),
            ),
            child: const Text('open'),
          ),
        ),
      ),
    );
    await tester.tap(find.text('open'));
    await tester.pumpAndSettle();
  }

  Future<void> submit(WidgetTester tester, String url) async {
    await tester.enterText(find.byKey(const Key('relay-url-field')), url);
    final button = find.byKey(const Key('switch-relay-button'));
    await tester.ensureVisible(button);
    await tester.pumpAndSettle();
    await tester.tap(button);
    await tester.pumpAndSettle();
  }

  testWidgets('a new relay is confirmed as starting empty, then switched to '
      'in its canonical form', (tester) async {
    await pump(tester);
    await submit(tester, 'wss://New.Example/');
    expect(find.text('Switch relay?'), findsOneWidget);
    expect(find.textContaining('This profile starts empty'), findsOneWidget);
    expect(find.textContaining('Your current notes stay here'), findsOneWidget);

    await tester.tap(find.text('Switch'));
    await tester.pumpAndSettle();
    expect(switched, ['https://new.example']);
    // Back where Settings was opened from.
    expect(find.byType(RelaySettingsScreen), findsNothing);
  });

  testWidgets('a relay with a profile already says its notes will open', (
    tester,
  ) async {
    await pump(tester);
    await submit(tester, 'https://relay.example:443');
    expect(find.textContaining('Its saved notes will open'), findsOneWidget);
  });

  testWidgets('first relay setup can move the current local notes', (tester) async {
    await pump(tester, offerMove: true);
    await submit(tester, 'https://new.example');
    expect(find.byKey(const Key('move-local-notes')), findsOneWidget);
    expect(find.byKey(const Key('start-empty-collection')), findsOneWidget);
    await tester.tap(find.byKey(const Key('move-local-notes')));
    await tester.pumpAndSettle();
    expect(moved, ['https://new.example']);
    expect(switched, isEmpty);
    expect(find.byType(RelaySettingsScreen), findsNothing);
  });

  testWidgets('starting empty keeps local notes in their own collection', (tester) async {
    await pump(tester, offerMove: true);
    await submit(tester, 'https://new.example');
    await tester.tap(find.byKey(const Key('start-empty-collection')));
    await tester.pumpAndSettle();
    expect(switched, ['https://new.example']);
    expect(moved, isEmpty);
  });

  testWidgets('cancelling setup neither moves nor switches', (tester) async {
    await pump(tester, offerMove: true);
    await submit(tester, 'https://new.example');
    await tester.tap(find.text('Cancel'));
    await tester.pumpAndSettle();
    expect(switched, isEmpty);
    expect(moved, isEmpty);
  });

  testWidgets('saved relay collections never offer to move local notes over them', (tester) async {
    await pump(tester, offerMove: true);
    await submit(tester, 'https://relay.example');
    expect(find.byKey(const Key('move-local-notes')), findsNothing);
    expect(find.textContaining('Its saved notes will open'), findsOneWidget);
  });

  testWidgets('cancelling switches nothing', (tester) async {
    await pump(tester);
    await submit(tester, 'https://new.example');
    await tester.tap(find.text('Cancel'));
    await tester.pumpAndSettle();
    expect(switched, isEmpty);
    expect(find.byType(RelaySettingsScreen), findsOneWidget);
  });

  testWidgets('the relay already selected is not offered as a switch', (
    tester,
  ) async {
    await pump(tester, profile: _relay);
    await submit(tester, 'wss://relay.example');
    expect(find.text('Switch relay?'), findsNothing);
    expect(switched, isEmpty);
  });

  testWidgets('an address that is not a relay is explained, not accepted', (
    tester,
  ) async {
    await pump(tester);
    await submit(tester, 'relay.example');
    expect(
      find.text('Enter a relay URL such as https://relay.example.'),
      findsOneWidget,
    );
    expect(find.text('Switch relay?'), findsNothing);
    expect(switched, isEmpty);
  });

  testWidgets('a page served over HTTPS explains why it cannot use http://', (
    tester,
  ) async {
    await pump(tester, servedOverHttps: true);
    await submit(tester, 'http://192.168.1.10:8080');
    expect(
      find.textContaining(
        'only reach a relay whose address starts with '
        'https://',
      ),
      findsOneWidget,
    );
    expect(switched, isEmpty);

    await submit(tester, 'https://relay.example');
    expect(find.text('Switch relay?'), findsOneWidget);
  });

  testWidgets('a released phone app refuses a relay without TLS', (
    tester,
  ) async {
    await pump(tester, mobileRelease: true);
    await submit(tester, 'ws://192.168.1.10:8080');
    expect(
      find.textContaining('only sync over an encrypted connection'),
      findsOneWidget,
    );
    expect(switched, isEmpty);

    await submit(tester, 'wss://relay.example');
    expect(find.text('Switch relay?'), findsOneWidget);
  });

  testWidgets('a saved plain-http profile cannot be chosen in a phone '
      'release', (tester) async {
    const plain = RelayProfile(id: 'lan', url: 'http://192.168.1.10:8080');
    await pump(tester, profiles: const [_local, plain], mobileRelease: true);
    await tester.scrollUntilVisible(
      find.text('Needs an https:// relay in this version'),
      100,
      scrollable: find.byType(Scrollable).first,
    );
    await tester.pumpAndSettle();
    expect(
      find.text('Needs an https:// relay in this version'),
      findsOneWidget,
    );
    await tester.scrollUntilVisible(
      find.text('http://192.168.1.10:8080'),
      100,
      scrollable: find.byType(Scrollable).first,
    );
    await tester.ensureVisible(find.text('http://192.168.1.10:8080').last);
    await tester.pumpAndSettle();
    await tester.tap(find.text('http://192.168.1.10:8080').last);
    await tester.pumpAndSettle();
    expect(find.text('Switch relay?'), findsNothing);
  });

  testWidgets('a saved profile is one tap and a confirmation away', (
    tester,
  ) async {
    await pump(tester, profile: _relay);
    await tester.scrollUntilVisible(
      find.text('On this device'),
      100,
      scrollable: find.byType(Scrollable).first,
    );
    await tester.ensureVisible(find.text('On this device').last);
    await tester.pumpAndSettle();
    await tester.tap(find.text('On this device').last);
    await tester.pumpAndSettle();
    expect(find.textContaining('Switch to On this device?'), findsOneWidget);
    await tester.tap(find.text('Switch'));
    await tester.pumpAndSettle();
    expect(switched, [null]);
  });

  testWidgets('a switch that fails says why and stays on Settings', (
    tester,
  ) async {
    failWith = StateError('This profile has unsaved local changes.');
    await pump(tester);
    await submit(tester, 'https://new.example');
    await tester.tap(find.text('Switch'));
    await tester.pumpAndSettle();
    expect(
      find.text(
        'Could not switch relay: This profile has unsaved local '
        'changes.',
      ),
      findsOneWidget,
    );
    expect(find.byType(RelaySettingsScreen), findsOneWidget);
    // And it can be tried again.
    expect(
      tester
          .widget<FilledButton>(find.byKey(const Key('switch-relay-button')))
          .onPressed,
      isNotNull,
    );
  });

  group('a relay with an access key', () {
    final key = base64Url.encode(List<int>.filled(32, 3)).replaceAll('=', '');

    testWidgets('a relay link switches with its key', (tester) async {
      await pump(tester);
      await submit(tester, 'https://new.example/#access=$key');
      await tester.tap(find.text('Switch'));
      await tester.pumpAndSettle();
      expect(switched, ['https://new.example with $key']);
    });

    testWidgets('an address and a typed key switch together', (tester) async {
      await pump(tester);
      await tester.enterText(find.byKey(const Key('relay-access-field')), key);
      await submit(tester, 'https://new.example');
      await tester.tap(find.text('Switch'));
      await tester.pumpAndSettle();
      expect(switched, ['https://new.example with $key']);
    });

    testWidgets('a key that is not one is explained', (tester) async {
      await pump(tester);
      await tester.enterText(
        find.byKey(const Key('relay-access-field')),
        'nope',
      );
      await submit(tester, 'https://new.example');
      expect(find.text('That is not an access key.'), findsOneWidget);
      expect(switched, isEmpty);
    });

    testWidgets('the relay in use takes a key without switching', (
      tester,
    ) async {
      await pump(tester, profile: _relay);
      expect(find.text('No access key'), findsOneWidget);
      await tester.tap(find.byKey(const Key('relay-access-enter')));
      await tester.pumpAndSettle();
      await tester.enterText(
        find.byKey(const Key('relay-access-dialog-field')),
        'https://other.example/#access=$key',
      );
      await tester.tap(find.byKey(const Key('relay-access-save')));
      await tester.pumpAndSettle();
      expect(
        find.text('That link is for another relay: https://other.example.'),
        findsOneWidget,
      );

      await tester.enterText(
        find.byKey(const Key('relay-access-dialog-field')),
        'https://relay.example/#access=$key',
      );
      await tester.tap(find.byKey(const Key('relay-access-save')));
      await tester.pumpAndSettle();
      expect(keys, [key]);
      expect(switched, isEmpty);
      expect(find.byType(RelaySettingsScreen), findsNothing);
    });

    testWidgets('a relay with a key offers its link to copy', (tester) async {
      await pump(
        tester,
        profile: _relay.withAccess(key),
        profiles: [_local, _relay.withAccess(key)],
      );
      expect(find.text('Access key saved'), findsOneWidget);
      expect(find.byKey(const Key('relay-link-copy')), findsOneWidget);
    });
  });
}

