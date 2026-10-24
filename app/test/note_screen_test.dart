/// Widget tests for the note screen, against a faked core.
///
/// These check one thing above all: that the screen is a function of the view it
/// is given. A gesture must produce an *edit* and nothing else, and the rendered
/// list must follow the stream even when the change did not originate here.
library;

import 'dart:async';

import 'package:easynotes/core/fake_note_core.dart';
import 'package:easynotes/core/fake_note_library.dart';
import 'package:easynotes/core/invites.dart';
import 'package:easynotes/core/note_view.dart';
import 'package:easynotes/ui/invites.dart';
import 'package:easynotes/ui/note_screen.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

Future<void> pumpScreen(
  WidgetTester tester,
  FakeNoteCore core, {
  FakeStorageHealth? storage,
  FakeNoteLibrary? invites,
}) async {
  await tester.pumpWidget(
    MaterialApp(
      home: NoteScreen(
        core: core,
        storage: storage ?? FakeStorageHealth(),
        noteId: 'this-note',
        invites: invites ?? (FakeNoteLibrary()..setUpSharing()),
      ),
    ),
  );
  await tester.pumpAndSettle();
}

/// Open the collaborators screen and make a link, returning the invite it
/// made.
Future<String> makeLink(WidgetTester tester, FakeNoteLibrary invites) async {
  await tester.tap(find.byKey(const Key('share-button')));
  await tester.pumpAndSettle();
  await tester.tap(find.byKey(const Key('new-invite')));
  await tester.pumpAndSettle();
  return invites.invitesFor('this-note').single.id;
}

List<String> visibleItems(WidgetTester tester) => tester
    .widgetList<TextField>(find.byType(TextField))
    .where(
      (f) => (f.key as ValueKey<String>?)?.value.startsWith('text:') ?? false,
    )
    .map((f) => f.controller!.text)
    .toList();

/// Unfold the crossed-off items, which start collapsed below the rest.
Future<void> showCrossedOff(WidgetTester tester) async {
  await tester.tap(find.byKey(const ValueKey('crossed-off-items')));
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('crossed-off items stay as this person last left them', (
    tester,
  ) async {
    final library = FakeNoteLibrary();
    Future<void> open() async {
      // A note the library has: the screen closes itself on one it does not,
      // since that is what a note deleted on another device looks like.
      final id = await library.createNote();
      final core = library.core(id);
      await core.addItem('milk');
      await core.setChecked(await core.addItem('bread'), true);
      await tester.pumpWidget(
        MaterialApp(
          home: NoteScreen(
            key: UniqueKey(),
            core: core,
            storage: FakeStorageHealth(),
            noteId: id,
            invites: library,
            library: library,
          ),
        ),
      );
      await tester.pumpAndSettle();
    }

    await open();
    expect(visibleItems(tester), ['milk']);
    await showCrossedOff(tester);
    expect(library.calls, contains('setTickedShown(true)'));

    // The next note opens unfolded, because the setting is the person's.
    await open();
    expect(visibleItems(tester), ['milk', 'bread']);
  });

  testWidgets('the list is people, not phones, and a guest cannot manage it', (
    tester,
  ) async {
    final invites = FakeNoteLibrary()..setUpSharing('Anna');
    invites.setPeople(
      'this-note',
      const People(
        shared: true,
        canManage: false,
        people: [
          Person(member: 'olga', invite: '', name: 'Olga', owner: true),
          Person(member: 'anna', invite: 'i1', name: 'Anna', me: true),
          Person(member: 'other-anna', invite: 'i2', name: 'Anna'),
        ],
      ),
    );
    await pumpScreen(tester, FakeNoteCore(), invites: invites);
    await tester.tap(find.byKey(const Key('share-button')));
    await tester.pumpAndSettle();
    expect(find.text('Olga'), findsOneWidget);
    expect(find.text('Owner'), findsOneWidget);
    expect(find.text('Anna (you)'), findsOneWidget);
    // Two accounts that both chose "Anna" are two entries.
    expect(find.text('Anna'), findsOneWidget);
    expect(find.byKey(const Key('people-footer')), findsOneWidget);
    expect(find.text('Names are chosen by each person.'), findsOneWidget);
    expect(find.textContaining('Device'), findsNothing);
    expect(find.byKey(const Key('new-invite')), findsNothing);
    expect(find.byKey(const Key('invites-heading')), findsNothing);
    expect(find.byKey(const Key('stop-sharing')), findsNothing);
    expect(find.byKey(const Key('remove-other-anna')), findsNothing);
    await tester.tap(find.byKey(const Key('share-done')));
    await tester.pumpAndSettle();
  });

  testWidgets('send a copy moves below Reminders in the overflow menu', (
    tester,
  ) async {
    final core = FakeNoteCore(title: 'Groceries', items: const ['milk']);
    final library = FakeNoteLibrary(notes: {'note': core});
    await tester.pumpWidget(
      MaterialApp(
        home: NoteScreen(
          core: core,
          storage: library,
          noteId: 'note',
          invites: library,
          library: library,
          onReminders: () async {},
        ),
      ),
    );
    await tester.pumpAndSettle();
    expect(find.byIcon(Icons.person_add_outlined), findsOneWidget);
    expect(find.byKey(const Key('share-content-button')), findsNothing);
    await tester.tap(find.byKey(const Key('note-more-menu')));
    await tester.pumpAndSettle();
    expect(
      tester.getTopLeft(find.text('Send a copy')).dy,
      greaterThan(tester.getTopLeft(find.text('Reminders')).dy),
    );
    await tester.tap(find.text('Send a copy'));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('share-content-preview')), findsOneWidget);
    expect(find.text('Share as checklist'), findsOneWidget);
  });
  testWidgets('renders the title and items it is given', (tester) async {
    final core = FakeNoteCore(
      title: 'Groceries',
      items: const ['milk', 'bread', 'eggs'],
    );
    await pumpScreen(tester, core);

    expect(find.text('Groceries'), findsOneWidget);
    expect(visibleItems(tester), ['milk', 'bread', 'eggs']);
    expect(core.edits, isEmpty);
    expect(focusedFieldText(tester), isNull);
  });

  testWidgets('an empty checklist starts with one focused blank item', (
    tester,
  ) async {
    final core = FakeNoteCore();
    await pumpScreen(tester, core);

    expect(core.edits, ['addItem()']);
    expect(visibleItems(tester), ['']);
    expect(focusedFieldText(tester), '');
    expect(find.byKey(const Key('empty-state')), findsNothing);

    await core.setTitle('Groceries');
    await tester.pumpAndSettle();
    expect(core.current.items, hasLength(1));
    expect(core.edits.where((edit) => edit == 'addItem()'), hasLength(1));
  });

  testWidgets(
    'deleting the last item leaves a blank list until Add is tapped',
    (tester) async {
      final core = FakeNoteCore();
      await pumpScreen(tester, core);
      final id = core.current.items.single.id;

      await tester.tap(find.byKey(Key('delete:$id')));
      await tester.pumpAndSettle();
      await core.setTitle('Still empty');
      await tester.pumpAndSettle();

      expect(core.current.items, isEmpty);
      expect(visibleItems(tester), isEmpty);
      expect(find.byType(Checkbox), findsNothing);
      expect(find.byKey(const Key('empty-state')), findsNothing);
      expect(find.text('Nothing on this checklist yet.'), findsNothing);
      expect(core.edits.where((edit) => edit == 'addItem()'), hasLength(1));

      await tester.tap(find.byKey(const Key('add-list-entry')));
      await tester.pumpAndSettle();
      expect(visibleItems(tester), ['']);
      expect(focusedFieldText(tester), '');
    },
  );

  testWidgets('the add icon lines up with checkboxes and its label with text', (
    tester,
  ) async {
    for (final direction in TextDirection.values) {
      final core = FakeNoteCore(items: const ['milk']);
      await tester.pumpWidget(
        MaterialApp(
          home: Directionality(
            textDirection: direction,
            child: NoteScreen(
              key: UniqueKey(),
              core: core,
              storage: FakeStorageHealth(),
              noteId: 'this-note',
              invites: FakeNoteLibrary()..setUpSharing(),
            ),
          ),
        ),
      );
      await tester.pumpAndSettle();

      final add = find.descendant(
        of: find.byKey(const Key('add-list-entry')),
        matching: find.byIcon(Icons.add),
      );
      expect(
        tester.getCenter(add).dx,
        tester.getCenter(find.byType(Checkbox)).dx,
      );
      final text = find.byKey(Key('text:${core.current.items.single.id}'));
      expect(
        tester.getRect(find.text('Checklist item')).left,
        tester.getRect(text).left,
      );
    }
  });

  testWidgets('adding an entry makes a blank item, focused, saved as typed', (
    tester,
  ) async {
    final core = FakeNoteCore(items: const ['milk']);
    await pumpScreen(tester, core);

    await tester.tap(find.byKey(const Key('add-list-entry')));
    await tester.pumpAndSettle();

    expect(core.edits, ['addItem()']);
    final id = core.current.items.last.id;
    expect(
      tester.widget<TextField>(find.byKey(Key('text:$id'))).focusNode!.hasFocus,
      isTrue,
      reason: 'the new row should be ready to type into',
    );

    await tester.enterText(find.byKey(Key('text:$id')), 'asparagus');
    await tester.pumpAndSettle();
    expect(core.edits, contains('setItemText($id, asparagus)'));
    expect(visibleItems(tester), ['milk', 'asparagus']);
  });

  testWidgets('an entry left blank is gone when the note is closed', (
    tester,
  ) async {
    final core = FakeNoteCore();
    await tester.pumpWidget(
      MaterialApp(
        home: Builder(
          builder: (context) => TextButton(
            key: const Key('open'),
            onPressed: () => Navigator.of(context).push(
              MaterialPageRoute<void>(
                builder: (_) => NoteScreen(
                  core: core,
                  storage: FakeStorageHealth(),
                  noteId: 'this-note',
                  invites: FakeNoteLibrary()..setUpSharing(),
                ),
              ),
            ),
            child: const Text('open'),
          ),
        ),
      ),
    );
    await tester.tap(find.byKey(const Key('open')));
    await tester.pumpAndSettle();

    final id = core.current.items.single.id;
    await tester.enterText(find.byKey(Key('text:$id')), '   ');
    await tester.pumpAndSettle();

    await tester.binding.handlePopRoute();
    await tester.pumpAndSettle();

    expect(find.byType(NoteScreen), findsNothing);
    expect(core.edits, contains('removeItem($id)'));
    expect(core.current.items, isEmpty);
  });

  testWidgets('checking an item sends an edit and strikes the text through', (
    tester,
  ) async {
    final core = FakeNoteCore(items: const ['milk', 'bread']);
    await pumpScreen(tester, core);
    final id = core.current.items[1].id;

    await tester.tap(find.byKey(Key('check:$id')));
    await tester.pumpAndSettle();

    expect(core.edits, contains('setChecked($id, true)'));
    // Crossed off, it moves below the rest, folded away until asked for.
    expect(visibleItems(tester), ['milk']);
    expect(find.text('Crossed off items (1)'), findsOneWidget);
    await showCrossedOff(tester);
    final text = tester.widget<TextField>(find.byKey(Key('text:$id')));
    expect(text.style?.decoration, TextDecoration.lineThrough);
  });

  testWidgets('editing an item saves it as it is typed', (tester) async {
    // The core folds a burst of typing into one envelope (ADR 0016), so the
    // screen does not wait for the field to lose focus.
    final core = FakeNoteCore(items: const ['eggs']);
    await pumpScreen(tester, core);
    final id = core.current.items[0].id;

    await tester.enterText(find.byKey(Key('text:$id')), 'eggs (12)');
    await tester.pumpAndSettle();

    expect(core.edits, ['setItemText($id, eggs (12))']);
  });

  testWidgets('enter in an item starts a new one after it', (tester) async {
    final core = FakeNoteCore(items: const ['milk', 'bread']);
    await pumpScreen(tester, core);
    final milk = core.current.items[0].id;

    await tester.showKeyboard(find.byKey(Key('text:$milk')));
    await tester.testTextInput.receiveAction(TextInputAction.next);
    await tester.pumpAndSettle();

    expect(visibleItems(tester), ['milk', '', 'bread']);
    expect(core.current.items.map((item) => item.text), ['milk', '', 'bread']);
    expect(
      focusedFieldText(tester),
      '',
      reason: 'the new row has the keyboard',
    );
  });

  testWidgets('typing on after enter, before the new item exists, keeps every '
      'letter', (tester) async {
    // Making an item is a round trip to the core, and a person types straight
    // through it. The keyboard has to be in the new row from the moment Enter
    // is pressed: on Android a letter sent while it moves is dropped, so it
    // cannot move a round trip later, which is when the next letter comes.
    final core = _SlowAddCore(items: const ['milk', 'bread']);
    await pumpScreen(tester, core);
    final milk = core.current.items[0].id;

    await tester.showKeyboard(find.byKey(Key('text:$milk')));
    core.gate = Completer<void>();
    await tester.testTextInput.receiveAction(TextInputAction.next);
    await tester.pump();
    // The core has not answered, and the new row already has the keyboard.
    expect(visibleItems(tester), ['milk', '', 'bread']);
    tester.testTextInput.updateEditingValue(
      const TextEditingValue(
        text: 'eggs',
        selection: TextSelection.collapsed(offset: 4),
      ),
    );
    await tester.pump();
    // And Enter again, still before it has.
    await tester.testTextInput.receiveAction(TextInputAction.next);
    await tester.pump();
    tester.testTextInput.updateEditingValue(
      const TextEditingValue(
        text: 'jam',
        selection: TextSelection.collapsed(offset: 3),
      ),
    );
    await tester.pump();
    expect(visibleItems(tester), ['milk', 'eggs', 'jam', 'bread']);

    core.gate!.complete();
    for (var i = 0; i < 10; i++) {
      await tester.pump(const Duration(milliseconds: 20));
    }
    tester.testTextInput.updateEditingValue(
      const TextEditingValue(
        text: 'jam!',
        selection: TextSelection.collapsed(offset: 4),
      ),
    );
    await tester.pump(const Duration(milliseconds: 100));
    await tester.pumpAndSettle();

    expect(visibleItems(tester), ['milk', 'eggs', 'jam!', 'bread']);
    expect(core.current.items.map((item) => item.text), [
      'milk',
      'eggs',
      'jam!',
      'bread',
    ], reason: 'and that is what the core holds');
    expect(
      focusedFieldText(tester),
      'jam!',
      reason: 'the keyboard never left the row it was typing into',
    );
  });

  testWidgets('deleting an item sends an edit', (tester) async {
    final core = FakeNoteCore(items: const ['milk', 'bread']);
    await pumpScreen(tester, core);
    final id = core.current.items[0].id;

    // The X is only on the row being edited.
    expect(find.byKey(Key('delete:$id')), findsNothing);
    await tester.showKeyboard(find.byKey(Key('text:$id')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(Key('delete:$id')));
    await tester.pumpAndSettle();

    expect(core.edits, contains('removeItem($id)'));
    expect(visibleItems(tester), ['bread']);
  });

  group('sharing', () {
    setUp(() => invitePollInterval = const Duration(milliseconds: 100));
    tearDown(() => joinAlerts = null);

    testWidgets('sending a link asks once to notify when someone joins', (
      tester,
    ) async {
      final alerts = joinAlerts = RecordingJoinAlerts();
      final invites = FakeNoteLibrary()..setUpSharing();
      await pumpScreen(tester, FakeNoteCore(), invites: invites);
      await makeLink(tester, invites);
      // A second link is not a new question.
      await tester.tap(find.byKey(const Key('new-invite')));
      await tester.pumpAndSettle();
      expect(invites.invitesFor('this-note'), hasLength(2));
      expect(alerts.calls, ['requestPermission()']);
      await tester.tap(find.byKey(const Key('share-done')));
      await tester.pumpAndSettle();
    });

    testWidgets('share makes a link that carries no key', (tester) async {
      final core = FakeNoteCore(items: const ['milk']);
      final invites = FakeNoteLibrary()..setUpSharing();
      await pumpScreen(tester, core, invites: invites);

      final id = await makeLink(tester, invites);

      expect(invites.calls, contains('createInvite(this-note)'));
      expect(find.byKey(Key('invite-link-$id')), findsOneWidget);
      expect(find.byKey(Key('invite-status-$id')), findsOneWidget);
      expect(find.text('Not opened yet'), findsOneWidget);
    });

    testWidgets('Send link hands the link to the share sheet', (tester) async {
      const channel = MethodChannel('eu.fraisl.easynotes/content_sharing');
      final sent = <String>[];
      final messenger =
          TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
      messenger.setMockMethodCallHandler(channel, (call) async {
        sent.add((call.arguments as Map)['text'] as String);
        return true;
      });
      final invites = FakeNoteLibrary()..setUpSharing();
      await pumpScreen(tester, FakeNoteCore(), invites: invites);

      await makeLink(tester, invites);

      final link = invites.invitesFor('this-note').single.link;
      // The link last, so a pasted message still reads as one.
      expect(sent, ['Join my shared note in EasyNotes: $link']);
      expect(find.text('Link copied'), findsNothing);
    });

    testWidgets('with no share sheet, Send link copies instead', (
      tester,
    ) async {
      String? copied;
      final messenger =
          TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
      messenger.setMockMethodCallHandler(SystemChannels.platform, (call) async {
        if (call.method == 'Clipboard.setData') {
          copied = (call.arguments as Map)['text'] as String;
        }
        return null;
      });
      addTearDown(
        () => messenger.setMockMethodCallHandler(SystemChannels.platform, null),
      );
      final invites = FakeNoteLibrary()..setUpSharing();
      await pumpScreen(tester, FakeNoteCore(), invites: invites);

      final id = await makeLink(tester, invites);

      expect(copied, invites.invitesFor('this-note').single.link);
      expect(find.text('Link copied'), findsOneWidget);
      expect(find.byKey(Key('copy-invite-$id')), findsOneWidget);
    });

    testWidgets('whoever opens the link is let in without anyone asking', (
      tester,
    ) async {
      final invites = FakeNoteLibrary()..setUpSharing();
      await pumpScreen(tester, FakeNoteCore(), invites: invites);
      final id = await makeLink(tester, invites);

      final alerts = joinAlerts = RecordingJoinAlerts();
      invites.fakeOpened(id, name: 'Anna');
      await tester.pump(invitePollInterval);
      await tester.pumpAndSettle();

      // Said in the dialog the owner is looking at, so not notified too.
      expect(alerts.calls.where((c) => c.startsWith('joined')), isEmpty);
      // No request to answer, no code to compare: she is in.
      expect(find.textContaining('wants to join'), findsNothing);
      expect(invites.calls, contains('checkInvite($id)'));
      expect(
        find.text('Anna joined. They can open this note now.'),
        findsOneWidget,
      );
      expect(find.byKey(Key('invite-link-$id')), findsNothing);
      expect(find.text('Anna'), findsOneWidget);
      await tester.tap(find.byKey(const Key('share-done')));
      await tester.pumpAndSettle();
      invites.relayError = 'Offline';
      await tester.tap(find.byKey(const Key('share-button')));
      await tester.pumpAndSettle();
      expect(find.text('Anna'), findsOneWidget);
    });

    testWidgets(
      'somebody on their way in is shown until this device is online',
      (tester) async {
        final invites = FakeNoteLibrary()..setUpSharing();
        await pumpScreen(tester, FakeNoteCore(), invites: invites);
        final id = await makeLink(tester, invites);

        invites.online = false;
        invites.fakeOpened(id, name: 'Anna');
        await tester.pump(invitePollInterval);
        await tester.pumpAndSettle();
        expect(find.byKey(const Key('adding-Anna')), findsOneWidget);

        invites.online = true;
        await tester.pump(invitePollInterval);
        await tester.pumpAndSettle();
        expect(find.byKey(const Key('adding-Anna')), findsNothing);
        expect(
          find.text('Anna joined. They can open this note now.'),
          findsOneWidget,
        );
        await tester.tap(find.byKey(const Key('share-done')));
        await tester.pumpAndSettle();
      },
    );

    testWidgets('a link can be cancelled', (tester) async {
      final invites = FakeNoteLibrary()..setUpSharing();
      await pumpScreen(tester, FakeNoteCore(), invites: invites);
      final id = await makeLink(tester, invites);

      await tester.tap(find.byKey(Key('withdraw-invite-$id')));
      await tester.pumpAndSettle();

      expect(invites.calls, contains('withdrawInvite($id)'));
      expect(invites.invitesFor('this-note'), isEmpty);
      expect(find.byKey(Key('invite-link-$id')), findsNothing);
      expect(find.byKey(const Key('no-invites')), findsOneWidget);
    });

    testWidgets('coming back finds the link that was sent', (tester) async {
      final invites = FakeNoteLibrary()..setUpSharing();
      await pumpScreen(tester, FakeNoteCore(), invites: invites);
      final id = await makeLink(tester, invites);
      await tester.tap(find.byKey(const Key('share-done')));
      await tester.pumpAndSettle();

      await tester.tap(find.byKey(const Key('share-button')));
      await tester.pumpAndSettle();

      expect(find.byKey(Key('invite-link-$id')), findsOneWidget);
      expect(
        invites.calls.where((c) => c.startsWith('createInvite')),
        hasLength(1),
      );
    });

    testWidgets('with no relay it says so rather than showing a link', (
      tester,
    ) async {
      final invites = FakeNoteLibrary()
        ..setUpSharing()
        ..relayError =
            'Sharing needs a relay, and this device is not '
            'connected to one.';
      await pumpScreen(tester, FakeNoteCore(), invites: invites);

      await tester.tap(find.byKey(const Key('share-button')));
      await tester.pumpAndSettle();
      await tester.tap(find.byKey(const Key('new-invite')));
      await tester.pumpAndSettle();

      expect(find.textContaining('needs a relay'), findsOneWidget);
      expect(find.byKey(const Key('invites-table')), findsNothing);
    });

    testWidgets('several people are invited at once, one link each', (
      tester,
    ) async {
      final invites = FakeNoteLibrary()..setUpSharing();
      await pumpScreen(tester, FakeNoteCore(), invites: invites);
      final first = await makeLink(tester, invites);
      await tester.tap(find.byKey(const Key('new-invite')));
      await tester.pumpAndSettle();
      await tester.tap(find.byKey(const Key('new-invite')));
      await tester.pumpAndSettle();

      final ids = [for (final i in invites.invitesFor('this-note')) i.id];
      expect(ids, hasLength(3));
      expect(ids.first, first);
      for (final id in ids) {
        expect(find.byKey(Key('invite-link-$id')), findsOneWidget);
      }
      expect(find.text('Invite links (3)'), findsOneWidget);
      expect(find.text('Not opened yet'), findsNWidgets(3));

      // Anna opens the second; the others stay open, waiting for theirs.
      invites.fakeOpened(ids[1], name: 'Anna');
      await tester.pump(invitePollInterval);
      await tester.pumpAndSettle();
      expect(find.text('Anna'), findsOneWidget);
      expect(find.byKey(Key('invite-link-${ids[1]}')), findsNothing);
      expect(find.byKey(Key('invite-link-${ids[0]}')), findsOneWidget);
      expect(find.byKey(Key('invite-link-${ids[2]}')), findsOneWidget);
      expect(find.text('Invite links (2)'), findsOneWidget);
      expect(find.text('People (2)'), findsOneWidget);

      // Cancelling one leaves the other.
      await tester.ensureVisible(find.byKey(Key('withdraw-invite-${ids[0]}')));
      await tester.pumpAndSettle();
      await tester.tap(find.byKey(Key('withdraw-invite-${ids[0]}')));
      await tester.pumpAndSettle();
      expect(invites.invitesFor('this-note').map((i) => i.id), [ids[2]]);

      // Ben opens the last.
      invites.fakeOpened(ids[2], name: 'Ben');
      await tester.pump(invitePollInterval);
      await tester.pumpAndSettle();
      expect(find.text('Ben'), findsOneWidget);
      expect(find.text('Anna'), findsOneWidget);
      expect(find.byKey(const Key('no-invites')), findsOneWidget);
      await tester.tap(find.byKey(const Key('share-done')));
      await tester.pumpAndSettle();
    });
  });

  testWidgets('the screen follows the stream, not its own state', (
    tester,
  ) async {
    // This is the one that matters. Nothing was tapped: the change arrived the
    // way a collaborator's edit arrives from the relay, and the screen must
    // render it anyway.
    final core = FakeNoteCore(title: 'Groceries', items: const ['milk']);
    await pumpScreen(tester, core);
    expect(visibleItems(tester), ['milk']);

    core.pushRemote(
      const NoteView(
        title: 'Shopping',
        items: [
          ItemView(
            id: 'remote-1',
            text: 'coffee',
            checked: false,
            order: '0001',
          ),
          ItemView(
            id: 'remote-2',
            text: 'oat milk',
            checked: true,
            order: '0002',
          ),
        ],
      ),
    );
    await tester.pumpAndSettle();

    expect(visibleItems(tester), ['coffee']);
    expect(find.text('Shopping'), findsOneWidget);
    await showCrossedOff(tester);
    expect(visibleItems(tester), ['coffee', 'oat milk']);
    expect(
      core.edits,
      isEmpty,
      reason: 'rendering a remote change must not echo an edit back',
    );
  });

  testWidgets('a remote rename replaces the text in an unfocused field', (
    tester,
  ) async {
    final core = FakeNoteCore(items: const ['eggs']);
    await pumpScreen(tester, core);
    final id = core.current.items[0].id;

    core.pushRemote(
      NoteView(
        title: core.current.title,
        items: [
          ItemView(id: id, text: 'eggs (12)', checked: false, order: '0000'),
        ],
      ),
    );
    await tester.pumpAndSettle();

    expect(visibleItems(tester), ['eggs (12)']);
  });

  testWidgets('a remote edit to the item being typed in appears in place', (
    tester,
  ) async {
    final core = FakeNoteCore(items: const ['milk']);
    await pumpScreen(tester, core);
    final id = core.current.items[0].id;
    final field = find.byKey(ValueKey('text:$id'));
    await tester.tap(field);
    await tester.pumpAndSettle();
    final controller = tester.widget<TextField>(field).controller!;
    // The caret after "milk", as if this person were about to carry on.
    controller.selection = const TextSelection.collapsed(offset: 4);

    // The other device adds to the same line.
    core.pushRemote(
      NoteView(
        title: core.current.title,
        items: [
          ItemView(
            id: id,
            text: 'milk and eggs',
            checked: false,
            order: '0000',
          ),
        ],
      ),
    );
    await tester.pumpAndSettle();

    expect(controller.text, 'milk and eggs');
    // Nothing was typed here, so nothing was sent back over the other person.
    expect(core.edits, isEmpty);
    // And the caret stayed where it was, rather than jumping to the end.
    expect(controller.selection, const TextSelection.collapsed(offset: 4));
  });

  testWidgets('leaving an item after a remote edit does not put the old text '
      'back', (tester) async {
    final core = FakeNoteCore(items: const ['milk']);
    await pumpScreen(tester, core);
    final id = core.current.items[0].id;
    final field = find.byKey(ValueKey('text:$id'));
    await tester.tap(field);
    await tester.pumpAndSettle();

    core.pushRemote(
      NoteView(
        title: core.current.title,
        items: [
          ItemView(
            id: id,
            text: 'milk and eggs',
            checked: false,
            order: '0000',
          ),
        ],
      ),
    );
    await tester.pumpAndSettle();

    // Tapping away commits; there is nothing newer to commit.
    FocusManager.instance.primaryFocus?.unfocus();
    await tester.pumpAndSettle();
    expect(core.edits, isEmpty);
    expect(visibleItems(tester), ['milk and eggs']);
  });

  testWidgets('typing on after a remote edit sends the merged line', (
    tester,
  ) async {
    final core = FakeNoteCore(items: const ['milk']);
    await pumpScreen(tester, core);
    final id = core.current.items[0].id;
    final field = find.byKey(ValueKey('text:$id'));
    await tester.tap(field);
    await tester.pumpAndSettle();

    core.pushRemote(
      NoteView(
        title: core.current.title,
        items: [
          ItemView(
            id: id,
            text: 'milk and eggs',
            checked: false,
            order: '0000',
          ),
        ],
      ),
    );
    await tester.pumpAndSettle();

    await tester.enterText(field, 'milk and eggs, please');
    await tester.pumpAndSettle();
    expect(core.edits, contains('setItemText($id, milk and eggs, please)'));
  });

  testWidgets(
    'a remote delete of the row being typed in leaves nothing behind',
    (tester) async {
      final core = FakeNoteCore(items: const ['milk', 'bread']);
      await pumpScreen(tester, core);
      final milk = core.current.items[0].id;
      final bread = core.current.items[1].id;
      await tester.tap(find.byKey(ValueKey('text:$milk')));
      await tester.pumpAndSettle();

      core.pushRemote(
        NoteView(
          title: core.current.title,
          items: [
            ItemView(id: bread, text: 'bread', checked: false, order: '0000'),
          ],
        ),
      );
      await tester.pumpAndSettle();

      expect(visibleItems(tester), ['bread']);
      expect(find.byKey(ValueKey('text:$milk')), findsNothing);
      // Nothing was written back for the row that is no longer there.
      expect(core.edits, isEmpty);
    },
  );

  testWidgets('leaving an item sends what was held for a pause', (
    tester,
  ) async {
    final library = FakeNoteLibrary();
    final id = await library.createNote();
    final core = library.core(id);
    await core.addItem('milk');
    await tester.pumpWidget(
      MaterialApp(
        home: NoteScreen(
          core: core,
          storage: FakeStorageHealth(),
          noteId: id,
          invites: library,
          library: library,
        ),
      ),
    );
    await tester.pumpAndSettle();
    final field = find.byKey(ValueKey('text:${core.current.items[0].id}'));
    await tester.tap(field);
    await tester.pumpAndSettle();
    FocusManager.instance.primaryFocus?.unfocus();
    await tester.pumpAndSettle();
    expect(library.calls, contains('sendHeldEdits()'));
  });

  testWidgets('dragging an item reports the index the core expects', (
    tester,
  ) async {
    // ReorderableListView counts the target index with the dragged item still in
    // place; the core wants it without. Getting this wrong moves items one slot
    // off, which is the kind of bug that only shows up in a demo.
    final core = FakeNoteCore(items: const ['one', 'two', 'three']);
    await pumpScreen(tester, core);
    final id = core.current.items[0].id;

    final state = tester.state<State<NoteScreen>>(find.byType(NoteScreen));
    final list = tester.widget<ReorderableListView>(
      find.byType(ReorderableListView),
    );
    expect(state, isNotNull);

    // Dragging item 0 down past item 2 arrives as (oldIndex: 0, newIndex: 2):
    // `onReorderItem` reports the target slot in the list without the dragged
    // item, which is the index the core wants.
    list.onReorderItem!(0, 2);
    await tester.pumpAndSettle();

    expect(core.edits, contains('moveItem($id, 2)'));
    expect(visibleItems(tester), ['two', 'three', 'one']);
  });

  testWidgets('dragging an item upwards passes the index through unchanged', (
    tester,
  ) async {
    final core = FakeNoteCore(items: const ['one', 'two', 'three']);
    await pumpScreen(tester, core);
    final id = core.current.items[2].id;

    final list = tester.widget<ReorderableListView>(
      find.byType(ReorderableListView),
    );
    list.onReorderItem!(2, 0);
    await tester.pumpAndSettle();

    expect(core.edits, contains('moveItem($id, 0)'));
    expect(visibleItems(tester), ['three', 'one', 'two']);
  });

  testWidgets('says so when an edit could not be kept', (tester) async {
    // The screen a person is typing into is the one that has to say it: a
    // warning only the note list draws is a warning they see after the edit
    // they have lost.
    final storage = FakeStorageHealth();
    addTearDown(storage.dispose);
    await pumpScreen(
      tester,
      FakeNoteCore(items: const ['milk']),
      storage: storage,
    );

    expect(find.byKey(const Key('storage-error')), findsNothing);

    storage.push('QuotaExceededError');
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('storage-error')), findsOneWidget);

    // And the browser's own words are a tap away, for whoever can act on them.
    await tester.tap(find.byKey(const Key('storage-error')));
    await tester.pumpAndSettle();
    expect(find.text('QuotaExceededError'), findsOneWidget);
  });

  group('text notes', () {
    TextField bodyField(WidgetTester tester) =>
        tester.widget<TextField>(find.byKey(const Key('body-text')));

    testWidgets('switching an empty text note to checkboxes focuses one item', (
      tester,
    ) async {
      final core = FakeNoteCore(kind: NoteKind.text);
      await pumpScreen(tester, core);
      expect(core.edits, isEmpty);
      expect(visibleItems(tester), isEmpty);

      await tester.tap(find.byKey(const Key('kind-toggle')));
      await tester.pumpAndSettle();

      expect(core.edits, ['setKind(checklist)', 'addItem()']);
      expect(visibleItems(tester), ['']);
      expect(focusedFieldText(tester), '');
    });

    testWidgets('a text note is a body, with no list under it', (tester) async {
      await pumpScreen(
        tester,
        FakeNoteCore(kind: NoteKind.text, body: 'Call the plumber'),
      );
      expect(bodyField(tester).controller!.text, 'Call the plumber');
      expect(find.byKey(const Key('new-item-field')), findsNothing);
      expect(find.byTooltip('Show checkboxes'), findsOneWidget);
    });

    testWidgets('typing sends the body as it is typed', (tester) async {
      final core = FakeNoteCore(kind: NoteKind.text);
      await pumpScreen(tester, core);

      await tester.enterText(find.byKey(const Key('body-text')), 'milk');
      await tester.pumpAndSettle();
      expect(core.edits, ['editBody( -> milk)']);
      expect(core.current.body, 'milk');
    });

    testWidgets('the switch turns lines into items, and back', (tester) async {
      final core = FakeNoteCore(kind: NoteKind.text, body: 'milk\nbread\n');
      await pumpScreen(tester, core);

      await tester.tap(find.byKey(const Key('kind-toggle')));
      await tester.pumpAndSettle();
      expect(core.edits, ['setKind(checklist)']);
      expect(visibleItems(tester), ['milk', 'bread']);
      expect(find.byKey(const Key('body-text')), findsNothing);
      expect(find.byTooltip('Hide checkboxes'), findsOneWidget);

      await tester.tap(find.byKey(const Key('kind-toggle')));
      await tester.pumpAndSettle();
      expect(bodyField(tester).controller!.text, 'milk\nbread');
      expect(visibleItems(tester), isEmpty);
    });

    testWidgets('a checklist that a merge left text on shows the text', (
      tester,
    ) async {
      // What the peer who was typing while this device converted sees: the
      // rule is that a merge never hides anything.
      final core = FakeNoteCore(items: const ['milk']);
      await pumpScreen(tester, core);
      core.pushRemote(
        NoteView(title: '', body: 'coffee', items: core.current.items),
      );
      await tester.pumpAndSettle();

      expect(bodyField(tester).controller!.text, 'coffee');
      expect(visibleItems(tester), ['milk']);
    });

    testWidgets('a text note that a merge left items on shows the items', (
      tester,
    ) async {
      final core = FakeNoteCore(kind: NoteKind.text, body: 'milk');
      await pumpScreen(tester, core);
      core.pushRemote(
        const NoteView(
          kind: NoteKind.text,
          title: '',
          body: 'milk',
          items: [
            ItemView(id: 'late', text: 'coffee', checked: false, order: 'a'),
          ],
        ),
      );
      await tester.pumpAndSettle();

      expect(visibleItems(tester), ['coffee']);
    });

    testWidgets('someone else typing does not move the caret', (tester) async {
      final core = FakeNoteCore(kind: NoteKind.text, body: 'milk and bread');
      await pumpScreen(tester, core);
      final controller = bodyField(tester).controller!;
      // The caret after "milk", as if this person were about to type there.
      controller.selection = const TextSelection.collapsed(offset: 4);

      // The other device adds a word in front of it, and one after it.
      core.pushRemote(
        const NoteView(
          kind: NoteKind.text,
          title: '',
          body: 'oat milk and bread',
          items: [],
        ),
      );
      await tester.pumpAndSettle();
      expect(controller.text, 'oat milk and bread');
      expect(controller.selection, const TextSelection.collapsed(offset: 8));

      core.pushRemote(
        const NoteView(
          kind: NoteKind.text,
          title: '',
          body: 'oat milk and rye bread',
          items: [],
        ),
      );
      await tester.pumpAndSettle();
      expect(controller.selection, const TextSelection.collapsed(offset: 8));
      expect(core.edits, isEmpty);
    });
  });
}

/// The text of whichever item field has the keyboard, or null.
String? focusedFieldText(WidgetTester tester) => tester
    .widgetList<TextField>(find.byType(TextField))
    .where((field) => field.focusNode?.hasFocus ?? false)
    .map((field) => field.controller?.text)
    .firstOrNull;

/// A core that makes items only when told to, so a test can type into the
/// time it takes.
class _SlowAddCore extends FakeNoteCore {
  _SlowAddCore({super.items});

  Completer<void>? gate;

  @override
  Future<String> addItem(String text) async {
    await gate?.future;
    return super.addItem(text);
  }

  /// A new text comes back later, as it does across the bridge — by when the
  /// field it was typed for may already have the keyboard.
  @override
  Future<void> setItemText(String itemId, String text) async {
    await Future<void>.delayed(const Duration(milliseconds: 30));
    return super.setItemText(itemId, text);
  }
}
