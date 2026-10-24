/// Settings → Sync server: choosing among saved collections, and connecting a
/// new one only through the handshake.
library;

import 'package:easynotes/core/relay_profiles.dart';
import 'package:easynotes/ui/relay_setup_wizard.dart';
import 'package:easynotes/ui/server_switcher_screen.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

const _local = RelayProfile(id: 'local', url: null);
const _relay = RelayProfile(id: 'relay', url: 'https://relay.example');

void main() {
  late List<String?> switched;
  late List<String> verified;
  late List<String> moved;
  Object? failWith;

  setUp(() {
    switched = [];
    verified = [];
    moved = [];
    failWith = null;
  });

  Future<void> onSwitch(String? url, {String? access}) async {
    switched.add(url);
    if (failWith != null) throw failWith!;
  }

  Future<void> onVerify(String url, {String? access}) async {
    verified.add(access == null ? url : '$url with $access');
  }

  /// The screen pushed over a home route, so that a switch has somewhere to
  /// pop back to.
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
                builder: (_) => ServerSwitcherScreen(
                  profile: profile,
                  profiles: profiles,
                  onSwitchRelay: onSwitch,
                  onVerifyRelay: onVerify,
                  onMoveLocalToRelay: offerMove
                      ? (url, {access}) async => moved.add(url)
                      : null,
                  hasLocalNotes: offerMove,
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

  testWidgets('a saved collection opens after one confirmation', (
    tester,
  ) async {
    await pump(tester, profile: _relay);
    await tester.tap(find.text('On this device'));
    await tester.pumpAndSettle();
    expect(find.textContaining('Switch to On this device?'), findsOneWidget);
    expect(find.textContaining('Its saved notes will open'), findsOneWidget);

    await tester.tap(find.text('Switch'));
    await tester.pumpAndSettle();
    expect(switched, [null]);
    expect(find.byType(ServerSwitcherScreen), findsNothing);
  });

  testWidgets('the selected collection is not offered as a switch', (
    tester,
  ) async {
    await pump(tester);
    expect(find.text('Selected'), findsOneWidget);
    await tester.tap(find.text('On this device'));
    await tester.pumpAndSettle();
    expect(find.text('Switch sync server?'), findsNothing);
    expect(switched, isEmpty);
  });

  testWidgets('cancelling switches nothing', (tester) async {
    await pump(tester, profile: _relay);
    await tester.tap(find.text('On this device'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Cancel'));
    await tester.pumpAndSettle();
    expect(switched, isEmpty);
    expect(find.byType(ServerSwitcherScreen), findsOneWidget);
  });

  testWidgets('a saved plain-http collection cannot be chosen in a phone '
      'release', (tester) async {
    const plain = RelayProfile(id: 'lan', url: 'http://192.168.1.10:8080');
    await pump(tester, profiles: const [_local, plain], mobileRelease: true);

    expect(
      find.text('Needs an https:// server in this version'),
      findsOneWidget,
    );
    await tester.tap(find.text('http://192.168.1.10:8080'));
    await tester.pumpAndSettle();
    expect(find.text('Switch sync server?'), findsNothing);
    expect(switched, isEmpty);
  });

  testWidgets('a new server is checked before it is switched to', (
    tester,
  ) async {
    await pump(tester);
    await tester.tap(find.byKey(const Key('connect-server')));
    await tester.pumpAndSettle();
    expect(find.byType(RelaySetupWizard), findsOneWidget);

    await tester.enterText(
      find.byKey(const Key('relay-setup-url')),
      'wss://New.Example/',
    );
    await tester.tap(find.byKey(const Key('relay-setup-url-continue')));
    await tester.pumpAndSettle();
    expect(
      find.text('This server starts with an empty collection.'),
      findsOneWidget,
    );

    await tester.tap(find.byKey(const Key('relay-setup-access-continue')));
    await tester.pumpAndSettle();

    expect(verified, ['https://new.example']);
    expect(switched, ['https://new.example']);
    expect(find.byType(ServerSwitcherScreen), findsNothing);
    expect(find.text('open'), findsOneWidget);
  });

  testWidgets('a new server from On this device offers to take the notes '
      'along', (tester) async {
    await pump(tester, offerMove: true);
    await tester.tap(find.byKey(const Key('connect-server')));
    await tester.pumpAndSettle();
    await tester.enterText(
      find.byKey(const Key('relay-setup-url')),
      'https://new.example',
    );
    await tester.tap(find.byKey(const Key('relay-setup-url-continue')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('relay-setup-access-continue')));
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('relay-setup-move-question')), findsOneWidget);
    expect(switched, isEmpty);

    await tester.tap(find.byKey(const Key('move-local-notes')));
    await tester.pumpAndSettle();
    expect(moved, ['https://new.example']);
    expect(switched, isEmpty);
    expect(find.byType(ServerSwitcherScreen), findsNothing);
  });

  testWidgets('a server with a saved collection says its notes will open', (
    tester,
  ) async {
    await pump(tester);
    await tester.tap(find.byKey(const Key('connect-server')));
    await tester.pumpAndSettle();
    await tester.enterText(
      find.byKey(const Key('relay-setup-url')),
      'https://relay.example:443',
    );
    await tester.tap(find.byKey(const Key('relay-setup-url-continue')));
    await tester.pumpAndSettle();

    expect(
      find.textContaining('already has a collection saved here'),
      findsOneWidget,
    );
  });

  testWidgets('the server already in use is refused at the address step', (
    tester,
  ) async {
    await pump(tester, profile: _relay);
    await tester.tap(find.byKey(const Key('connect-server')));
    await tester.pumpAndSettle();
    await tester.enterText(
      find.byKey(const Key('relay-setup-url')),
      'wss://relay.example',
    );
    await tester.tap(find.byKey(const Key('relay-setup-url-continue')));
    await tester.pumpAndSettle();

    expect(
      find.text('You are already using this sync server.'),
      findsOneWidget,
    );
    expect(verified, isEmpty);
  });

  testWidgets('a page served over HTTPS explains why it cannot use http://', (
    tester,
  ) async {
    await pump(tester, servedOverHttps: true);
    await tester.tap(find.byKey(const Key('connect-server')));
    await tester.pumpAndSettle();
    await tester.enterText(
      find.byKey(const Key('relay-setup-url')),
      'http://192.168.1.10:8080',
    );
    await tester.tap(find.byKey(const Key('relay-setup-url-continue')));
    await tester.pumpAndSettle();

    expect(
      find.textContaining(
        'only reach a sync server whose address starts with https://',
      ),
      findsOneWidget,
    );
    expect(verified, isEmpty);
  });

  testWidgets('a switch that fails says why and stays on the switcher', (
    tester,
  ) async {
    failWith = StateError('This profile has unsaved local changes.');
    await pump(tester, profile: _relay);
    await tester.tap(find.text('On this device'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Switch'));
    await tester.pumpAndSettle();

    expect(
      find.text(
        'Could not switch sync server: This profile has unsaved local '
        'changes.',
      ),
      findsOneWidget,
    );
    expect(find.byType(ServerSwitcherScreen), findsOneWidget);
  });
}
