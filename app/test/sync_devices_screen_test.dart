import 'dart:convert';

import 'package:easynotes/core/fake_note_library.dart';
import 'package:easynotes/core/relay_profiles.dart';
import 'package:easynotes/core/sync_status.dart';
import 'package:easynotes/ui/account_screen.dart';
import 'package:easynotes/ui/note_list_screen.dart';
import 'package:easynotes/ui/relay_setup_wizard.dart';
import 'package:easynotes/ui/setup_widgets.dart';
import 'package:easynotes/ui/sync_devices_screen.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

const relay = RelayProfile(id: 'relay', url: 'https://relay.example');
const local = RelayProfile(id: 'local', url: null);

void main() {
  final key = base64Url.encode(List<int>.filled(32, 3)).replaceAll('=', '');

  /// The screen on its own, which is where a settings screen starts.
  Future<void> pump(
    WidgetTester tester,
    FakeNoteLibrary library, {
    RelayProfile profile = relay,
    List<RelayProfile> profiles = const [local, relay],
    Future<void> Function(String?)? onSetAccess,
  }) async {
    await tester.pumpWidget(
      MaterialApp(
        home: SyncDevicesScreen(
          library: library,
          profile: profile,
          profiles: profiles,
          onSetAccess: onSetAccess,
          onSwitchRelay: (url, {access}) async {},
        ),
      ),
    );
  }

  testWidgets('a relay and no account is two steps, the second one open', (
    tester,
  ) async {
    final library = FakeNoteLibrary()
      ..pushStatus(const SyncStatus(state: SyncState.synced));
    await pump(tester, library);

    expect(find.text('Up to date'), findsOneWidget);
    // The host names the server in the status card and again in its row.
    expect(find.text('relay.example'), findsNWidgets(2));
    expect(find.text('Connected to relay.example.'), findsOneWidget);
    expect(
      find.text('Sync between your devices and share notes.'),
      findsOneWidget,
    );
    expect(find.text('Fully set up'), findsNothing);
    expect(find.byKey(const Key('account-settings')), findsNothing);

    await tester.tap(find.byKey(const Key('sync-setup-account-step')));
    await tester.pumpAndSettle();
    expect(find.byType(AccountScreen), findsOneWidget);
    expect(find.byKey(const Key('create-account-button')), findsOneWidget);
  });

  testWidgets('without a server the checklist starts at the first step', (
    tester,
  ) async {
    final library = FakeNoteLibrary();
    await pump(tester, library, profile: local);

    expect(find.text('Sync not set up'), findsOneWidget);
    expect(find.text('Your notes are saved on this device.'), findsOneWidget);
    expect(find.text('Works once a sync server is connected.'), findsOneWidget);
    expect(
      tester.widget<TextButton>(find.byKey(const Key('sync-now'))).onPressed,
      isNull,
    );

    await tester.tap(find.byKey(const Key('sync-setup-server-step')));
    await tester.pumpAndSettle();
    expect(find.byType(RelaySetupWizard), findsOneWidget);
  });

  testWidgets('an account without a relay does not claim to be syncing', (
    tester,
  ) async {
    final library = FakeNoteLibrary();
    await library.createAccount();
    library.pushStatus(const SyncStatus(state: SyncState.synced));
    await pump(tester, library, profile: local);

    expect(find.text('Sync not set up'), findsOneWidget);
    expect(find.text('Up to date'), findsNothing);
    expect(find.text('Account created.'), findsOneWidget);
    // Both setup steps are done except the one that needs a server first.
    expect(find.byType(SetupStep), findsNWidgets(2));
    expect(find.text('Fully set up'), findsNothing);
  });

  testWidgets('a finished setup is one line, not a checklist', (tester) async {
    final library = FakeNoteLibrary();
    await library.createAccount();
    await pump(tester, library);

    expect(find.byKey(const Key('sync-fully-set-up')), findsOneWidget);
    expect(find.byType(SetupStep), findsNothing);
    expect(find.byKey(const Key('sync-setup-server-step')), findsNothing);
    expect(find.byKey(const Key('account-settings')), findsOneWidget);
    expect(find.byKey(const Key('switch-server')), findsOneWidget);
  });

  testWidgets('a recovery phrase can be reached from the account step', (
    tester,
  ) async {
    final library = FakeNoteLibrary();
    await pump(tester, library);

    await tester.tap(find.byKey(const Key('sync-setup-account-step')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('adopt-account-button')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('adopt-phrase-field')), findsOneWidget);
  });

  testWidgets('the status card holds the last sync and the sync button', (
    tester,
  ) async {
    final library = FakeNoteLibrary()
      ..pushStatus(const SyncStatus(state: SyncState.synced));
    await pump(tester, library);

    final card = find.byType(Card);
    expect(card, findsOneWidget);
    expect(
      find.descendant(of: card, matching: find.byKey(const Key('last-synced'))),
      findsOneWidget,
    );
    expect(
      find.descendant(of: card, matching: find.byKey(const Key('sync-now'))),
      findsOneWidget,
    );
    expect(find.textContaining('Last synced'), findsOneWidget);
  });

  testWidgets('manual refresh is available only for relay profiles', (
    tester,
  ) async {
    final library = FakeNoteLibrary();
    await pump(tester, library);
    await tester.tap(find.byKey(const Key('sync-now')));
    await tester.pumpAndSettle();
    expect(library.calls, contains('syncNow()'));

    await pump(tester, library, profile: local);
    expect(
      tester.widget<TextButton>(find.byKey(const Key('sync-now'))).onPressed,
      isNull,
    );
  });

  testWidgets('a relay error is shown where sync stands', (tester) async {
    final library = FakeNoteLibrary();
    await pump(tester, library);
    library.pushStatus(
      const SyncStatus(
        state: SyncState.offline,
        pending: 2,
        lastError: 'Relay unavailable',
      ),
    );
    await tester.pumpAndSettle();

    expect(find.text('Not syncing, 2 edits waiting'), findsOneWidget);
    expect(find.byKey(const Key('sync-error')), findsOneWidget);
    expect(find.text('Relay unavailable'), findsOneWidget);
  });

  testWidgets('a key the server refuses is never kept', (tester) async {
    final library = FakeNoteLibrary();
    final saved = <String?>[];
    await pump(
      tester,
      library,
      onSetAccess: (access) async => saved.add(access),
    );
    expect(find.text('No access key'), findsOneWidget);

    library.verifyRelayFails = 'the server refused this key';
    final access = find.byKey(const Key('relay-access-change'));
    await tester.scrollUntilVisible(
      access,
      200,
      scrollable: find.byType(Scrollable).first,
    );
    await tester.pumpAndSettle();
    await tester.tap(access);
    await tester.pumpAndSettle();
    expect(find.byType(RelaySetupWizard), findsOneWidget);

    await tester.enterText(find.byKey(const Key('relay-setup-access')), key);
    await tester.tap(find.byKey(const Key('relay-setup-access-continue')));
    await tester.pumpAndSettle();

    expect(saved, isEmpty);
    expect(find.byKey(const Key('relay-setup-failed')), findsOneWidget);
    expect(find.textContaining('refused this key'), findsOneWidget);

    // The same key is fine once the server accepts it.
    library.verifyRelayFails = null;
    await tester.tap(find.byKey(const Key('relay-setup-retry')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('relay-setup-access-continue')));
    await tester.pumpAndSettle();

    expect(saved, [key]);
    expect(find.byType(RelaySetupWizard), findsNothing);
    expect(find.text('Access key saved'), findsOneWidget);
  });

  testWidgets(
    'how sync works explains the same three things as the first run',
    (tester) async {
      final library = FakeNoteLibrary();
      await pump(tester, library);

      final more = find.byKey(const Key('how-sync-works'));
      await tester.scrollUntilVisible(
        more,
        200,
        scrollable: find.byType(Scrollable).first,
      );
      await tester.pumpAndSettle();
      await tester.tap(more);
      await tester.pumpAndSettle();

      expect(find.text('On this device'), findsOneWidget);
      expect(find.textContaining('Backs up your notes online'), findsOneWidget);
      expect(
        find.textContaining('Syncs your notes between your devices'),
        findsOneWidget,
      );
    },
  );

  testWidgets('the note list drawer opens the new screen', (tester) async {
    final library = FakeNoteLibrary()
      ..pushStatus(const SyncStatus(state: SyncState.synced));
    await tester.pumpWidget(
      MaterialApp(
        home: NoteListScreen(library: library, profile: relay),
      ),
    );
    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('sync-status')));
    await tester.pumpAndSettle();

    expect(find.text('Up to date'), findsOneWidget);
    expect(find.byKey(const Key('switch-server')), findsOneWidget);
  });
}
