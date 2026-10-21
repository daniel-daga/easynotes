/// The first-run relay setup: address, access key, then the handshake.
///
/// What these hold is the promise of the three screens: the address is read
/// before the key, nothing is committed until the relay has answered, and a
/// relay that does not answer leaves the collection where it was.
library;

import 'dart:convert';

import 'package:easynotes/ui/relay_setup_wizard.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  final key = base64Url.encode(List<int>.filled(32, 3)).replaceAll('=', '');

  late List<String> verified;
  late List<String> switched;
  late List<String> events;
  late bool? result;
  Object? verifyFails;
  Object? switchFails;

  setUp(() {
    verified = [];
    switched = [];
    events = [];
    result = null;
    verifyFails = null;
    switchFails = null;
  });

  /// The wizard over a home route, so that popping it has somewhere to go and
  /// the value it popped with can be read.
  Future<void> pump(
    WidgetTester tester, {
    bool servedOverHttps = false,
    bool mobileRelease = false,
  }) async {
    await tester.pumpWidget(
      MaterialApp(
        home: Builder(
          builder: (context) => Scaffold(
            body: TextButton(
              onPressed: () async {
                result = await Navigator.of(context).push<bool>(
                  MaterialPageRoute<bool>(
                    builder: (_) => RelaySetupWizard(
                      servedOverHttps: servedOverHttps,
                      mobileRelease: mobileRelease,
                      onVerifyRelay: (url, {access}) async {
                        events.add('verify');
                        verified.add(
                          access == null ? url : '$url with $access',
                        );
                        if (verifyFails case final error?) throw error;
                      },
                      onSwitchRelay: (url, {access}) async {
                        events.add('switch');
                        switched.add(
                          access == null ? '$url' : '$url with $access',
                        );
                        if (switchFails case final error?) throw error;
                      },
                    ),
                  ),
                );
              },
              child: const Text('open'),
            ),
          ),
        ),
      ),
    );
    await tester.tap(find.text('open'));
    await tester.pumpAndSettle();
  }

  /// Tap something that may be below the fold, scrolling it into view the way
  /// a person on a small phone would.
  Future<void> tapKey(WidgetTester tester, String key) async {
    final finder = find.byKey(Key(key));
    await tester.scrollUntilVisible(
      finder,
      200,
      scrollable: find.byType(Scrollable).last,
    );
    await tester.tap(finder);
    await tester.pumpAndSettle();
  }

  Future<void> enterUrl(WidgetTester tester, String url) async {
    await tester.enterText(find.byKey(const Key('relay-setup-url')), url);
    await tapKey(tester, 'relay-setup-url-continue');
  }

  Future<void> enterAccess(WidgetTester tester, [String? access]) async {
    if (access != null) {
      await tester.enterText(
        find.byKey(const Key('relay-setup-access')),
        access,
      );
    }
    await tapKey(tester, 'relay-setup-access-continue');
  }

  testWidgets('asks for the address, then the key, then commits', (
    tester,
  ) async {
    await pump(tester);
    expect(find.byKey(const Key('relay-setup-url')), findsOneWidget);
    expect(find.byKey(const Key('relay-setup-access')), findsNothing);

    await enterUrl(tester, 'wss://New.Example/');
    expect(find.byKey(const Key('relay-setup-access')), findsOneWidget);
    expect(find.textContaining('https://new.example'), findsOneWidget);
    expect(verified, isEmpty);

    await enterAccess(tester, key);
    expect(verified, ['https://new.example with $key']);
    expect(switched, ['https://new.example with $key']);
    expect(events, ['verify', 'switch']);
    expect(result, isTrue);
    expect(find.byType(RelaySetupWizard), findsNothing);
  });

  testWidgets('an open relay is connected with no key', (tester) async {
    await pump(tester);
    await enterUrl(tester, 'https://relay.example');
    await enterAccess(tester);

    expect(verified, ['https://relay.example']);
    expect(switched, ['https://relay.example']);
    expect(result, isTrue);
  });

  testWidgets('an address that is not a relay is explained at its own step', (
    tester,
  ) async {
    await pump(tester);
    await enterUrl(tester, 'relay.example');

    expect(
      find.text('Enter a relay URL such as https://relay.example.'),
      findsOneWidget,
    );
    expect(find.byKey(const Key('relay-setup-access')), findsNothing);
    expect(verified, isEmpty);
  });

  testWidgets('an access key that is not one is refused before connecting', (
    tester,
  ) async {
    await pump(tester);
    await enterUrl(tester, 'https://relay.example');
    await enterAccess(tester, 'nope');

    expect(
      find.text('That is not an access key or relay link.'),
      findsOneWidget,
    );
    expect(verified, isEmpty);
    expect(switched, isEmpty);
  });

  testWidgets('a relay link for another relay is refused at the key step', (
    tester,
  ) async {
    await pump(tester);
    await enterUrl(tester, 'https://relay.example');
    await enterAccess(tester, 'https://other.example/#access=$key');

    expect(
      find.text('That link is for another relay: https://other.example.'),
      findsOneWidget,
    );
    expect(verified, isEmpty);
  });

  testWidgets('a relay link for this relay is accepted at the key step', (
    tester,
  ) async {
    await pump(tester);
    await enterUrl(tester, 'https://relay.example');
    await enterAccess(tester, 'https://relay.example/#access=$key');

    expect(verified, ['https://relay.example with $key']);
    expect(result, isTrue);
  });

  testWidgets('a relay link carries its key into the access step', (
    tester,
  ) async {
    await pump(tester);
    await enterUrl(tester, 'https://relay.example/#access=$key');

    final field = tester.widget<TextFormField>(
      find.byKey(const Key('relay-setup-access')),
    );
    expect(field.controller?.text, key);

    await enterAccess(tester);
    expect(verified, ['https://relay.example with $key']);
    expect(result, isTrue);
  });

  testWidgets('a relay that cannot be reached says so and can be retried', (
    tester,
  ) async {
    verifyFails = 'connection refused';
    await pump(tester);
    await enterUrl(tester, 'https://relay.example');
    await enterAccess(tester, key);

    expect(find.byKey(const Key('relay-setup-failed')), findsOneWidget);
    expect(find.textContaining('connection refused'), findsOneWidget);
    expect(switched, isEmpty);
    expect(result, isNull);

    await tapKey(tester, 'relay-setup-retry');
    expect(find.byKey(const Key('relay-setup-url')), findsOneWidget);
    // What was typed is still there, so a retry is a correction and not a
    // fresh start.
    expect(
      tester
          .widget<TextFormField>(find.byKey(const Key('relay-setup-url')))
          .controller
          ?.text,
      'https://relay.example',
    );
  });

  testWidgets('aborting after a failure leaves the collection alone', (
    tester,
  ) async {
    verifyFails = 'not a valid relay url';
    await pump(tester);
    await enterUrl(tester, 'https://relay.example');
    await enterAccess(tester, key);
    await tapKey(tester, 'relay-setup-abort');

    expect(result, isFalse);
    expect(switched, isEmpty);
    expect(verified, ['https://relay.example with $key']);
    expect(find.byType(RelaySetupWizard), findsNothing);
    expect(find.text('open'), findsOneWidget);
  });

  testWidgets('a selection that fails after verification is recoverable', (
    tester,
  ) async {
    switchFails = StateError('This tab has no storage.');
    await pump(tester);
    await enterUrl(tester, 'https://relay.example');
    await enterAccess(tester);

    expect(find.byKey(const Key('relay-setup-failed')), findsOneWidget);
    expect(find.textContaining('no storage'), findsOneWidget);
    expect(result, isNull);
  });

  testWidgets('back returns one step at a time and never skips the key', (
    tester,
  ) async {
    await pump(tester);
    await enterUrl(tester, 'https://relay.example');
    expect(find.byKey(const Key('relay-setup-access')), findsOneWidget);

    await tester.tap(find.byTooltip('Back'));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('relay-setup-url')), findsOneWidget);
  });

  testWidgets('a page served over HTTPS explains why it cannot use http://', (
    tester,
  ) async {
    await pump(tester, servedOverHttps: true);
    await enterUrl(tester, 'http://192.168.1.10:8080');

    expect(
      find.textContaining(
        'only reach a relay whose address starts with https://',
      ),
      findsOneWidget,
    );
    expect(verified, isEmpty);
  });

  testWidgets('a released phone app refuses a relay without TLS', (
    tester,
  ) async {
    await pump(tester, mobileRelease: true);
    await enterUrl(tester, 'ws://192.168.1.10:8080');

    expect(
      find.textContaining('only sync over an encrypted connection'),
      findsOneWidget,
    );
    expect(verified, isEmpty);
  });
}
