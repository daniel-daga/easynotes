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
import 'package:easynotes/core/relay_profiles.dart';
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

  testWidgets('pull to refresh works for an empty relay collection', (
    tester,
  ) async {
    final library = FakeNoteLibrary();
    await tester.pumpWidget(
      MaterialApp(
        home: NoteListScreen(
          library: library,
          profile: const RelayProfile(
            id: 'relay',
            url: 'https://relay.example',
          ),
        ),
      ),
    );
    await tester.drag(find.byKey(const Key('no-notes')), const Offset(0, 320));
    await tester.pumpAndSettle();
    expect(library.calls, contains('syncNow()'));
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

  testWidgets('the create-note button reveals the note types', (tester) async {
    final library = FakeNoteLibrary();
    await pumpList(tester, library);

    // Folded away until asked for.
    expect(find.byKey(const Key('new-text-note-button')), findsNothing);
    expect(find.byKey(const Key('new-list-button')), findsNothing);
    expect(find.byKey(const Key('new-audio-note-button')), findsNothing);

    await tester.tap(find.byKey(const Key('new-note-button')));
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('new-text-note-button')), findsOneWidget);
    expect(find.byKey(const Key('new-list-button')), findsOneWidget);
    expect(find.byKey(const Key('new-audio-note-button')), findsOneWidget);
  });

  testWidgets('a text note from the revealed types is made and opened', (
    tester,
  ) async {
    final library = FakeNoteLibrary();
    await pumpList(tester, library);

    await tester.tap(find.byKey(const Key('new-note-button')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('new-text-note-button')));
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

    await tester.tap(find.byKey(const Key('new-note-button')));
    await tester.pumpAndSettle();
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
      await tester.scrollUntilVisible(
        find.byKey(const Key('join-entry')),
        200,
        scrollable: find.descendant(
          of: find.byType(Drawer),
          matching: find.byType(Scrollable),
        ),
      );
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
      await tester.scrollUntilVisible(
        find.byKey(const Key('join-entry')),
        200,
        scrollable: find.descendant(
          of: find.byType(Drawer),
          matching: find.byType(Scrollable),
        ),
      );
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
    await tester.pumpWidget(
      MaterialApp(
        home: NoteListScreen(
          library: library,
          profile: const RelayProfile(
            id: 'relay',
            url: 'https://relay.example',
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();
    library.pushStatus(const SyncStatus(state: SyncState.offline, pending: 3));
    await tester.pumpAndSettle();
    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    // The drawer's status opens the consolidated screen with the same status.
    expect(
      find.descendant(
        of: find.byKey(const Key('sync-status')),
        matching: find.text('Not syncing, 3 edits waiting'),
      ),
      findsOneWidget,
    );
    await tester.tap(find.byKey(const Key('sync-status')));
    await tester.pumpAndSettle();

    expect(find.text('Not syncing, 3 edits waiting'), findsOneWidget);
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

  /// Whether a card is drawn as selected.
  Finder selectedMark(String note) => find.descendant(
    of: find.ancestor(
      of: find.byKey(Key('note:$note')),
      matching: find.byType(Stack),
    ),
    matching: find.byKey(const Key('card-selected')),
  );

  /// Long-press a card and let go without moving it: it is selected.
  Future<void> select(WidgetTester tester, String note) async {
    await tester.longPress(find.byKey(Key('note:$note')));
    await tester.pumpAndSettle();
  }

  /// The selection bar's actions that live behind its "more" menu.
  const overflow = {'bulk-pin', 'bulk-reminders'};

  /// Take one of the selection bar's actions.
  Future<void> act(WidgetTester tester, String key) async {
    if (overflow.contains(key)) {
      await tester.tap(find.byKey(const Key('selection-more')));
      await tester.pumpAndSettle();
    }
    await tester.tap(find.byKey(Key(key)));
    await tester.pumpAndSettle();
  }

  /// Select one card and take an action on it.
  Future<void> holdAndTap(WidgetTester tester, String note, String key) async {
    await select(tester, note);
    await act(tester, key);
  }

  testWidgets('holding a card selects it and offers what can be done', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);

    await select(tester, 'groceries');

    expect(selectedMark('groceries'), findsOneWidget);
    expect(selectedMark('hardware'), findsNothing);
    expect(find.byKey(const Key('selection-count')), findsOneWidget);
    expect(find.text('1'), findsOneWidget);
    for (final action in [
      'bulk-colour',
      'bulk-labels',
      'bulk-archive',
      'bulk-trash',
    ]) {
      expect(find.byKey(Key(action)), findsOneWidget, reason: action);
    }
    // Nothing is made while choosing what to do with what is there.
    expect(find.byKey(const Key('new-note-button')), findsNothing);

    await tester.tap(find.byKey(const Key('selection-more')));
    await tester.pumpAndSettle();
    expect(find.text('Pin'), findsOneWidget);
    expect(find.text('Reminders'), findsOneWidget);
  });

  testWidgets('a tap adds to the selection and the last one out ends it', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);
    await select(tester, 'groceries');

    await tester.tap(find.byKey(const Key('note:hardware')));
    await tester.pumpAndSettle();
    expect(selectedMark('hardware'), findsOneWidget);
    expect(find.text('2'), findsOneWidget);
    // A tap while selecting chooses a card, it does not open one.
    expect(library.calls, isNot(contains(startsWith('openNote'))));
    // A reminder is for one note at a time.
    await tester.tap(find.byKey(const Key('selection-more')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('bulk-reminders')), findsNothing);
    await tester.tapAt(Offset.zero);
    await tester.pumpAndSettle();

    // A long press while selecting toggles, too.
    await select(tester, 'groceries');
    expect(selectedMark('groceries'), findsNothing);
    await tester.tap(find.byKey(const Key('note:hardware')));
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('selection-count')), findsNothing);
    expect(onShelf('EasyNotes'), findsOneWidget);
    await tester.tap(find.byKey(const Key('note:hardware')));
    await tester.pumpAndSettle();
    expect(library.calls, contains('openNote(hardware)'));
  });

  testWidgets('the close button and Back both let go of a selection', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);

    await select(tester, 'groceries');
    await tester.tap(find.byKey(const Key('selection-close')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('selection-count')), findsNothing);

    await select(tester, 'groceries');
    final popped = await tester.binding.handlePopRoute();
    await tester.pumpAndSettle();
    expect(popped, isTrue, reason: 'Back should be taken by the selection');
    expect(find.byKey(const Key('selection-count')), findsNothing);
    expect(find.byKey(const Key('note:groceries')), findsOneWidget);
  });

  testWidgets('select all takes every card on the shelf', (tester) async {
    final library = twoLists();
    await pumpList(tester, library);
    await select(tester, 'groceries');

    await act(tester, 'selection-more');
    await tester.tap(find.byKey(const Key('bulk-select-all')));
    await tester.pumpAndSettle();

    expect(find.text('2'), findsOneWidget);
    expect(selectedMark('hardware'), findsOneWidget);
  });

  testWidgets('a selected note that leaves the shelf leaves the selection', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);
    await select(tester, 'groceries');
    await tester.tap(find.byKey(const Key('note:hardware')));
    await tester.pumpAndSettle();

    // Archived on another device.
    await library.setArchived('groceries', true);
    await tester.pumpAndSettle();

    expect(find.text('1'), findsOneWidget);
    expect(selectedMark('hardware'), findsOneWidget);
  });

  testWidgets('cards are not dragged while selecting', (tester) async {
    final library = twoLists();
    await pumpList(tester, library);
    await select(tester, 'groceries');

    final gesture = await tester.startGesture(
      tester.getCenter(find.byKey(const Key('note:hardware'))),
    );
    await tester.pump(kLongPressTimeout + const Duration(milliseconds: 50));
    expect(find.byKey(const Key('drag-placeholder')), findsNothing);
    await gesture.moveTo(
      tester.getCenter(find.byKey(const Key('note:groceries'))),
    );
    await tester.pump();
    await gesture.up();
    await tester.pumpAndSettle();

    expect(library.calls, isNot(contains(startsWith('moveNoteTo'))));
    // The hold was a press, which adds the card.
    expect(find.text('2'), findsOneWidget);
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

  testWidgets('a carried card leaves a gap the size of the card', (
    tester,
  ) async {
    // The card that was picked up is what the finger is holding, drawn once in
    // the overlay. What stays in the grid is a hole, not a faded second copy
    // for the drop target to find under the pointer.
    final library = twoLists();
    await pumpList(tester, library);
    final card = tester.getSize(find.byKey(const Key('note:hardware')));

    final gesture = await tester.startGesture(
      tester.getCenter(find.byKey(const Key('note:hardware'))),
    );
    await tester.pump(kLongPressTimeout + const Duration(milliseconds: 50));

    expect(find.byKey(const Key('drag-placeholder')), findsOneWidget);
    expect(
      tester.getSize(find.byKey(const Key('drag-placeholder'))),
      card,
      reason: 'the gap should be the card, so nothing jumps when it appears',
    );

    await gesture.moveBy(const Offset(0, 60));
    await tester.pumpAndSettle();
    await gesture.up();
    await tester.pumpAndSettle();
  });

  testWidgets('the grid holds its columns still while a card is carried', (
    tester,
  ) async {
    // Re-balancing on every preview moves the cards under the finger, which
    // changes where the finger is pointing. Only the gap moves until the drop.
    final library = twoLists();
    await pumpList(tester, library);
    final before = tester.getRect(find.byKey(const Key('note:groceries')));

    final gesture = await tester.startGesture(
      tester.getCenter(find.byKey(const Key('note:hardware'))),
    );
    await tester.pump(kLongPressTimeout + const Duration(milliseconds: 50));
    await gesture.moveTo(
      tester.getCenter(find.byKey(const Key('note:groceries'))),
    );
    await tester.pumpAndSettle();

    expect(
      tester.getRect(find.byKey(const Key('note:groceries'))),
      before,
      reason: 'the card it will land after should not move before the drop',
    );

    await gesture.up();
    await tester.pumpAndSettle();
    expect(library.calls, contains('moveNoteTo(hardware, groceries)'));
  });

  testWidgets('the preview does not chase the gap it makes', (tester) async {
    // The gap sits under the pointer once the preview moves to it. If the gap
    // were also a target, answering it would put the preview back where the
    // drag started, and the next pointer event would move it again.
    final library = twoLists();
    await pumpList(tester, library);

    final gesture = await tester.startGesture(
      tester.getCenter(find.byKey(const Key('note:hardware'))),
    );
    await tester.pump(kLongPressTimeout + const Duration(milliseconds: 50));
    await gesture.moveTo(
      tester.getCenter(find.byKey(const Key('note:groceries'))),
    );
    await tester.pumpAndSettle();

    final gap = find.byKey(const Key('drag-placeholder'));
    final preview = tester.getRect(gap);
    // It landed after Groceries, in Groceries' column.
    expect(
      preview.top,
      greaterThan(tester.getRect(find.byKey(const Key('note:groceries'))).top),
    );

    await gesture.moveTo(preview.center);
    await tester.pumpAndSettle();

    expect(
      tester.getRect(gap),
      preview,
      reason: 'resting on the gap is not a new drop target',
    );

    await gesture.up();
    await tester.pumpAndSettle();
    expect(library.calls, contains('moveNoteTo(hardware, groceries)'));
  });

  testWidgets('a card released without moving is selected, not moved', (
    tester,
  ) async {
    // The gesture does two jobs: a hold that moves is a drag, and one that
    // goes nowhere picks the card.
    final library = twoLists();
    await pumpList(tester, library);

    await select(tester, 'groceries');

    expect(selectedMark('groceries'), findsOneWidget);
    expect(library.calls, isNot(contains(startsWith('moveNoteTo'))));
  });

  testWidgets('a dragged card is moved, not selected', (tester) async {
    final library = twoLists();
    await pumpList(tester, library);

    await dragCard(tester, 'hardware', 'groceries');

    expect(library.calls, contains('moveNoteTo(hardware, groceries)'));
    expect(find.byKey(const Key('selection-count')), findsNothing);
  });

  testWidgets('a card dragged across the pin line does not move', (
    tester,
  ) async {
    // A pin lifts a note out of the sequence it shares with the rest, so a move
    // between the two groups is one nothing on screen could show.
    final library = twoLists();
    await pumpList(tester, library);
    await holdAndTap(tester, 'groceries', 'bulk-pin');
    library.calls.clear();

    await dragCard(tester, 'hardware', 'groceries');

    expect(library.calls, isNot(contains(startsWith('moveNoteTo'))));
    // And it did not quietly turn into a selection either: the drag went
    // somewhere.
    expect(find.byKey(const Key('selection-count')), findsNothing);
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
    // The hold still selects, which is how a result is acted on.
    expect(selectedMark('hardware'), findsOneWidget);
  });

  testWidgets('colouring a note tells the library and paints the card', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);

    await holdAndTap(tester, 'groceries', 'bulk-colour');
    expect(find.byKey(const Key('colour-sheet')), findsOneWidget);
    await tester.tap(find.byKey(const Key('swatch-teal')));
    await tester.pumpAndSettle();

    expect(library.calls, contains('setColourMany(groceries; teal)'));
    final card = tester.widget<Card>(
      find.ancestor(
        of: find.byKey(const Key('note:groceries')),
        matching: find.byType(Card),
      ),
    );
    expect(card.color, isNotNull, reason: 'the card was not painted');
    // Still selected, so another colour is one tap away.
    expect(selectedMark('groceries'), findsOneWidget);
  });

  testWidgets('a selection is coloured in one change', (tester) async {
    final library = twoLists();
    await pumpList(tester, library);
    await library.setColour('groceries', NoteColour.red);
    await tester.pumpAndSettle();
    await select(tester, 'groceries');
    await tester.tap(find.byKey(const Key('note:hardware')));
    await tester.pumpAndSettle();

    await act(tester, 'bulk-colour');
    // The notes do not share a colour, so none is marked as theirs.
    expect(
      find.byWidgetPredicate(
        (widget) =>
            widget is Semantics &&
            widget.properties.selected == true &&
            (widget.properties.label ?? '').endsWith('olour'),
      ),
      findsNothing,
    );
    await tester.tap(find.byKey(const Key('swatch-none')));
    await tester.pumpAndSettle();

    expect(library.calls, contains('setColourMany(hardware, groceries; null)'));
    expect(
      library.calls.where((call) => call.startsWith('setColour(')),
      hasLength(1),
      reason: 'only the setup coloured a note on its own',
    );
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

    await holdAndTap(tester, 'groceries', 'bulk-pin');

    expect(library.calls, contains('setPinnedMany(groceries; true)'));
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

    await holdAndTap(tester, 'groceries', 'bulk-archive');

    expect(library.calls, contains('setArchivedMany(groceries; true)'));
    expect(find.byKey(const Key('note:groceries')), findsNothing);
    expect(find.text('Note archived'), findsOneWidget);
    expect(find.byKey(const Key('selection-count')), findsNothing);

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

    await holdAndTap(tester, 'hardware', 'bulk-trash');
    expect(find.byKey(const Key('note:hardware')), findsNothing);

    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('shelf-trash')));
    await tester.pumpAndSettle();

    await select(tester, 'hardware');
    // In the trash a note is restored, not pinned, coloured or archived.
    expect(find.byKey(const Key('selection-more')), findsNothing);
    expect(find.byKey(const Key('bulk-archive')), findsNothing);
    expect(find.byKey(const Key('bulk-colour')), findsNothing);
    await act(tester, 'bulk-restore');

    expect(library.calls, contains('setTrashedMany(hardware; false)'));
    expect(find.byKey(const Key('no-notes')), findsOneWidget);
  });

  testWidgets('archiving a selection can be undone, pins and all', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);
    await library.setPinned('groceries', true);
    await tester.pumpAndSettle();
    await select(tester, 'groceries');
    await tester.tap(find.byKey(const Key('note:hardware')));
    await tester.pumpAndSettle();
    library.calls.clear();

    await act(tester, 'bulk-archive');

    expect(library.calls, ['setArchivedMany(groceries, hardware; true)']);
    expect(find.text('2 notes archived'), findsOneWidget);
    expect(find.byKey(const Key('no-notes')), findsOneWidget);

    await tester.tap(find.text('Undo'));
    await tester.pumpAndSettle();

    expect(library.calls.skip(1), [
      'setArchivedMany(groceries, hardware; false)',
      'setPinnedMany(groceries; true)',
    ]);
    expect(find.byKey(const Key('pinned')), findsOneWidget);
    expect(find.byKey(const Key('note:hardware')), findsOneWidget);
  });

  testWidgets('trashing a selection can be undone', (tester) async {
    final library = twoLists();
    await pumpList(tester, library);
    await select(tester, 'groceries');
    await tester.tap(find.byKey(const Key('note:hardware')));
    await tester.pumpAndSettle();

    await act(tester, 'bulk-trash');

    expect(
      library.calls,
      contains('setTrashedMany(hardware, groceries; true)'),
    );
    expect(find.text('2 notes moved to trash'), findsOneWidget);
    expect(find.byKey(const Key('no-notes')), findsOneWidget);

    await tester.tap(find.text('Undo'));
    await tester.pumpAndSettle();

    expect(
      library.calls,
      contains('setTrashedMany(hardware, groceries; false)'),
    );
    expect(find.byKey(const Key('note:groceries')), findsOneWidget);
    expect(find.byKey(const Key('note:hardware')), findsOneWidget);
  });

  // -- deleting for good ----------------------------------------------------

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
    await select(tester, 'hardware');
    expect(find.byKey(const Key('bulk-delete-forever')), findsNothing);
    await act(tester, 'bulk-trash');

    // Nor on an archived note, which is out of the way rather than thrown away.
    await holdAndTap(tester, 'groceries', 'bulk-archive');
    await openShelf(tester, 'archive');
    await select(tester, 'groceries');
    expect(find.byKey(const Key('bulk-delete-forever')), findsNothing);
    expect(find.byTooltip('Unarchive'), findsOneWidget);
    await act(tester, 'bulk-archive');
    expect(library.calls, contains('setArchivedMany(groceries; false)'));

    await openShelf(tester, 'trash');
    await select(tester, 'hardware');
    expect(find.byKey(const Key('bulk-delete-forever')), findsOneWidget);
  });

  testWidgets(
    'deleting for good asks first, and keeping the note means keeping '
    'it',
    (tester) async {
      final library = twoLists();
      await pumpList(tester, library);
      await holdAndTap(tester, 'hardware', 'bulk-trash');
      await openShelf(tester, 'trash');

      await holdAndTap(tester, 'hardware', 'bulk-delete-forever');
      expect(find.byKey(const Key('confirm-delete')), findsOneWidget);
      // The one thing the app cannot do is said on the dialog that does it.
      expect(
        find.textContaining('People you shared this note with keep their'),
        findsOneWidget,
      );

      await tester.tap(find.byKey(const Key('confirm-keep')));
      await tester.pumpAndSettle();

      expect(library.calls, isNot(contains('forgetNotes(hardware)')));
      expect(find.byKey(const Key('note:hardware')), findsOneWidget);
    },
  );

  testWidgets('a note deleted for good leaves the device', (tester) async {
    final library = twoLists();
    await pumpList(tester, library);
    await holdAndTap(tester, 'hardware', 'bulk-trash');
    await openShelf(tester, 'trash');

    await holdAndTap(tester, 'hardware', 'bulk-delete-forever');
    await tester.tap(find.byKey(const Key('confirm-delete-forever')));
    await tester.pumpAndSettle();

    expect(library.calls, contains('forgetNotes(hardware)'));
    expect(find.byKey(const Key('no-notes')), findsOneWidget);
    // Gone from the whole device, not moved to another shelf: the grid it was
    // thrown away from does not have it back.
    await openShelf(tester, 'notes');
    expect(find.byKey(const Key('note:hardware')), findsNothing);
    expect(find.byKey(const Key('note:groceries')), findsOneWidget);
  });

  testWidgets('a selection in the trash is deleted for good together', (
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
    await select(tester, 'hardware');
    await tester.tap(find.byKey(const Key('note:old')));
    await tester.pumpAndSettle();
    await act(tester, 'bulk-trash');
    await openShelf(tester, 'trash');

    await select(tester, 'hardware');
    await tester.tap(find.byKey(const Key('note:old')));
    await tester.pumpAndSettle();
    await act(tester, 'bulk-delete-forever');
    expect(find.text('Delete 2 notes permanently?'), findsOneWidget);
    await tester.tap(find.byKey(const Key('confirm-delete-forever')));
    await tester.pumpAndSettle();

    expect(library.calls, contains('forgetNotes(old, hardware)'));
    expect(find.byKey(const Key('no-notes')), findsOneWidget);
    expect(find.byKey(const Key('selection-count')), findsNothing);
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
    await holdAndTap(tester, 'hardware', 'bulk-trash');
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
    await holdAndTap(tester, 'hardware', 'bulk-trash');
    await holdAndTap(tester, 'old', 'bulk-trash');
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
    await holdAndTap(tester, 'hardware', 'bulk-trash');
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
    await holdAndTap(tester, 'hardware', 'bulk-delete-forever');
    await tester.tap(find.byKey(const Key('confirm-delete-forever')));
    await tester.pumpAndSettle();
    expect(library.calls, contains('forgetNotes(hardware)'));
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

  testWidgets(
    'the drawer hints at no labels instead of offering to create one',
    (tester) async {
      final library = twoLists();
      await pumpList(tester, library);
      await tester.tap(find.byTooltip('Open navigation menu'));
      await tester.pumpAndSettle();

      expect(find.text('No labels yet'), findsOneWidget);
      expect(find.byKey(const Key('create-label')), findsNothing);

      await library.addTag('groceries', 'errands');
      await tester.pumpAndSettle();

      expect(find.text('No labels yet'), findsNothing);
      expect(find.byKey(const Key('label-shelf-errands')), findsOneWidget);
    },
  );

  /// Make a label in the manager, then choose it from the note's picker.
  Future<void> labelNote(WidgetTester tester, String id, String label) async {
    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('edit-labels')));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('label-field')), label);
    await tester.tap(find.byKey(const Key('label-add')));
    await tester.pumpAndSettle();
    await tester.pageBack();
    await tester.pumpAndSettle();
    await holdAndTap(tester, id, 'bulk-labels');
    expect(find.byKey(const Key('label-field')), findsNothing);
    expect(find.byIcon(Icons.edit_outlined), findsNothing);
    await tester.tap(find.byKey(Key('label-check-$label')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('labels-done')));
    await tester.pumpAndSettle();
    // The selection outlives the labels screen, so more can be done to it.
    expect(selectedMark(id), findsOneWidget);
    await tester.tap(find.byKey(const Key('selection-close')));
    await tester.pumpAndSettle();
  }

  testWidgets('a selection is labelled together, and a label on some of it '
      'is shown as half-ticked', (tester) async {
    final library = twoLists();
    await pumpList(tester, library);
    await labelNote(tester, 'groceries', 'errands');
    final errands = library.tags.single;
    library.calls.clear();

    await select(tester, 'groceries');
    await tester.tap(find.byKey(const Key('note:hardware')));
    await tester.pumpAndSettle();
    await act(tester, 'bulk-labels');

    final check = find.byKey(const Key('label-check-errands'));
    expect(tester.widget<CheckboxListTile>(check).value, isNull);

    await tester.tap(check);
    await tester.pumpAndSettle();
    expect(library.calls, ['addTagMany(hardware, groceries; errands)']);
    expect(tester.widget<CheckboxListTile>(check).value, isTrue);

    await tester.tap(check);
    await tester.pumpAndSettle();
    expect(
      library.calls,
      contains('removeTagMany(hardware, groceries; ${errands.id})'),
    );
    expect(tester.widget<CheckboxListTile>(check).value, isFalse);

    // A new name goes on every note at once.
    await tester.enterText(find.byKey(const Key('label-search-field')), 'home');
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('label-add-to-note')));
    await tester.pumpAndSettle();
    expect(library.calls, contains('addTagMany(hardware, groceries; home)'));

    await tester.tap(find.byKey(const Key('labels-done')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('chip:groceries:home')), findsOneWidget);
    expect(find.byKey(const Key('chip:hardware:home')), findsOneWidget);
  });

  testWidgets('a selection is let go of when its shelf goes away', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);
    await labelNote(tester, 'groceries', 'errands');
    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('label-shelf-errands')));
    await tester.pumpAndSettle();
    await select(tester, 'groceries');

    // Renamed elsewhere: the same shelf, so the same selection.
    await library.renameTag(library.tags.single.id, 'chores');
    await tester.pumpAndSettle();
    expect(selectedMark('groceries'), findsOneWidget);

    // Thrown away: back to the grid, with nothing selected.
    await library.forgetTag(library.tags.single.id);
    await tester.pumpAndSettle();
    expect(onShelf('EasyNotes'), findsOneWidget);
    expect(find.byKey(const Key('selection-count')), findsNothing);
  });

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
    await holdAndTap(tester, 'hardware', 'bulk-labels');
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

  testWidgets('the drawer can create the first label and rejects whitespace', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);
    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('edit-labels')), findsOneWidget);
    await tester.tap(find.byKey(const Key('edit-labels')));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('label-field')), '   ');
    await tester.tap(find.byKey(const Key('label-add')));
    await tester.pumpAndSettle();

    // The field never reaches the library, so there is nothing to undo.
    expect(library.calls.where((call) => call.startsWith('makeTag')), isEmpty);
    expect(library.tags, isEmpty);
  });

  testWidgets('reminder bells follow changes in grid, list and search', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);
    final bell = find.byKey(const Key('reminder-indicator'));
    expect(bell, findsNothing);

    Future<String> remind() => library.createReminder(
      noteId: 'groceries',
      fireAt: DateTime.utc(2030, 1, 1),
      repeat: ReminderRepeat.never,
      timezone: 'Etc/UTC',
    );

    final first = await remind();
    final second = await remind();
    await tester.pumpAndSettle();
    expect(bell, findsOneWidget);
    expect(
      find.descendant(
        of: find.byKey(const Key('note:groceries')),
        matching: bell,
      ),
      findsOneWidget,
    );
    // The card may merge the indicator into its own accessibility label.
    final semantics = tester.ensureSemantics();
    await tester.pump();
    try {
      expect(find.bySemanticsLabel(RegExp('Has reminder')), findsWidgets);
    } finally {
      semantics.dispose();
    }

    await tester.tap(find.byKey(const Key('layout-toggle')));
    await tester.pumpAndSettle();
    expect(bell, findsOneWidget);

    await tester.tap(find.byKey(const Key('search-button')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('filter-reminders-has')));
    await tester.pumpAndSettle();
    expect(bell, findsOneWidget);
    expect(find.text('Hardware'), findsNothing);

    await tester.enterText(find.byKey(const Key('search-field')), 'milk');
    await tester.pumpAndSettle();
    expect(bell, findsOneWidget);
    await library.forgetReminder(first);
    await tester.pumpAndSettle();
    expect(bell, findsOneWidget);
    await library.forgetReminder(second);
    await tester.pumpAndSettle();
    expect(bell, findsNothing);
    expect(find.byKey(const Key('no-matches')), findsOneWidget);
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

    await holdAndTap(tester, 'groceries', 'bulk-reminders');
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
    await tester.tap(find.byKey(const Key('new-text-note-button')));
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

  testWidgets('a label can be renamed in the manager, and its notes keep it', (
    tester,
  ) async {
    final library = twoLists();
    await pumpList(tester, library);
    await labelNote(tester, 'groceries', 'errands');

    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('label-menu-errands')), findsNothing);
    await tester.tap(find.byKey(const Key('edit-labels')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('label-edit-errands')));
    await tester.pumpAndSettle();

    await tester.enterText(
      find.byKey(const Key('rename-label-field')),
      'Chores',
    );
    await tester.tap(find.byKey(const Key('rename-label-confirm')));
    await tester.pumpAndSettle();

    // One label, under its new name, still on the note that had it.
    expect(library.tags.single.name, 'Chores');
    await tester.pageBack();
    await tester.pumpAndSettle();
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

  testWidgets('a label can be thrown away in the manager', (tester) async {
    final library = twoLists();
    await pumpList(tester, library);
    await labelNote(tester, 'groceries', 'errands');

    await tester.tap(find.byTooltip('Open navigation menu'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('edit-labels')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('label-edit-errands')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('label-forget')));
    await tester.pumpAndSettle();

    // It says what it is about to do to notes that are not on screen.
    expect(find.textContaining('comes off 1 note'), findsOneWidget);
    await tester.tap(find.byKey(const Key('confirm-forget-label')));
    await tester.pumpAndSettle();

    expect(library.tags, isEmpty);
    await tester.pageBack();
    await tester.pumpAndSettle();
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
