/// Settings → Relay: choosing a relay, and being told what that does first.
library;

import 'package:easynotes/core/relay_profiles.dart';
import 'package:easynotes/ui/relay_settings_screen.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

const _local = RelayProfile(id: 'local', url: null);
const _relay = RelayProfile(id: 'relay', url: 'https://relay.example');

void main() {
  late List<String?> switched;
  Object? failWith;

  setUp(() {
    switched = [];
    failWith = null;
  });

  Future<void> onSwitch(String? url) async {
    switched.add(url);
    if (failWith != null) throw failWith!;
  }

  /// The screen pushed over a home route, so that a successful switch has
  /// somewhere to pop back to.
  Future<void> pump(
    WidgetTester tester, {
    RelayProfile profile = _local,
    List<RelayProfile> profiles = const [_local, _relay],
    bool servedOverHttps = false,
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
                  servedOverHttps: servedOverHttps,
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
    await tester.tap(find.byKey(const Key('switch-relay-button')));
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

  testWidgets('a saved profile is one tap and a confirmation away', (
    tester,
  ) async {
    await pump(tester, profile: _relay);
    await tester.tap(find.text('On this device'));
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
}
