/// The first run: the intro, the one setup card, and the send-off.
///
/// What these hold is the shape of the choice: the intro explains that sync is
/// one decision (a server and the account that belongs to it), a finished step
/// looks finished, and every path ends on the send-off page. Without sync the
/// send-off says the notes are not backed up.
library;

import 'package:easynotes/ui/onboarding_screen.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  late int continued;
  late int setUps;

  setUp(() {
    continued = 0;
    setUps = 0;
  });

  Widget app({bool relayConnected = false, bool accountCreated = false}) =>
      MaterialApp(
        home: OnboardingScreen(
          relayConnected: relayConnected,
          accountCreated: accountCreated,
          serverUrl: relayConnected ? 'https://sync.example.com/team' : null,
          onContinue: () => continued++,
          onSetUpSync: () => setUps++,
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

  testWidgets('the intro explains the app and the one sync decision', (
    tester,
  ) async {
    await tester.pumpWidget(app());

    expect(find.byKey(const Key('onboarding-welcome')), findsOneWidget);
    expect(find.text('On this device'), findsOneWidget);
    expect(find.text('Sync server & account'), findsOneWidget);
    expect(find.byKey(const Key('onboarding-optional')), findsOneWidget);
    // Nothing is set up from the intro.
    expect(find.byKey(const Key('onboarding-sync-step')), findsNothing);
  });

  testWidgets('setup is one card, and leaving it stays offline', (
    tester,
  ) async {
    await tester.pumpWidget(app());
    await openSetup(tester);

    expect(find.byKey(const Key('onboarding-sync-step')), findsOneWidget);
    expect(find.text('Sync server & account'), findsOneWidget);
    expect(find.byKey(const Key('onboarding-continue')), findsOneWidget);

    await tap(tester, find.byKey(const Key('onboarding-sync-step')));
    expect(setUps, 1);
    expect(continued, 0);

    // Leaving the setup goes to the send-off, not out of onboarding. The
    // send-off must not imply a backup that does not exist.
    await tap(tester, find.byKey(const Key('onboarding-continue')));
    expect(find.byKey(const Key('onboarding-done')), findsOneWidget);
    expect(find.textContaining('not backed up'), findsOneWidget);
    expect(continued, 0);
  });

  testWidgets('a finished setup is a finished card and the send-off says how '
      'to get the notes back', (tester) async {
    await tester.pumpWidget(app(relayConnected: true, accountCreated: true));
    await openSetup(tester);

    expect(find.textContaining('Set up with sync.example.com'), findsOneWidget);
    expect(
      find.descendant(
        of: find.byKey(const Key('onboarding-sync-step')),
        matching: find.byIcon(Icons.check),
      ),
      findsOneWidget,
    );
    expect(find.byKey(const Key('onboarding-continue')), findsOneWidget);

    // A finished step is not set up again.
    await tap(tester, find.byKey(const Key('onboarding-sync-step')));
    expect(setUps, 0);

    await tap(tester, find.byKey(const Key('onboarding-continue')));
    expect(find.byKey(const Key('onboarding-done')), findsOneWidget);
    expect(find.textContaining('twelve words'), findsOneWidget);
  });

  testWidgets('a half-finished setup says which half is open', (tester) async {
    await tester.pumpWidget(app(relayConnected: true));
    await openSetup(tester);

    expect(
      find.textContaining('Connected to sync.example.com'),
      findsOneWidget,
    );
    expect(find.textContaining('account step is still open'), findsOneWidget);
    await tap(tester, find.byKey(const Key('onboarding-sync-step')));
    expect(setUps, 1);
  });

  testWidgets('completing setup moves on to the send-off by itself', (
    tester,
  ) async {
    await tester.pumpWidget(app());
    await openSetup(tester);
    await tap(tester, find.byKey(const Key('onboarding-sync-step')));

    // The parent reports the finished setup, as the real screen does once the
    // account step closes.
    await tester.pumpWidget(app(relayConnected: true, accountCreated: true));
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('onboarding-done')), findsOneWidget);
    expect(find.textContaining('sync across your devices'), findsOneWidget);
    expect(continued, 0);

    await tap(tester, find.byKey(const Key('onboarding-start-writing')));
    expect(continued, 1);
  });

  testWidgets('the setup page has no way back to the intro', (tester) async {
    await tester.pumpWidget(app());
    await openSetup(tester);

    // Going forward is a choice made with the buttons here; the intro is not
    // something to return to.
    expect(find.byIcon(Icons.arrow_back), findsNothing);
    expect(find.byKey(const Key('onboarding-welcome')), findsNothing);
  });
}
