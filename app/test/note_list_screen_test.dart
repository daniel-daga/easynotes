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
import 'package:easynotes/ui/invites.dart';
import 'package:easynotes/ui/note_list_screen.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

Future<void> pumpList(WidgetTester tester, FakeNoteLibrary library) async {
  await tester.pumpWidget(MaterialApp(home: NoteListScreen(library: library)));
  await tester.pumpAndSettle();
}

/// The shelf the screen is on. The app bar is a search launcher rather than a
/// heading, so the shelf is read from the key the launcher is given for it.
Finder onShelf(String title) => find.byKey(ValueKey('search-launcher:$title'));

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
  testWidgets('People filters are one per person and update after a removal', (
    tester,
  ) async {
    final library = twoLists()..setUpSharing();
    const anna = Person(member: 'anna', invite: 'i1', name: 'Anna');
    library.setPeople(
      'groceries',
      const People(
        shared: true,
        canManage: true,
        people: [
          Person(member: 'me', invite: '', name: 'Olga', owner: true, me: true),
          anna,
        ],
      ),
    );
    // Another account that also chose "Anna": another person.
    library.setPeople(
      'hardware',
      const People(
        shared: true,
        canManage: true,
        people: [
          Person(member: 'me', invite: '', name: 'Olga', owner: true, me: true),
          Person(member: 'another-anna', invite: 'i2', name: 'Anna'),
        ],
      ),
    );
    await pumpList(tester, library);
    await tester.tap(find.byKey(const Key('search-button')));
    await tester.pumpAndSettle();
    await tester.ensureVisible(find.byKey(const Key('filter-person-anna')));
    await tester.tap(find.byKey(const Key('filter-person-anna')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('note:groceries')), findsOneWidget);
    expect(find.byKey(const Key('note:hardware')), findsNothing);
    await library.removePerson('groceries', anna);
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('note:groceries')), findsNothing);
  });
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

  testWidgets('the new-note button makes a text note and opens it', (
    tester,
  ) async {
    final library = FakeNoteLibrary();
    await pumpList(tester, library);

    await tester.tap(find.byKey(const Key('new-note-button')));
    await tester.pumpAndSettle();

    expect(library.calls, contains('createNote(text)'));
    // On the screen for the note it just made, ready to be typed into.
    expect(find.byKey(const Key('body-text')), findsOneWidget);
    expect(find.byKey(const Key('new-item-field')), findsNothing);
  });

  testWidgets('the new-list button makes a checklist and opens it', (
    tester,
  ) async {
    final library = FakeNoteLibrary();
    await pumpList(tester, library);

    await tester.tap(find.byKey(const Key('new-list-button')));
    await tester.pumpAndSettle();

    expect(library.calls, contains('createNote()'));
    expect(find.byKey(const Key('add-list-entry')), findsOneWidget);
  });

  testWidgets('a text note\'s card shows its lines, and no tally', (
    tester,
  ) async {
    final library = FakeNoteLibrary(
      notes: {
        'memo': FakeNoteCore(
          kind: NoteKind.text,
          title: 'Plumber',
          body: 'Call on Monday\n\nabout the boiler',
        ),
      },
    );
    await pumpList(tester, library);

    expect(find.text('Call on Monday'), findsOneWidget);
    expect(find.text('about the boiler'), findsOneWidget);
    expect(find.textContaining('•'), findsNothing);
    expect(find.textContaining('done'), findsNothing);
  });

  testWidgets('tapping a card opens that note', (tester) async {
    final library = twoLists();
    await pumpList(tester, library);

    await tester.tap(find.byKey(const Key('note:groceries')));
    await tester.pumpAndSettle();

    expect(library.calls, contains('openNote(groceries)'));
    expect(find.text('milk'), findsOneWidget);
  });

  group('joining', () {
    Future<void> settleWhileWaiting(WidgetTester tester) async {
      for (var i = 0; i < 10; i++) {
        await tester.pump(const Duration(milliseconds: 20));
      }
    }

    setUp(() {
      invitePollInterval = const Duration(milliseconds: 100);
      inviteBannerPollInterval = const Duration(milliseconds: 200);
    });
    tearDown(() => joinAlerts = null);

    Future<void> ask(WidgetTester tester, String link) async {
      await tester.tap(find.byTooltip('Open navigation menu'));
      await tester.pumpAndSettle();
      await tester.tap(find.byKey(const Key('join-entry')));
      await tester.pumpAndSettle();
      await tester.enterText(find.byKey(const Key('join-link-field')), link);
      // The name is the account's: the button says it, and nothing asks.
      expect(find.text('Join as Anna'), findsOneWidget);
      await tester.tap(find.byKey(const Key('join-confirm')));
      // Not `pumpAndSettle`: the waiting dialog's progress bar never settles,
      // which is the point of it.
      await settleWhileWaiting(tester);
    }

    testWidgets('a link joins, waits, and opens the list once let in', (
      tester,
    ) async {
      final library = twoLists()..setUpSharing('Anna');
      await pumpList(tester, library);

      await ask(tester, '  easynotes://join#abc  ');

      expect(library.calls, contains('requestJoin(easynotes://join#abc)'));
      // No code to read out: there is nobody to read it to.
      expect(find.byKey(const Key('join-waiting')), findsOneWidget);

      library.fakeAnswer('abc', JoinState.joined);
      await tester.pump(invitePollInterval);
      await tester.pumpAndSettle();

      expect(
        library.calls.where((c) => c.startsWith('openNote(joined-')),
        hasLength(1),
      );
      expect(find.byKey(const Key('join-waiting')), findsNothing);
    });

    testWidgets('a tapped link only asks for a yes', (tester) async {
      final library = twoLists()..setUpSharing('Anna');
      await tester.pumpWidget(
        MaterialApp(
          home: NoteListScreen(
            library: library,
            initialInviteLink: 'easynotes://join#abc',
          ),
        ),
      );
      await tester.pumpAndSettle();

      // Nothing to paste or check: the link is already in hand.
      expect(find.byKey(const Key('join-link-field')), findsNothing);
      expect(find.byKey(const Key('join-explainer')), findsOneWidget);
      await tester.tap(find.byKey(const Key('join-confirm')));
      await settleWhileWaiting(tester);

      expect(library.calls, contains('requestJoin(easynotes://join#abc)'));
      expect(find.byKey(const Key('join-waiting')), findsOneWidget);
      await tester.tap(find.byKey(const Key('join-stop-waiting')));
      await settleWhileWaiting(tester);
    });

    testWidgets('there is no join button on the grid, only in the drawer', (
      tester,
    ) async {
      await pumpList(tester, twoLists());
      expect(find.byKey(const Key('join-button')), findsNothing);
      await tester.tap(find.byTooltip('Open navigation menu'));
      await tester.pumpAndSettle();
      expect(find.byKey(const Key('join-entry')), findsOneWidget);
    });

    testWidgets('an empty link asks nothing', (tester) async {
      final library = twoLists()..setUpSharing('Anna');
      await pumpList(tester, library);

      await ask(tester, '   ');

      expect(library.calls.where((c) => c.startsWith('requestJoin')), isEmpty);
    });

    testWidgets('a link that is not one says why and stays open to fix', (
      tester,
    ) async {
      final library = twoLists()..setUpSharing('Anna');
      await pumpList(tester, library);

      await ask(tester, 'AQIDBAUGBwgJCgsMDQ4PEBES');

      expect(find.byKey(const Key('join-error')), findsOneWidget);
      expect(find.textContaining('not an invite link'), findsOneWidget);
      expect(find.byKey(const Key('join-link-field')), findsOneWidget);
    });

    testWidgets('being turned away says so', (tester) async {
      final library = twoLists()..setUpSharing('Anna');
      await pumpList(tester, library);
      await ask(tester, 'easynotes://join#abc');

      library.fakeAnswer('abc', JoinState.declined);
      await tester.pump(invitePollInterval);
      await tester.pumpAndSettle();

      expect(find.byKey(const Key('join-ended')), findsOneWidget);
      expect(find.textContaining('Ask them for a new link'), findsOneWidget);
    });

    testWidgets('stopping the wait cancels the request', (tester) async {
      final library = twoLists()..setUpSharing('Anna');
      await pumpList(tester, library);
      await ask(tester, 'easynotes://join#abc');

      await tester.tap(find.byKey(const Key('join-stop-waiting')));
      await settleWhileWaiting(tester);

      expect(library.calls, contains('cancelJoin(abc)'));
      expect(library.pendingJoins, isEmpty);
    });

    testWidgets('a request left waiting is let in from the banner', (
      tester,
    ) async {
      final library = twoLists()..setUpSharing('Anna');
      await library.requestJoin('easynotes://join#abc');
      await pumpList(tester, library);

      expect(find.byKey(const Key('join-banner-abc')), findsOneWidget);

      library.fakeAnswer('abc', JoinState.joined);
      await tester.pump(inviteBannerPollInterval);
      await tester.pumpAndSettle();

      expect(find.byKey(const Key('joined-in-background')), findsOneWidget);
      expect(find.byKey(const Key('join-banner-abc')), findsNothing);
    });

    testWidgets(
      'someone who opened a link is let in and shown above the grid',
      (tester) async {
        final alerts = joinAlerts = RecordingJoinAlerts();
        final library = twoLists()..setUpSharing('Olga');
        final invite = await library.createInvite('groceries');
        library.fakeOpened(invite.id, name: 'Anna');
        await pumpList(tester, library);

        // Nobody was asked: the grid's own poll let her in, and told the
        // owner — once, however many polls follow.
        expect(find.text('Anna joined "Groceries"'), findsOneWidget);
        await tester.pump(inviteBannerPollInterval * 3);
        expect(alerts.calls, ['joined(${invite.id}, groceries, Anna)']);
        expect(
          library.people('groceries').people.map((p) => p.name),
          contains('Anna'),
        );
        expect(library.allInvites, isEmpty);

        await tester.tap(find.text('View'));
        await tester.pumpAndSettle();

        expect(find.text('Anna joined "Groceries"'), findsNothing);
        expect(find.byKey(Key('person-member-${invite.id}')), findsOneWidget);
        // Closed before the test ends, so its poll does not outlive it.
        await tester.tap(find.byKey(const Key('share-done')));
        await tester.pumpAndSettle();
      },
    );
  });

  testWidgets('there is no way to join where joining is not possible', (
    tester,
  ) async {
    await pumpList(tester, FakeNoteLibrary(canJoin: false));
    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('join-entry')), findsNothing);
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

  testWidgets('sync status is available in the home drawer', (tester) async {
    final library = twoLists();
    await pumpList(tester, library);

    library.pushStatus(const SyncStatus(state: SyncState.offline, pending: 3));
    await tester.pumpAndSettle();
    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    // The drawer says it in a line, and the tile opens the same sentence.
    expect(
      find.descendant(
        of: find.byKey(const Key('sync-status')),
        matching: find.text('Not syncing, 3 edits waiting'),
      ),
      findsOneWidget,
    );
    await tester.tap(find.byKey(const Key('sync-status')));
    await tester.pumpAndSettle();

    expect(
      find.descendant(
        of: find.byType(AlertDialog),
        matching: find.text('Not syncing, 3 edits waiting'),
      ),
      findsOneWidget,
    );
  });

  testWidgets('a layout chosen on another device is drawn without a restart', (
    tester,
  ) async {
    // Found on two emulators: the second device adopted the account, got the
    // first one's pins live, and kept drawing a grid until it was relaunched,
    // because the screen had read the layout once when it opened.
    final library = twoLists();
    await pumpList(tester, library);
    final inGrid = tester.getSize(find.byKey(const Key('note:hardware'))).width;

    // The account document changes underneath the screen, and the list is
    // refreshed — which is what the core does for any remote account edit.
    await library.setLayout(NoteLayout.list);
    library
        .core('groceries')
        .pushRemote(
          const NoteView(
            title: 'Groceries',
            items: [
              ItemView(id: 'a', text: 'milk', checked: false, order: '0000'),
            ],
          ),
        );
    await tester.pumpAndSettle();

    expect(
      tester.getSize(find.byKey(const Key('note:hardware'))).width,
      greaterThan(inGrid),
      reason: 'still drawn as a grid after the account said list',
    );
  });

  testWidgets('the layout it was left in is the layout it opens in', (
    tester,
  ) async {
    // The hole the account document fills: a view setting that lasts as long as
    // the screen is not a setting. The library is asked what it remembers, and
    // told when that changes.
    final library = twoLists();
    await pumpList(tester, library);
    final inGrid = tester.getSize(find.byKey(const Key('note:hardware'))).width;

    await tester.tap(find.byKey(const Key('layout-toggle')));
    await tester.pumpAndSettle();
    expect(library.calls, contains('setLayout(list)'));

    // A second screen over the same library — a relaunch, as far as this is
    // concerned. It asks what was chosen rather than starting again at a grid.
    await pumpList(tester, library);
    final reopened = tester
        .getSize(find.byKey(const Key('note:hardware')))
        .width;
    expect(
      reopened,
      greaterThan(inGrid),
      reason: 'it opened as a grid, having been left as a list',
    );
  });

  // -- placements -----------------------------------------------------------

  testWidgets('holding a card offers what can be done to it', (tester) async {
    final library = twoLists();
    await pumpList(tester, library);

    await tester.longPress(find.byKey(const Key('note:groceries')));
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('note-actions')), findsOneWidget);
    expect(find.text('Pin'), findsOneWidget);
    expect(find.text('Archive'), findsOneWidget);
    expect(find.text('Move to trash'), findsOneWidget);
  });

  /// Pick a card up and let go over another one, the way a finger does.
  ///
  /// Not `tester.drag`: a `LongPressDraggable` has to be held before it moves,
  /// and a drag with no hold in it is a scroll.
  Future<void> dragCard(WidgetTester tester, String from, String onto) async {
    final gesture = await tester.startGesture(
      tester.getCenter(find.byKey(Key('note:$from'))),
    );
    await tester.pump(kLongPressTimeout + const Duration(milliseconds: 50));
    await gesture.moveTo(tester.getCenter(find.byKey(Key('note:$onto'))));
    await tester.pump();
    await gesture.up();
    await tester.pumpAndSettle();
  }

  testWidgets('dragging a card onto another moves it there', (tester) async {
    final library = twoLists();
    await pumpList(tester, library);
    // Hardware is the newer, so it starts first.
    expect(
      tester.getTopLeft(find.byKey(const Key('note:hardware'))).dx,
      lessThan(tester.getTopLeft(find.byKey(const Key('note:groceries'))).dx),
    );

    await dragCard(tester, 'hardware', 'groceries');

    // The library is told which two notes, not which two positions: the grid
    // may be a shelf or a label and the order underneath is over every note.
    expect(library.calls, contains('moveNoteTo(hardware, groceries)'));
    expect(
      tester.getTopLeft(find.byKey(const Key('note:groceries'))).dx,
      lessThan(tester.getTopLeft(find.byKey(const Key('note:hardware'))).dx),
      reason:
          'the dragged card should have landed after the one it was dropped on',
    );
  });

  testWidgets('a card released without moving still opens its menu', (
    tester,
  ) async {
    // The gesture does two jobs, and this is the one that was there first.
    final library = twoLists();
    await pumpList(tester, library);

    await tester.longPress(find.byKey(const Key('note:groceries')));
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('note-actions')), findsOneWidget);
    expect(library.calls, isNot(contains(startsWith('moveNoteTo'))));
  });

  testWidgets('a card dragged across the pin line does not move', (
    tester,
  ) async {
    // A pin lifts a note out of the sequence it shares with the rest, so a move
    // between the two groups is one nothing on screen could show.
    final library = twoLists();
    await pumpList(tester, library);
    await tester.longPress(find.byKey(const Key('note:groceries')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('action-pin')));
    await tester.pumpAndSettle();
    library.calls.clear();

    await dragCard(tester, 'hardware', 'groceries');

    expect(library.calls, isNot(contains(startsWith('moveNoteTo'))));
    // And it did not quietly turn into a menu either: the drag went somewhere.
    expect(find.byKey(const Key('note-actions')), findsNothing);
  });

  testWidgets('a card dragged in the single column moves the same way', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);
    await tester.tap(find.byKey(const Key('layout-toggle')));
    await tester.pumpAndSettle();

    await dragCard(tester, 'hardware', 'groceries');

    expect(library.calls, contains('moveNoteTo(hardware, groceries)'));
    expect(
      tester.getTopLeft(find.byKey(const Key('note:groceries'))).dy,
      lessThan(tester.getTopLeft(find.byKey(const Key('note:hardware'))).dy),
    );
  });

  testWidgets('search results are not reordered by dragging', (tester) async {
    // The cards on screen are the answer to a question, and most of the order
    // they would be moved in is not being shown.
    final library = twoLists();
    await pumpList(tester, library);
    await tester.tap(find.byKey(const Key('search-button')));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('search-field')), 'e');
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('note:groceries')), findsOneWidget);
    expect(find.byKey(const Key('note:hardware')), findsOneWidget);

    await dragCard(tester, 'hardware', 'groceries');

    expect(library.calls, isNot(contains(startsWith('moveNoteTo'))));
    // The hold still reaches the card's menu, which is a result's only gesture.
    expect(find.byKey(const Key('note-actions')), findsOneWidget);
  });

  testWidgets('colouring a note tells the library and paints the card', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);

    await tester.longPress(find.byKey(const Key('note:groceries')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('swatch-teal')));
    await tester.pumpAndSettle();

    expect(library.calls, contains('setColour(groceries, teal)'));
    final card = tester.widget<Card>(
      find.ancestor(
        of: find.byKey(const Key('note:groceries')),
        matching: find.byType(Card),
      ),
    );
    expect(card.color, isNotNull, reason: 'the card was not painted');
  });

  testWidgets('a pinned note goes to the top and says that it is pinned', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);
    // Groceries is the older of the two, so it starts second.
    expect(
      tester.getTopLeft(find.byKey(const Key('note:groceries'))).dx,
      greaterThan(tester.getTopLeft(find.byKey(const Key('note:hardware'))).dx),
    );

    await tester.longPress(find.byKey(const Key('note:groceries')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('action-pin')));
    await tester.pumpAndSettle();

    expect(library.calls, contains('setPinned(groceries, true)'));
    expect(find.byKey(const Key('pinned')), findsOneWidget);
    expect(
      tester.getTopLeft(find.byKey(const Key('note:groceries'))).dx,
      lessThan(tester.getTopLeft(find.byKey(const Key('note:hardware'))).dx),
      reason: 'a pinned note belongs at the top',
    );
  });

  testWidgets('an archived note leaves the grid and is on the archive shelf', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);

    await tester.longPress(find.byKey(const Key('note:groceries')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('action-archive')));
    await tester.pumpAndSettle();

    expect(library.calls, contains('setArchived(groceries, true)'));
    expect(find.byKey(const Key('note:groceries')), findsNothing);

    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('shelf-archive')));
    await tester.pumpAndSettle();

    expect(onShelf('Archive'), findsOneWidget);
    expect(find.byKey(const Key('note:groceries')), findsOneWidget);
    expect(find.byKey(const Key('note:hardware')), findsNothing);
    // Nothing is made where it would not be seen.
    expect(find.byKey(const Key('new-note-button')), findsNothing);
  });

  testWidgets('a trashed note can be taken back out', (tester) async {
    final library = twoLists();
    await pumpList(tester, library);

    await tester.longPress(find.byKey(const Key('note:hardware')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('action-trash')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('note:hardware')), findsNothing);

    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('shelf-trash')));
    await tester.pumpAndSettle();

    await tester.longPress(find.byKey(const Key('note:hardware')));
    await tester.pumpAndSettle();
    // In the trash a note is restored, not pinned or archived.
    expect(find.byKey(const Key('action-pin')), findsNothing);
    expect(find.text('Restore'), findsOneWidget);
    await tester.tap(find.byKey(const Key('action-trash')));
    await tester.pumpAndSettle();

    expect(library.calls, contains('setTrashed(hardware, false)'));
    expect(find.byKey(const Key('no-notes')), findsOneWidget);
  });

  // -- deleting for good ----------------------------------------------------

  /// Long-press a card and take the sheet's `key` action.
  Future<void> holdAndTap(WidgetTester tester, String note, String key) async {
    await tester.longPress(find.byKey(Key('note:$note')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(Key(key)));
    await tester.pumpAndSettle();
  }

  Future<void> openShelf(WidgetTester tester, String shelf) async {
    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(Key('shelf-$shelf')));
    await tester.pumpAndSettle();
  }

  testWidgets('deleting for good is offered from the trash and nowhere else', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);

    // Not on a note in the grid: throwing it away comes first, and that is the
    // gesture that can be taken back.
    await tester.longPress(find.byKey(const Key('note:hardware')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('action-delete-forever')), findsNothing);
    await tester.tap(find.byKey(const Key('action-trash')));
    await tester.pumpAndSettle();

    // Nor on an archived note, which is out of the way rather than thrown away.
    await holdAndTap(tester, 'groceries', 'action-archive');
    await openShelf(tester, 'archive');
    await tester.longPress(find.byKey(const Key('note:groceries')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('action-delete-forever')), findsNothing);
    await tester.tap(find.byKey(const Key('action-archive')));
    await tester.pumpAndSettle();

    await openShelf(tester, 'trash');
    await tester.longPress(find.byKey(const Key('note:hardware')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('action-delete-forever')), findsOneWidget);
  });

  testWidgets(
    'deleting for good asks first, and keeping the note means keeping '
    'it',
    (tester) async {
      final library = twoLists();
      await pumpList(tester, library);
      await holdAndTap(tester, 'hardware', 'action-trash');
      await openShelf(tester, 'trash');

      await holdAndTap(tester, 'hardware', 'action-delete-forever');
      expect(find.byKey(const Key('confirm-delete')), findsOneWidget);
      // The one thing the app cannot do is said on the dialog that does it.
      expect(
        find.textContaining('People you shared this note with keep their'),
        findsOneWidget,
      );

      await tester.tap(find.byKey(const Key('confirm-keep')));
      await tester.pumpAndSettle();

      expect(library.calls, isNot(contains('forgetNote(hardware)')));
      expect(find.byKey(const Key('note:hardware')), findsOneWidget);
    },
  );

  testWidgets('a note deleted for good leaves the device', (tester) async {
    final library = twoLists();
    await pumpList(tester, library);
    await holdAndTap(tester, 'hardware', 'action-trash');
    await openShelf(tester, 'trash');

    await holdAndTap(tester, 'hardware', 'action-delete-forever');
    await tester.tap(find.byKey(const Key('confirm-delete-forever')));
    await tester.pumpAndSettle();

    expect(library.calls, contains('forgetNote(hardware)'));
    expect(find.byKey(const Key('no-notes')), findsOneWidget);
    // Gone from the whole device, not moved to another shelf: the grid it was
    // thrown away from does not have it back.
    await openShelf(tester, 'notes');
    expect(find.byKey(const Key('note:hardware')), findsNothing);
    expect(find.byKey(const Key('note:groceries')), findsOneWidget);
  });

  testWidgets('emptying the trash is offered only when there is a trash to '
      'empty', (tester) async {
    final library = twoLists();
    await pumpList(tester, library);

    // Not on the grid, where it would delete nothing that is on screen.
    expect(find.byKey(const Key('empty-trash-button')), findsNothing);

    await openShelf(tester, 'trash');
    // An empty trash offers no button to empty it: the one gesture that cannot
    // be undone is not worth offering for no result.
    expect(find.byKey(const Key('empty-trash-button')), findsNothing);

    await openShelf(tester, 'notes');
    await holdAndTap(tester, 'hardware', 'action-trash');
    await openShelf(tester, 'trash');
    expect(find.byKey(const Key('empty-trash-button')), findsOneWidget);
  });

  testWidgets('emptying the trash takes the trash and leaves the rest', (
    tester,
  ) async {
    final library = FakeNoteLibrary(
      notes: {
        'groceries': FakeNoteCore(title: 'Groceries'),
        'hardware': FakeNoteCore(title: 'Hardware'),
        'old': FakeNoteCore(title: 'Old'),
      },
    );
    await pumpList(tester, library);
    await holdAndTap(tester, 'hardware', 'action-trash');
    await holdAndTap(tester, 'old', 'action-trash');
    await openShelf(tester, 'trash');

    await tester.tap(find.byKey(const Key('empty-trash-button')));
    await tester.pumpAndSettle();
    expect(find.text('Delete all 2 notes permanently?'), findsOneWidget);
    await tester.tap(find.byKey(const Key('confirm-delete-forever')));
    await tester.pumpAndSettle();

    expect(library.calls, contains('emptyTrash()'));
    expect(find.byKey(const Key('no-notes')), findsOneWidget);
    await openShelf(tester, 'notes');
    expect(find.byKey(const Key('note:groceries')), findsOneWidget);
    expect(find.byKey(const Key('note:hardware')), findsNothing);
    expect(find.byKey(const Key('note:old')), findsNothing);
  });

  testWidgets('a deletion that fails says so and leaves the trash', (
    tester,
  ) async {
    // The error has to reach the person rather than escape the button, where
    // on the web it was an uncaught page error and nothing on the screen.
    final library = FakeNoteLibrary(
      notes: {
        'groceries': FakeNoteCore(title: 'Groceries'),
        'hardware': FakeNoteCore(title: 'Hardware'),
      },
    )..refuseDeletion = StateError('out of space');
    await pumpList(tester, library);
    await holdAndTap(tester, 'hardware', 'action-trash');
    await openShelf(tester, 'trash');

    await tester.tap(find.byKey(const Key('empty-trash-button')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('confirm-delete-forever')));
    await tester.pumpAndSettle();
    expect(library.calls, contains('emptyTrash()'));
    expect(find.byKey(const Key('deletion-failed')), findsOneWidget);
    expect(find.textContaining('out of space'), findsOneWidget);
    expect(find.byKey(const Key('note:hardware')), findsOneWidget);

    ScaffoldMessenger.of(
      tester.element(find.byKey(const Key('note:hardware'))),
    ).removeCurrentSnackBar();
    await tester.pumpAndSettle();
    await holdAndTap(tester, 'hardware', 'action-delete-forever');
    await tester.tap(find.byKey(const Key('confirm-delete-forever')));
    await tester.pumpAndSettle();
    expect(library.calls, contains('forgetNote(hardware)'));
    expect(find.byKey(const Key('deletion-failed')), findsOneWidget);
    expect(find.byKey(const Key('note:hardware')), findsOneWidget);
  });

  testWidgets('each shelf says how many notes are on it', (tester) async {
    final library = twoLists();
    await pumpList(tester, library);
    await library.setArchived('groceries', true);
    await tester.pumpAndSettle();

    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();

    final counts = find.descendant(
      of: find.byKey(const Key('shelf-notes')),
      matching: find.text('1'),
    );
    expect(counts, findsOneWidget);
    expect(
      find.descendant(
        of: find.byKey(const Key('shelf-archive')),
        matching: find.text('1'),
      ),
      findsOneWidget,
    );
  });

  // -- labels ---------------------------------------------------------------

  /// Put a label on a note the way a person does: hold the card, open the
  /// dialog, type, add, close.
  Future<void> labelNote(WidgetTester tester, String id, String label) async {
    await tester.longPress(find.byKey(Key('note:$id')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('action-labels')));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('label-field')), label);
    await tester.tap(find.byKey(const Key('label-add')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('labels-done')));
    await tester.pumpAndSettle();
  }

  testWidgets(
    'labelling a note tells the library and puts a chip on the card',
    (tester) async {
      final library = twoLists();
      await pumpList(tester, library);

      await labelNote(tester, 'groceries', 'errands');

      expect(library.calls, contains('addTag(groceries, errands)'));
      expect(
        find.byKey(const Key('chip:groceries:errands')),
        findsOneWidget,
        reason: 'the label is part of what the card says',
      );
      // And only on the note it was put on.
      expect(find.byKey(const Key('chip:hardware:errands')), findsNothing);
    },
  );

  testWidgets('a label already on the device is offered rather than retyped', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);
    await labelNote(tester, 'groceries', 'errands');

    // The other note: the label exists now, so it is a checkbox and not a
    // second thing to spell.
    await tester.longPress(find.byKey(const Key('note:hardware')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('action-labels')));
    await tester.pumpAndSettle();
    final check = find.byKey(const Key('label-check-errands'));
    expect(check, findsOneWidget);
    expect(tester.widget<CheckboxListTile>(check).value, isFalse);

    await tester.tap(check);
    await tester.pumpAndSettle();
    expect(tester.widget<CheckboxListTile>(check).value, isTrue);

    // And off again, from the same checkbox. Taking a label off names the
    // label rather than the text on it, because the text is the part that can
    // change out from under a dialog that is already open.
    final errands = library.tags.single;
    await tester.tap(check);
    await tester.pumpAndSettle();
    expect(library.calls, contains('removeTag(hardware, ${errands.id})'));
    expect(tester.widget<CheckboxListTile>(check).value, isFalse);
  });

  testWidgets('a label that is only whitespace is not a label', (tester) async {
    final library = twoLists();
    await pumpList(tester, library);
    await labelNote(tester, 'groceries', '   ');

    // The field never reaches the library, so there is nothing to undo.
    expect(library.calls.where((call) => call.startsWith('addTag')), isEmpty);
    expect(library.tags, isEmpty);
  });

  testWidgets('a note can have, edit and remove a reminder', (tester) async {
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(
          const MethodChannel('flutter_timezone'),
          (call) async => 'Etc/UTC',
        );
    addTearDown(
      () => TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
          .setMockMethodCallHandler(
            const MethodChannel('flutter_timezone'),
            null,
          ),
    );
    final library = twoLists();
    await pumpList(tester, library);

    await tester.longPress(find.byKey(const Key('note:groceries')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('action-reminders')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('reminders-dialog')), findsOneWidget);
    expect(find.text('No reminders for this note.'), findsOneWidget);

    await tester.tap(find.byKey(const Key('add-reminder')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('save-reminder')));
    await tester.pumpAndSettle();

    expect(library.reminders, hasLength(1));
    final reminder = library.reminders.single;
    expect(reminder.noteId, 'groceries');
    expect(
      library.calls,
      contains('createReminder(groceries, ${reminder.id})'),
    );

    await tester.tap(find.byKey(Key('forget-reminder-${reminder.id}')));
    await tester.pumpAndSettle();
    expect(library.reminders, isEmpty);
    expect(library.calls, contains('forgetReminder(${reminder.id})'));
  });

  testWidgets('a label is a shelf of its own, with what is on it', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);
    await labelNote(tester, 'groceries', 'errands');

    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    final shelf = find.byKey(const Key('label-shelf-errands'));
    expect(shelf, findsOneWidget);
    expect(
      find.descendant(of: shelf, matching: find.text('1')),
      findsOneWidget,
      reason: 'a label says how many notes carry it',
    );

    await tester.tap(shelf);
    await tester.pumpAndSettle();

    // The shelf is the label, and it holds the one note that has it.
    expect(find.text('errands'), findsWidgets);
    expect(find.byKey(const Key('note:groceries')), findsOneWidget);
    expect(find.byKey(const Key('note:hardware')), findsNothing);
  });

  testWidgets('a note made on a label shelf carries that label', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);
    await labelNote(tester, 'groceries', 'errands');

    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('label-shelf-errands')));
    await tester.pumpAndSettle();

    // Otherwise it would be made, drawn, and gone on the next frame.
    await tester.tap(find.byKey(const Key('new-note-button')));
    await tester.pumpAndSettle();
    expect(library.calls, contains('addTag(note-0, errands)'));
  });

  testWidgets('a shelf whose last label came off stays, and says so', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);
    await labelNote(tester, 'groceries', 'errands');
    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('label-shelf-errands')));
    await tester.pumpAndSettle();

    // Taken off from underneath the screen, which is what another device doing
    // it would look like.
    final errands = library.tags.single;
    await library.removeTag('groceries', errands.id);
    await tester.pumpAndSettle();

    // The label still exists — it outlives the notes that carried it — so the
    // shelf is a real place to be standing, and it says what it is: empty.
    // This is the half that changed when a label stopped being its own name.
    expect(onShelf('errands'), findsOneWidget);
    expect(find.byKey(const Key('note:hardware')), findsNothing);
    expect(find.textContaining('Nothing here is labelled'), findsOneWidget);
  });

  testWidgets('a shelf whose label was thrown away goes back to the grid', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);
    await labelNote(tester, 'groceries', 'errands');
    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('label-shelf-errands')));
    await tester.pumpAndSettle();

    // Gone for good, rather than merely emptied.
    await library.forgetTag(library.tags.single.id);
    await tester.pumpAndSettle();

    // A shelf that does not exist cannot say why it is empty, so the screen
    // goes somewhere that can.
    expect(onShelf('EasyNotes'), findsOneWidget);
    expect(find.byKey(const Key('note:hardware')), findsOneWidget);
  });

  testWidgets('a label can be renamed from the drawer, and its notes keep it', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);
    await labelNote(tester, 'groceries', 'errands');

    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('label-menu-errands')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('label-rename')));
    await tester.pumpAndSettle();

    await tester.enterText(
      find.byKey(const Key('rename-label-field')),
      'Chores',
    );
    await tester.tap(find.byKey(const Key('rename-label-confirm')));
    await tester.pumpAndSettle();

    // One label, under its new name, still on the note that had it.
    expect(library.tags.single.name, 'Chores');
    expect(find.byKey(const Key('chip:groceries:Chores')), findsOneWidget);
  });

  testWidgets('renaming the label being looked at keeps the shelf', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);
    await labelNote(tester, 'groceries', 'errands');
    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('label-shelf-errands')));
    await tester.pumpAndSettle();

    // Renamed from underneath, as another device would.
    await library.renameTag(library.tags.single.id, 'Chores');
    await tester.pumpAndSettle();

    // The same shelf, under the new heading — not a bounce back to the grid.
    // The shelf is the label's identity, which a rename does not touch.
    expect(find.text('Chores'), findsWidgets);
    expect(find.byKey(const Key('note:groceries')), findsOneWidget);
    expect(find.byKey(const Key('note:hardware')), findsNothing);
  });

  testWidgets('a label can be thrown away from the drawer', (tester) async {
    final library = twoLists();
    await pumpList(tester, library);
    await labelNote(tester, 'groceries', 'errands');

    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('label-menu-errands')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('label-forget')));
    await tester.pumpAndSettle();

    // It says what it is about to do to notes that are not on screen.
    expect(find.textContaining('comes off 1 note'), findsOneWidget);
    await tester.tap(find.byKey(const Key('confirm-forget-label')));
    await tester.pumpAndSettle();

    expect(library.tags, isEmpty);
    // The note is untouched apart from losing the label.
    expect(find.byKey(const Key('note:groceries')), findsOneWidget);
  });

  testWidgets('search from a label includes notes outside that label', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);
    await labelNote(tester, 'groceries', 'errands');

    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('label-shelf-errands')));
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('search-button')));
    await tester.pumpAndSettle();
    // Search includes unlabelled notes too.
    await tester.enterText(find.byKey(const Key('search-field')), 'screws');
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('note:hardware')), findsOneWidget);

    await tester.enterText(find.byKey(const Key('search-field')), 'milk');
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('note:groceries')), findsOneWidget);
  });

  // -- search ---------------------------------------------------------------

  /// A list long enough that the interesting item is off the card, which is the
  /// case the matched line exists for.
  FakeNoteLibrary aLongList() => FakeNoteLibrary(
    notes: {
      'hardware': FakeNoteCore(title: 'Hardware', items: const ['screws']),
      'groceries': FakeNoteCore(
        title: 'Groceries',
        items: const ['milk', 'bread', 'eggs', 'coffee', 'asparagus'],
      ),
    },
  );

  Future<void> searchFor(WidgetTester tester, String query) async {
    await tester.tap(find.byKey(const Key('search-button')));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('search-field')), query);
    await tester.pumpAndSettle();
  }

  testWidgets('searching keeps the notes that match and drops the rest', (
    tester,
  ) async {
    final library = aLongList();
    await pumpList(tester, library);

    await searchFor(tester, 'groc');

    expect(library.calls, contains('search(groc)'));
    expect(find.byKey(const Key('note:groceries')), findsOneWidget);
    expect(find.byKey(const Key('note:hardware')), findsNothing);
    // Nothing is made from inside a search: it would not match it.
    expect(find.byKey(const Key('new-note-button')), findsNothing);
  });

  testWidgets('a match the card does not show is shown', (tester) async {
    await pumpList(tester, aLongList());
    await searchFor(tester, 'aspara');

    expect(find.byKey(const Key('note:groceries')), findsOneWidget);
    // The preview stops at four items and asparagus is the fifth, so without
    // this line the card would say nothing about why it is a result.
    expect(find.text('• asparagus'), findsNothing);
    expect(
      tester.widget<Text>(find.byKey(const Key('matched:groceries'))).data,
      'asparagus',
    );
  });

  testWidgets('a match on the title is not repeated under it', (tester) async {
    await pumpList(tester, aLongList());
    await searchFor(tester, 'hardware');

    expect(find.byKey(const Key('note:hardware')), findsOneWidget);
    expect(find.text('Hardware'), findsOneWidget);
  });

  testWidgets('a search that finds nothing says so', (tester) async {
    await pumpList(tester, aLongList());
    await searchFor(tester, 'cortado');

    expect(find.byKey(const Key('no-matches')), findsOneWidget);
    expect(find.byKey(const Key('note:groceries')), findsNothing);
  });

  testWidgets('closing the search brings every note back', (tester) async {
    await pumpList(tester, aLongList());
    await searchFor(tester, 'groc');
    expect(find.byKey(const Key('note:hardware')), findsNothing);

    await tester.tap(find.byKey(const Key('search-close')));
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('search-field')), findsNothing);
    expect(find.byKey(const Key('note:hardware')), findsOneWidget);
    expect(find.byKey(const Key('note:groceries')), findsOneWidget);
  });

  testWidgets('a search follows an edit from another device', (tester) async {
    // The same property the grid has, one layer further in: results are a
    // function of the notes, not a snapshot taken when the query was typed.
    final library = aLongList();
    await pumpList(tester, library);
    await searchFor(tester, 'cortado');
    expect(find.byKey(const Key('no-matches')), findsOneWidget);

    library
        .core('hardware')
        .pushRemote(
          const NoteView(
            title: 'Hardware',
            items: [
              ItemView(id: 'a', text: 'cortado', checked: false, order: '0000'),
            ],
          ),
        );
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('note:hardware')), findsOneWidget);
  });

  testWidgets('search includes Archive and Back always returns Home', (
    tester,
  ) async {
    final library = aLongList();
    await pumpList(tester, library);
    await library.setArchived('groceries', true);
    await tester.pumpAndSettle();

    // Archived, so not in the grid — and so not in a search of the grid.
    await searchFor(tester, 'groc');
    expect(find.byKey(const Key('note:groceries')), findsOneWidget);
    expect(find.text('Archived'), findsOneWidget);

    await tester.tap(find.byKey(const Key('search-close')));
    await tester.pumpAndSettle();
    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('shelf-archive')));
    await tester.pumpAndSettle();
    await searchFor(tester, 'groc');

    expect(find.byKey(const Key('note:groceries')), findsOneWidget);
    await tester.tap(find.byKey(const Key('search-close')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('note:hardware')), findsOneWidget);
    expect(find.byKey(const Key('note:groceries')), findsNothing);
  });

  for (final width in [360.0, 800.0]) {
    testWidgets('one search filter and reset at width $width', (tester) async {
      tester.view.physicalSize = Size(width, 740);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      final library = twoLists();
      await library.createNote(kind: NoteKind.text);
      await pumpList(tester, library);
      await tester.tap(find.byKey(const Key('search-button')));
      await tester.pumpAndSettle();
      expect(find.byKey(const Key('search-clear')), findsOneWidget);
      await tester.tap(find.byKey(const Key('search-clear')));
      await tester.pumpAndSettle();
      expect(find.byKey(const Key('search-filter-browser')), findsOneWidget);
      final hintY = tester.getCenter(find.text('Search notes and archive')).dy;
      await tester.tap(find.byKey(const Key('filter-type-checklist')));
      await tester.pump();
      // Check intermediate frames: settling alone misses a hint that jumps.
      for (final milliseconds in [0, 16, 50, 200]) {
        await tester.pump(Duration(milliseconds: milliseconds));
        expect(
          tester.getCenter(find.text('Search in Checklists')).dy,
          closeTo(hintY, 0.1),
        );
      }
      await tester.pumpAndSettle();
      expect(
        tester
            .widget<TextField>(find.byKey(const Key('search-field')))
            .decoration!
            .hintText,
        'Search in Checklists',
      );
      expect(find.byKey(const Key('search-filter-browser')), findsNothing);
      expect(find.byType(InputChip), findsNothing);
      expect(find.byType(ActionChip), findsNothing);
      expect(find.byIcon(Icons.arrow_drop_down), findsNothing);
      expect(find.text('Empty note'), findsNothing);
      await tester.enterText(find.byKey(const Key('search-field')), 'milk');
      await tester.pumpAndSettle();
      expect(find.byKey(const Key('note:groceries')), findsOneWidget);
      expect(find.byKey(const Key('note:hardware')), findsNothing);
      // Deleting just the text keeps the scope.
      await tester.enterText(find.byKey(const Key('search-field')), '');
      await tester.pumpAndSettle();
      expect(find.byKey(const Key('note:hardware')), findsOneWidget);
      expect(find.byKey(const Key('search-filter-browser')), findsNothing);
      await tester.enterText(find.byKey(const Key('search-field')), 'missing');
      await tester.pumpAndSettle();
      expect(find.byKey(const Key('no-matches')), findsOneWidget);
      // X resets both scope and text, even from no results.
      await tester.tap(find.byKey(const Key('search-clear')));
      await tester.pumpAndSettle();
      expect(
        tester
            .widget<TextField>(find.byKey(const Key('search-field')))
            .controller!
            .text,
        isEmpty,
      );
      expect(find.byKey(const Key('search-filter-browser')), findsOneWidget);
      await tester.tap(find.byKey(const Key('filter-type-text')));
      await tester.pumpAndSettle();
      expect(find.text('Empty note'), findsOneWidget);
      expect(find.byKey(const Key('note:groceries')), findsNothing);
      expect(
        tester.getCenter(find.text('Search in Notes')).dy,
        closeTo(
          tester.getCenter(find.byKey(const Key('search-field'))).dy,
          0.1,
        ),
      );
      expect(
        tester
            .widget<TextField>(find.byKey(const Key('search-field')))
            .decoration!
            .hintText,
        'Search in Notes',
      );
      await tester.tap(find.byKey(const Key('search-close')));
      await tester.pumpAndSettle();
      expect(find.byKey(const Key('search-field')), findsNothing);
      expect(find.byKey(const Key('note:groceries')), findsOneWidget);
      expect(tester.takeException(), isNull);
    });
  }

  testWidgets('label search and colour search do not combine', (tester) async {
    final library = twoLists();
    await library.setColour('hardware', NoteColour.red);
    await pumpList(tester, library);
    await labelNote(tester, 'groceries', 'errands');
    await tester.tap(find.byKey(const Key('search-button')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(Key('filter-label-${library.tags.single.id}')));
    await tester.pumpAndSettle();
    expect(
      tester
          .widget<TextField>(find.byKey(const Key('search-field')))
          .decoration!
          .hintText,
      'Search in errands',
    );
    expect(find.byKey(const Key('note:groceries')), findsOneWidget);
    expect(find.byKey(const Key('note:hardware')), findsNothing);
    await tester.tap(find.byKey(const Key('search-clear')));
    await tester.pumpAndSettle();
    await tester.ensureVisible(find.byKey(const Key('filter-color-red')));
    await tester.tap(find.byKey(const Key('filter-color-red')));
    await tester.pumpAndSettle();
    expect(
      tester
          .widget<TextField>(find.byKey(const Key('search-field')))
          .decoration!
          .hintText,
      'Search in Red notes',
    );
    expect(find.byKey(const Key('note:hardware')), findsOneWidget);
    expect(find.byKey(const Key('note:groceries')), findsNothing);
  });

  testWidgets('Trash search is separate and system Back returns Home', (
    tester,
  ) async {
    final library = twoLists();
    await library.setTrashed('groceries', true);
    await pumpList(tester, library);
    await searchFor(tester, 'milk');
    expect(find.byKey(const Key('no-matches')), findsOneWidget);
    await tester.tap(find.byKey(const Key('search-close')));
    await tester.pumpAndSettle();
    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('shelf-trash')));
    await tester.pumpAndSettle();
    await searchFor(tester, 'milk');
    expect(find.byKey(const Key('note:groceries')), findsOneWidget);
    await tester.binding.handlePopRoute();
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('search-field')), findsNothing);
    expect(find.byKey(const Key('note:hardware')), findsOneWidget);
    expect(find.byKey(const Key('note:groceries')), findsNothing);
  });

  test('a summary is a value', () {
    const a = NoteSummary(id: 'a', title: 'x', items: 1, checked: 0);
    const b = NoteSummary(id: 'a', title: 'x', items: 1, checked: 0);
    expect(a, b);
    expect(
      a ==
          const NoteSummary(
            id: 'a',
            title: 'x',
            items: 1,
            checked: 0,
            pinned: true,
          ),
      isFalse,
      reason: 'a placement is part of what a card shows',
    );
  });

  testWidgets('says so when an edit could not be kept', (tester) async {
    final library = twoLists();
    await pumpList(tester, library);

    // Nothing at all while the device is keeping what it is given, which is
    // every platform but the web and most of the web.
    expect(find.byKey(const Key('storage-error')), findsNothing);

    library.pushStorageError('QuotaExceededError');
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('storage-error')), findsOneWidget);
    // Above the notes, not instead of them: what was already stored is still
    // there and still worth reading.
    expect(find.text('Groceries'), findsOneWidget);
  });

  testWidgets('reading the detail is not the same as it being fixed', (
    tester,
  ) async {
    // There is no dismiss button, and opening the browser's own words must not
    // become one: the edit is still gone when the dialog closes. Whether the
    // library ever stops saying this is the library's decision and is held in
    // `rust_bridge_test.dart`; what this holds is that the screen does not make
    // it for it.
    final library = twoLists();
    await pumpList(tester, library);
    library.pushStorageError('QuotaExceededError');
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('storage-error')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('storage-error-detail')), findsOneWidget);

    await tester.tap(find.text('Done'));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('storage-error')), findsOneWidget);
  });
}
