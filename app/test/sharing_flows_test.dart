/// The plan's flow table, one widget test per row
/// (`docs/plan-account-collaboration.md`, "User flows").
///
/// Each test walks what a person does, from the first tap to the end state the
/// table promises, against the faked library: the steps marked *once* happen
/// here because the device starts without an account or a name.
library;

import 'package:easynotes/core/fake_note_core.dart';
import 'package:easynotes/core/fake_note_library.dart';
import 'package:easynotes/core/invites.dart';
import 'package:easynotes/ui/invites.dart';
import 'package:easynotes/ui/note_list_screen.dart';
import 'package:easynotes/ui/note_screen.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

Future<void> pumpNote(WidgetTester tester, FakeNoteLibrary library) async {
  await tester.pumpWidget(
    MaterialApp(
      home: NoteScreen(
        core: FakeNoteCore(title: 'Groceries'),
        storage: FakeStorageHealth(),
        noteId: 'groceries',
        invites: library,
      ),
    ),
  );
  await tester.pumpAndSettle();
}

Future<void> pumpList(WidgetTester tester, FakeNoteLibrary library) async {
  await tester.pumpWidget(MaterialApp(home: NoteListScreen(library: library)));
  await tester.pumpAndSettle();
}

Future<void> tapKey(WidgetTester tester, String key) async {
  final finder = find.byKey(Key(key));
  await tester.ensureVisible(finder);
  await tester.pumpAndSettle();
  await tester.tap(finder);
  await tester.pumpAndSettle();
}

/// Get past the backup check by typing back whichever words it asks for.
Future<void> passBackupCheck(WidgetTester tester) async {
  final written = find.byKey(const Key('phrase-written'));
  await tester.scrollUntilVisible(
    written,
    200,
    scrollable: find.byType(Scrollable).last,
  );
  await tester.tap(written);
  await tester.pumpAndSettle();
  final fields = find.byWidgetPredicate(
    (w) =>
        w.key is ValueKey<String> &&
        (w.key! as ValueKey<String>).value.startsWith('check-word-'),
  );
  for (final element in fields.evaluate().toList()) {
    final key = element.widget.key! as ValueKey<String>;
    final position = int.parse(key.value.substring('check-word-'.length));
    await tester.enterText(
      find.byKey(key),
      FakeNoteLibrary.phrase[position - 1],
    );
  }
  final confirm = find.byKey(const Key('check-confirm'));
  await tester.scrollUntilVisible(
    confirm,
    200,
    scrollable: find.byType(Scrollable).last,
  );
  await tester.tap(confirm);
  await tester.pumpAndSettle();
}

Future<void> giveName(WidgetTester tester, String name) async {
  expect(find.text('Your name'), findsOneWidget);
  await tester.enterText(find.byKey(const Key('profile-name-field')), name);
  await tester.pump();
  await tapKey(tester, 'profile-name-save');
}

Future<void> waitAWhile(WidgetTester tester) async {
  for (var i = 0; i < 10; i++) {
    await tester.pump(const Duration(milliseconds: 20));
  }
}

const olga = Person(
  member: 'me',
  invite: '',
  name: 'Olga',
  owner: true,
  me: true,
);
const anna = Person(member: 'anna', invite: 'i1', name: 'Anna');
const ben = Person(member: 'ben', invite: 'i2', name: 'Ben');

FakeNoteLibrary sharedGroceries() {
  final library = FakeNoteLibrary(
    notes: {'groceries': FakeNoteCore(title: 'Groceries')},
  )..setUpSharing();
  library.setPeople(
    'groceries',
    const People(shared: true, canManage: true, people: [olga, anna, ben]),
  );
  return library;
}

void main() {
  setUp(() {
    invitePollInterval = const Duration(milliseconds: 100);
    inviteBannerPollInterval = const Duration(milliseconds: 200);
  });

  testWidgets(
    'share with sync off says where to set it up and creates nothing',
    (tester) async {
      final library = FakeNoteLibrary();
      await pumpNote(tester, library);
      await tapKey(tester, 'share-button');

      expect(find.text('Set up sync first'), findsOneWidget);
      expect(find.textContaining('Sync & devices'), findsOneWidget);
      await tapKey(tester, 'setup-not-now');
      expect(library.hasAccount, isFalse);
      expect(find.byKey(const Key('new-invite')), findsNothing);
    },
  );

  testWidgets('the join setup sheet speaks to the person joining', (
    tester,
  ) async {
    await tester.pumpWidget(
      const MaterialApp(
        home: Scaffold(body: SharingSetupSheet(joining: true)),
      ),
    );
    expect(find.text('One step before you join'), findsOneWidget);
    expect(find.textContaining('Sync & devices'), findsOneWidget);
    expect(
      find.textContaining(
        RegExp('encrypt|server|key', caseSensitive: false),
      ),
      findsNothing,
    );
  });

  testWidgets('the join dialog stays out of the cryptography', (tester) async {
    final library = FakeNoteLibrary()..setUpSharing('Anna');
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: JoinDialog(
            invites: library,
            initialLink: 'easynotes://join#abc',
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();
    expect(find.text('Open this shared note?'), findsOneWidget);
    expect(find.textContaining('It works once'), findsOneWidget);
    expect(
      find.textContaining(
        RegExp(
          'encrypt|ciphertext|access key|relay|server',
          caseSensitive: false,
        ),
      ),
      findsNothing,
    );
  });

  testWidgets('the wait says who has to be there, without crypto words', (
    tester,
  ) async {
    final library = FakeNoteLibrary()..setUpSharing('Anna');
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: WaitingDialog(
            invites: library,
            join: const PendingJoin(inviteId: 'abc', name: 'Anna'),
          ),
        ),
      ),
    );
    await tester.pump();
    expect(find.text('Joining'), findsOneWidget);
    expect(find.textContaining('EasyNotes open'), findsOneWidget);
    expect(
      find.textContaining(
        RegExp(
          'encrypt|ciphertext|access key|relay|server',
          caseSensitive: false,
        ),
      ),
      findsNothing,
    );
  });

  testWidgets('Share a note: send the link, and they are in', (tester) async {
    final library = FakeNoteLibrary()..setUpSharing('Olga');
    await pumpNote(tester, library);

    await tapKey(tester, 'share-button');
    expect(find.text('Olga (you)'), findsOneWidget);

    await tapKey(tester, 'new-invite');
    final invite = library.invitesFor('groceries').single;
    expect(find.byKey(Key('invite-link-${invite.id}')), findsOneWidget);

    // Anna opens the link, and is in: nothing to approve.
    library.fakeOpened(invite.id, name: 'Anna');
    await tester.pump(invitePollInterval);
    await tester.pumpAndSettle();
    expect(
      find.text('Anna joined. They can open this note now.'),
      findsOneWidget,
    );
    expect(find.text('Anna'), findsOneWidget);
    await tapKey(tester, 'share-done');

    // The second time, nothing is asked again.
    await tapKey(tester, 'share-button');
    expect(find.text('Set up sync first'), findsNothing);
    await tapKey(tester, 'share-done');
  });

  testWidgets(
    'Join a note: open the link, restore once, join as yourself, open it',
    (tester) async {
      final library = FakeNoteLibrary(
        notes: {'hardware': FakeNoteCore(title: 'Hardware')},
      )..setUpSharing('Anna');
      // The link is tapped in a messenger; the app opens on it.
      await tester.pumpWidget(
        MaterialApp(
          home: NoteListScreen(
            library: library,
            initialInviteLink: 'easynotes://join#abc',
          ),
        ),
      );
      await tester.pumpAndSettle();

      // The join dialog has no name field, and nothing to paste: it says
      // whose name it uses and asks for a yes.
      expect(find.byKey(const Key('join-name-field')), findsNothing);
      expect(find.byKey(const Key('join-link-field')), findsNothing);
      expect(find.text('Join as Anna'), findsOneWidget);
      await tester.tap(find.byKey(const Key('join-confirm')));
      await waitAWhile(tester);
      expect(library.calls, contains('requestJoin(easynotes://join#abc)'));
      expect(find.byKey(const Key('join-waiting')), findsOneWidget);

      library.fakeAnswer('abc', JoinState.joined);
      await tester.pump(invitePollInterval);
      await tester.pumpAndSettle();
      expect(
        library.calls.where((c) => c.startsWith('openNote(joined-')),
        hasLength(1),
      );
    },
  );

  testWidgets('a link that hands the key opens the note without waiting', (
    tester,
  ) async {
    final library = FakeNoteLibrary(
      notes: {'hardware': FakeNoteCore(title: 'Hardware')},
    )..setUpSharing('Anna');
    library.grantOnJoin = 'hardware';
    await tester.pumpWidget(
      MaterialApp(
        home: NoteListScreen(
          library: library,
          initialInviteLink: 'easynotes://join#abc',
        ),
      ),
    );
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('join-confirm')));
    await tester.pumpAndSettle();

    // Nothing to wait for: the link carried the key, and the note opens.
    expect(find.byKey(const Key('join-waiting')), findsNothing);
    expect(library.calls.where((c) => c == 'openNote(hardware)'), hasLength(1));
    // The request is still answered in the background, so a decline can
    // take the note away again.
    expect(library.pendingJoins, hasLength(1));
  });

  testWidgets('the banner leaves a wait the dialog is showing for it', (
    tester,
  ) async {
    // The banner looks far more often than the dialog here, so it is the one
    // that would see the answer first.
    inviteBannerPollInterval = const Duration(milliseconds: 50);
    invitePollInterval = const Duration(seconds: 1);
    final library = FakeNoteLibrary()..setUpSharing('Anna');
    await tester.pumpWidget(
      MaterialApp(
        home: NoteListScreen(
          library: library,
          initialInviteLink: 'easynotes://join#abc',
        ),
      ),
    );
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('join-confirm')));
    await waitAWhile(tester);
    expect(find.byKey(const Key('join-waiting')), findsOneWidget);

    library.fakeAnswer('abc', JoinState.joined);
    await tester.pump(const Duration(milliseconds: 50));
    await tester.pump();

    // The banner said nothing and took nothing: the dialog still has the
    // answer to show, and will open the note with it.
    expect(find.byKey(const Key('joined-in-background')), findsNothing);
    expect(find.byKey(const Key('join-waiting')), findsOneWidget);

    await tester.pump(const Duration(seconds: 1));
    await tester.pumpAndSettle();
    expect(
      library.calls.where((c) => c.startsWith('openNote(joined-')),
      hasLength(1),
    );
    expect(find.byKey(const Key('joined-in-background')), findsNothing);
  });

  testWidgets(
    'Get a new phone: twelve words bring every note back, shared ones too',
    (tester) async {
      final library = FakeNoteLibrary();
      library.restoredWithAccount['groceries'] = FakeNoteCore(
        title: 'Groceries',
      );
      library.restoredWithAccount['from-anna'] = FakeNoteCore(
        title: 'Anna\'s shared list',
      );

      // A new device with no notes: the twelve words are the way in.
      await library.adoptAccount(FakeNoteLibrary.phrase.join(' '));
      expect(library.hasAccount, isTrue);

      await pumpList(tester, library);
      expect(find.text('Groceries'), findsOneWidget);
      expect(find.text('Anna\'s shared list'), findsOneWidget);
      expect(library.calls.where((c) => c.startsWith('requestJoin')), isEmpty);
    },
  );

  testWidgets('Remove someone: Remove next to their name, confirm', (
    tester,
  ) async {
    final library = sharedGroceries()..online = false;
    await pumpNote(tester, library);
    await tapKey(tester, 'share-button');
    expect(find.text('Names are chosen by each person.'), findsOneWidget);

    await tapKey(tester, 'remove-anna');
    expect(find.text('Remove Anna?'), findsOneWidget);
    expect(
      find.text(
        'Anna will lose this note on every device. Anything Anna already '
        'saved or copied can\'t be taken back.',
      ),
      findsOneWidget,
    );
    await tapKey(tester, 'cancel-remove');
    expect(library.calls.where((c) => c.startsWith('removePerson')), isEmpty);

    await tapKey(tester, 'remove-anna');
    await tapKey(tester, 'confirm-remove');
    expect(library.calls, contains('removePerson(groceries, anna)'));
    // Offline: the row stays, and says it has not happened yet.
    expect(find.text('Removing… finishes when you\'re online'), findsOneWidget);
    expect(find.byKey(const Key('remove-anna')), findsNothing);

    // Online: the row goes only now, and Ben is untouched.
    library.finishRemovals();
    await tester.pump(invitePollInterval);
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('person-anna')), findsNothing);
    expect(find.byKey(const Key('person-ben')), findsOneWidget);
    await tapKey(tester, 'share-done');
  });

  testWidgets('Stop sharing: confirm, and everyone else is gone', (
    tester,
  ) async {
    final library = sharedGroceries();
    await library.createInvite('groceries');
    await pumpNote(tester, library);
    await tapKey(tester, 'share-button');

    await tapKey(tester, 'stop-sharing');
    expect(find.text('Stop sharing?'), findsOneWidget);
    expect(
      find.text(
        'Everyone else will lose this note on every device, and every open '
        'link stops working. Anything they already saved or copied can\'t '
        'be taken back.',
      ),
      findsOneWidget,
    );
    await tapKey(tester, 'confirm-stop-sharing');

    expect(library.calls, contains('stopSharing(groceries)'));
    // It also cancels the open link.
    expect(library.invitesFor('groceries'), isEmpty);
    expect(find.byKey(const Key('person-anna')), findsNothing);
    expect(find.byKey(const Key('person-ben')), findsNothing);
    expect(find.text('Olga (you)'), findsOneWidget);
    expect(find.byKey(const Key('stop-sharing')), findsNothing);
    await tapKey(tester, 'share-done');
  });

  testWidgets('Be removed: nothing to do, and one message', (tester) async {
    final library = FakeNoteLibrary(
      notes: {
        'groceries': FakeNoteCore(title: 'Groceries'),
        'hardware': FakeNoteCore(title: 'Hardware'),
      },
    )..setUpSharing('Anna');
    await pumpList(tester, library);
    expect(find.text('Groceries'), findsOneWidget);

    library.fakeRemoved('groceries');
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('note:groceries')), findsNothing);
    expect(find.byKey(const Key('note:hardware')), findsOneWidget);
    expect(
      find.text('You no longer have access to ‘Groceries’.'),
      findsOneWidget,
    );
    // Once.
    await tester.pump(const Duration(seconds: 10));
    await tester.pumpAndSettle();
    expect(library.takeRemovals(), isEmpty);
  });
}
