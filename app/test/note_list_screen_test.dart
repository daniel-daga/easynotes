/// Widget tests for the note list, against a faked library.
///
/// The same question as the checklist's tests, one level up: is the grid a
/// function of the list it is given? A note made, a note joined and a note
/// edited by someone else must all reach it the same way, and a tap must open
/// the note it is on rather than the note the screen thinks it is on.
library;

import 'package:easynotes/core/fake_note_core.dart';
import 'package:easynotes/core/fake_note_library.dart';
import 'package:easynotes/core/note_library.dart';
import 'package:easynotes/core/note_view.dart';
import 'package:easynotes/core/sync_status.dart';
import 'package:easynotes/ui/note_list_screen.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

Future<void> pumpList(WidgetTester tester, FakeNoteLibrary library) async {
  await tester.pumpWidget(MaterialApp(home: NoteListScreen(library: library)));
  await tester.pumpAndSettle();
}

FakeNoteLibrary twoLists() => FakeNoteLibrary(
  notes: {
    'groceries': FakeNoteCore(
      title: 'Groceries',
      items: const ['milk', 'bread'],
    ),
    'hardware': FakeNoteCore(title: 'Hardware', items: const ['screws']),
  },
);

void main() {
  testWidgets('shows a card per note, newest first', (tester) async {
    await pumpList(tester, twoLists());

    expect(find.text('Groceries'), findsOneWidget);
    expect(find.text('Hardware'), findsOneWidget);
    // The preview is what makes a card readable when the note has no title.
    expect(find.text('• milk'), findsOneWidget);
    expect(find.text('0 of 2 done'), findsOneWidget);

    // Newest first is a claim about what the user sees, so it is checked
    // against where the cards landed rather than against the order of a list.
    expect(
      tester.getTopLeft(find.byKey(const Key('note:hardware'))).dx,
      lessThan(tester.getTopLeft(find.byKey(const Key('note:groceries'))).dx),
      reason: 'the newest note should come first',
    );
  });

  testWidgets('says so when there is nothing yet', (tester) async {
    await pumpList(tester, FakeNoteLibrary());
    expect(find.byKey(const Key('no-notes')), findsOneWidget);
  });

  testWidgets('an edit from another device changes the card', (tester) async {
    // The point of the list being a stream: nothing here asked for this change,
    // and the grid still shows it.
    final library = twoLists();
    await pumpList(tester, library);

    library
        .core('groceries')
        .pushRemote(
          const NoteView(
            title: 'Groceries',
            items: [
              ItemView(id: 'a', text: 'milk', checked: true, order: '0000'),
              ItemView(id: 'b', text: 'bread', checked: false, order: '0001'),
              ItemView(id: 'c', text: 'jam', checked: false, order: '0002'),
            ],
          ),
        );
    await tester.pumpAndSettle();

    expect(find.text('1 of 3 done'), findsOneWidget);
    expect(find.text('• jam'), findsOneWidget);
  });

  testWidgets('the new-note button makes one and opens it', (tester) async {
    final library = FakeNoteLibrary();
    await pumpList(tester, library);

    await tester.tap(find.byKey(const Key('new-note-button')));
    await tester.pumpAndSettle();

    expect(library.calls, contains('createNote()'));
    // On the checklist screen for the note it just made, ready to be typed into.
    expect(find.byKey(const Key('new-item-field')), findsOneWidget);
  });

  testWidgets('tapping a card opens that note', (tester) async {
    final library = twoLists();
    await pumpList(tester, library);

    await tester.tap(find.byKey(const Key('note:groceries')));
    await tester.pumpAndSettle();

    expect(library.calls, contains('openNote(groceries)'));
    expect(find.text('milk'), findsOneWidget);
  });

  testWidgets('pasting a share blob joins with it and opens the list', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);

    await tester.tap(find.byKey(const Key('join-button')));
    await tester.pumpAndSettle();
    await tester.enterText(
      find.byKey(const Key('join-blob-field')),
      '  AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyAhIiMkJSYnKCk  ',
    );
    await tester.tap(find.byKey(const Key('join-confirm')));
    await tester.pumpAndSettle();

    expect(
      library.calls,
      contains('join(AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyAhIiMkJSYnKCk)'),
    );
  });

  testWidgets('an empty paste joins nothing', (tester) async {
    final library = twoLists();
    await pumpList(tester, library);

    await tester.tap(find.byKey(const Key('join-button')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('join-confirm')));
    await tester.pumpAndSettle();

    expect(library.calls.where((c) => c.startsWith('join(')), isEmpty);
  });

  testWidgets('there is no join button where joining is not possible', (
    tester,
  ) async {
    await pumpList(tester, FakeNoteLibrary(canJoin: false));
    expect(find.byKey(const Key('join-button')), findsNothing);
  });

  testWidgets('the layout toggle switches between the grid and a list', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);

    // Both layouts show every note; what changes is how wide a card is, so that
    // is what this checks rather than the icon.
    final inGrid = tester.getSize(find.byKey(const Key('note:hardware'))).width;
    await tester.tap(find.byKey(const Key('layout-toggle')));
    await tester.pumpAndSettle();
    final inList = tester.getSize(find.byKey(const Key('note:hardware'))).width;

    expect(find.byKey(const Key('note:groceries')), findsOneWidget);
    expect(inList, greaterThan(inGrid));
  });

  testWidgets('the sync button is the whole device, not one note', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);

    library.pushStatus(const SyncStatus(state: SyncState.offline, pending: 3));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('sync-status')));
    await tester.pumpAndSettle();

    expect(find.text('Not syncing, 3 edits waiting'), findsOneWidget);
  });

  test('a summary is a value', () {
    const a = NoteSummary(id: 'a', title: 'x', items: 1, checked: 0);
    const b = NoteSummary(id: 'a', title: 'x', items: 1, checked: 0);
    expect(a, b);
  });
}
