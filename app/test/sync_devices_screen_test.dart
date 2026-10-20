import 'package:easynotes/core/fake_note_library.dart';
import 'package:easynotes/core/relay_profiles.dart';
import 'package:easynotes/core/sync_status.dart';
import 'package:easynotes/ui/note_list_screen.dart';
import 'package:easynotes/ui/sync_devices_screen.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

const relay = RelayProfile(id: 'relay', url: 'https://relay.example');

void main() {
  testWidgets('relay delivery is acknowledged without an account', (
    tester,
  ) async {
    final library = FakeNoteLibrary()
      ..pushStatus(const SyncStatus(state: SyncState.synced));
    await tester.pumpWidget(
      MaterialApp(
        home: NoteListScreen(library: library, profile: relay),
      ),
    );
    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    expect(find.text('Up to date with relay'), findsOneWidget);
    expect(find.text('Your devices'), findsNothing);
    expect(find.text('Relay settings'), findsNothing);
    await tester.tap(find.byKey(const Key('sync-status')));
    await tester.pumpAndSettle();
    expect(find.text('Up to date'), findsOneWidget);
    expect(
      find.text('Your notes’ encrypted updates have reached your relay.'),
      findsOneWidget,
    );
    expect(find.byKey(const Key('account-no')), findsOneWidget);
    expect(library.hasAccount, isFalse);

    library.pushStatus(
      const SyncStatus(
        state: SyncState.offline,
        pending: 2,
        lastError: 'Relay unavailable',
      ),
    );
    await tester.pumpAndSettle();
    expect(find.text('Not syncing, 2 edits waiting'), findsOneWidget);
    expect(find.text('Relay unavailable'), findsOneWidget);
    expect(
      find.text('Your notes’ encrypted updates have reached your relay.'),
      findsNothing,
    );
  });

  testWidgets('an account without a relay does not claim to be syncing', (
    tester,
  ) async {
    final library = FakeNoteLibrary();
    await library.createAccount();
    // Even a stale engine status must not override local collection setup.
    library.pushStatus(const SyncStatus(state: SyncState.synced));
    var openedServer = false;
    await tester.pumpWidget(
      MaterialApp(
        home: SyncDevicesScreen(
          library: library,
          profile: const RelayProfile(id: 'local', url: null),
          onServer: () => openedServer = true,
        ),
      ),
    );
    expect(find.text('Sync not set up'), findsOneWidget);
    expect(find.byKey(const Key('account-yes')), findsOneWidget);
    expect(find.text('Up to date'), findsNothing);
    await tester.tap(find.byKey(const Key('set-up-sync')));
    expect(openedServer, isTrue);
  });

  testWidgets('manual refresh is available only for relay profiles', (
    tester,
  ) async {
    final library = FakeNoteLibrary();
    await tester.pumpWidget(
      MaterialApp(
        home: SyncDevicesScreen(
          library: library,
          profile: relay,
          onServer: () {},
        ),
      ),
    );
    await tester.tap(find.byKey(const Key('sync-now')));
    await tester.pumpAndSettle();
    expect(library.calls, contains('syncNow()'));

    await tester.pumpWidget(
      MaterialApp(
        home: SyncDevicesScreen(
          library: library,
          profile: const RelayProfile(id: 'local', url: null),
          onServer: () {},
        ),
      ),
    );
    expect(
      tester.widget<TextButton>(find.byKey(const Key('sync-now'))).onPressed,
      isNull,
    );
  });

  testWidgets('recovery can be started from the combined screen', (
    tester,
  ) async {
    final library = FakeNoteLibrary();
    await tester.pumpWidget(
      MaterialApp(
        home: SyncDevicesScreen(
          library: library,
          profile: relay,
          onServer: () {},
        ),
      ),
    );
    final adopt = find.byKey(const Key('adopt-account-button'));
    await tester.ensureVisible(adopt);
    await tester.pumpAndSettle();
    await tester.tap(adopt);
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('adopt-phrase-field')), findsOneWidget);
  });
}
