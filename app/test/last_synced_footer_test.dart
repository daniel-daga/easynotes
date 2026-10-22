/// The "last synced" line, and the Sync & devices card it sits in.
library;

import 'package:easynotes/core/fake_note_library.dart';
import 'package:easynotes/core/relay_profiles.dart';
import 'package:easynotes/core/sync_status.dart';
import 'package:easynotes/ui/last_synced_footer.dart';
import 'package:easynotes/ui/note_list_screen.dart';
import 'package:easynotes/ui/note_screen.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  group('lastSyncedLabel', () {
    final at = DateTime(2026, 10, 22, 15, 42);

    test('is "just now" under a minute', () {
      expect(
        lastSyncedLabel(at, at.add(const Duration(seconds: 30))),
        'Last synced just now',
      );
    });

    test('counts minutes under an hour', () {
      expect(
        lastSyncedLabel(at, at.add(const Duration(minutes: 5))),
        'Last synced 5m ago',
      );
    });

    test('counts hours and minutes under three hours', () {
      expect(
        lastSyncedLabel(at, at.add(const Duration(hours: 1, minutes: 20))),
        'Last synced 1h 20m ago',
      );
    });

    test('turns into a clock time at three hours', () {
      expect(
        lastSyncedLabel(at, at.add(const Duration(hours: 3))),
        'Last synced at 3:42 PM',
      );
    });

    test('names the day when the clock time is not today', () {
      final late = DateTime(2026, 10, 22, 23, 50);
      expect(
        lastSyncedLabel(late, DateTime(2026, 10, 23, 4)),
        'Last synced at 11:50 PM yesterday',
      );
    });
  });

  testWidgets('hidden without a relay, even when a time is known', (
    tester,
  ) async {
    final library = FakeNoteLibrary();
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: LastSyncedFooter(
            hasRelay: false,
            initial: DateTime.now(),
            watch: library.watchLastSyncedAt,
          ),
        ),
      ),
    );
    expect(find.byKey(const Key('last-synced')), findsNothing);
  });

  testWidgets('stays hidden until the first sync, then appears', (
    tester,
  ) async {
    final library = FakeNoteLibrary();
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: LastSyncedFooter(
            hasRelay: true,
            initial: library.lastSyncedAt,
            watch: library.watchLastSyncedAt,
          ),
        ),
      ),
    );
    expect(find.byKey(const Key('last-synced')), findsNothing);

    library.pushStatus(const SyncStatus(state: SyncState.synced));
    await tester.pump();

    expect(find.byKey(const Key('last-synced')), findsOneWidget);
    expect(find.textContaining('Last synced'), findsOneWidget);
  });

  testWidgets('the note list leaves it to Sync & devices', (tester) async {
    final library = FakeNoteLibrary();
    await tester.pumpWidget(
      MaterialApp(
        home: NoteListScreen(
          library: library,
          profile: const RelayProfile(id: 'relay', url: 'https://relay.test'),
        ),
      ),
    );
    await tester.pumpAndSettle();

    library.pushStatus(const SyncStatus(state: SyncState.synced));
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('last-synced')), findsNothing);
  });

  testWidgets('the note screen leaves it to Sync & devices', (tester) async {
    final library = FakeNoteLibrary();
    final id = await library.createNote();
    await tester.pumpWidget(
      MaterialApp(
        home: NoteScreen(
          core: library.core(id),
          storage: FakeStorageHealth(),
          noteId: id,
          invites: library,
          library: library,
        ),
      ),
    );
    await tester.pumpAndSettle();

    library.pushStatus(const SyncStatus(state: SyncState.synced));
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('last-synced')), findsNothing);
  });
}
