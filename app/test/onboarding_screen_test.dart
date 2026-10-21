/// The first run: the intro, the two-step checklist, and the send-off.
///
/// What these hold is the shape of the choices: the intro explains both levels
/// of sync, the account step cannot be taken before a server is connected, a
/// finished step looks finished, and every path ends on the send-off page.
library;

import 'package:easynotes/ui/onboarding_screen.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  late int continued;
  late int setUps;
  late int created;

  setUp(() {
    continued = 0;
    setUps = 0;
    created = 0;
  });

  Widget app({bool relayConnected = false, bool accountCreated = false}) =>
      MaterialApp(
        home: OnboardingScreen(
          relayConnected: relayConnected,
          accountCreated: accountCreated,
          serverUrl: relayConnected ? 'https://sync.example.com/team' : null,
          onContinue: () => continued++,
          onSetUpRelay: () => setUps++,
          onCreateAccount: () => created++,
        ),
      );

  /// Tap something that may be below the fold, the way a person on a small
  /// phone would.
  Future<void> tap(WidgetTester tester, Finder finder) async {
    await tester.ensureVisible(finder);
    await tester.pumpAndSettle();
    await tester.tap(finder);
    await tester.pumpAndSettle();
  }

  Future<void> openSetup(WidgetTester tester) async {
    await tap(tester, find.byKey(const Key('onboarding-next')));
  }

  testWidgets('the intro explains the app and both levels of sync', (
    tester,
  ) async {
    await tester.pumpWidget(app());

    expect(find.byKey(const Key('onboarding-welcome')), findsOneWidget);
    expect(find.text('On this device'), findsOneWidget);
    expect(find.text('Sync server'), findsOneWidget);
    expect(find.text('Account'), findsOneWidget);
    expect(find.byKey(const Key('onboarding-optional')), findsOneWidget);
    // Nothing is set up from the intro.
    expect(find.byKey(const Key('onboarding-server-step')), findsNothing);
  });

  testWidgets('the account step waits for a sync server', (tester) async {
    await tester.pumpWidget(app());
    await openSetup(tester);

    expect(find.text('Connect a sync server first.'), findsOneWidget);

    // Not tappable yet, and the server step is.
    await tap(tester, find.byKey(const Key('onboarding-account-step')));
    expect(created, 0);
    await tap(tester, find.byKey(const Key('onboarding-server-step')));
    expect(setUps, 1);
    expect(continued, 0);

    // Leaving the checklist goes to the send-off, not out of onboarding.
    await tap(tester, find.byKey(const Key('onboarding-continue')));
    expect(find.byKey(const Key('onboarding-done')), findsOneWidget);
    expect(continued, 0);
  });

  testWidgets('a connected server is a finished step and opens the account', (
    tester,
  ) async {
    await tester.pumpWidget(app(relayConnected: true));
    await openSetup(tester);

    expect(
      find.textContaining('Connected to sync.example.com'),
      findsOneWidget,
    );
    expect(find.text('Connect a sync server first.'), findsNothing);
    expect(
      find.descendant(
        of: find.byKey(const Key('onboarding-server-step')),
        matching: find.byIcon(Icons.check),
      ),
      findsOneWidget,
    );
    expect(
      find.descendant(
        of: find.byKey(const Key('onboarding-account-step')),
        matching: find.byIcon(Icons.check),
      ),
      findsNothing,
    );

    // A finished step is not set up again.
    await tap(tester, find.byKey(const Key('onboarding-server-step')));
    expect(setUps, 0);

    await tap(tester, find.byKey(const Key('onboarding-account-step')));
    expect(created, 1);
  });

  testWidgets('making the account moves on to the send-off by itself', (
    tester,
  ) async {
    await tester.pumpWidget(app(relayConnected: true));
    await openSetup(tester);
    await tap(tester, find.byKey(const Key('onboarding-account-step')));

    // The parent reports the new account, as the real screen does once it
    // closes.
    await tester.pumpWidget(app(relayConnected: true, accountCreated: true));
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('onboarding-done')), findsOneWidget);
    expect(find.textContaining('sync across your devices'), findsOneWidget);
    expect(continued, 0);

    await tap(tester, find.byKey(const Key('onboarding-start-writing')));
    expect(continued, 1);
  });

  testWidgets('a server without an account still gets a send-off', (
    tester,
  ) async {
    await tester.pumpWidget(app(relayConnected: true));
    await openSetup(tester);
    await tap(tester, find.byKey(const Key('onboarding-continue')));

    expect(find.byKey(const Key('onboarding-done')), findsOneWidget);
    expect(
      find.textContaining('backed up to sync.example.com'),
      findsOneWidget,
    );

    await tap(tester, find.byKey(const Key('onboarding-start-writing')));
    expect(continued, 1);
  });

  testWidgets('staying offline still gets a send-off', (tester) async {
    await tester.pumpWidget(app());
    await openSetup(tester);
    await tap(tester, find.byKey(const Key('onboarding-continue')));

    expect(find.byKey(const Key('onboarding-done')), findsOneWidget);
    expect(find.textContaining('stored on this device'), findsOneWidget);

    await tap(tester, find.byKey(const Key('onboarding-start-writing')));
    expect(continued, 1);
  });

  testWidgets('the checklist can go back to the intro', (tester) async {
    await tester.pumpWidget(app());
    await openSetup(tester);
    await tap(tester, find.byKey(const Key('onboarding-back')));

    expect(find.byKey(const Key('onboarding-welcome')), findsOneWidget);
  });
}
