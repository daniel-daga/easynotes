/// Widget tests for the account screens, against a faked library.
///
/// The claim worth holding is the backup check's: a phrase is shown once, and
/// the screen showing it cannot be left by the ordinary gestures until the
/// person has typed back words that prove it was written down. Everything else
/// here — the drawer line, the second device's field — is the way in and out.
library;

import 'package:easynotes/core/fake_note_library.dart';
import 'package:easynotes/core/relay_profiles.dart';
import 'package:easynotes/ui/account_screen.dart';
import 'package:easynotes/ui/invites.dart' show JoinDialog;
import 'package:easynotes/ui/note_list_screen.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

/// The words the check asks for in these tests. Fixed so a test can type them;
/// the screen picks at random everywhere else.
const ask = [2, 7, 11];

String word(int position) => FakeNoteLibrary.phrase[position - 1];

Future<void> pumpAccount(WidgetTester tester, FakeNoteLibrary library) async {
  await tester.pumpWidget(
    MaterialApp(
      home: AccountScreen(library: library, ask: ask),
    ),
  );
  await tester.pumpAndSettle();
}

/// Tap something that may be below the fold, scrolling to it the way a person
/// on a small phone would. The screens are lists, so what is off-screen is not
/// built at all until it is scrolled to.
Future<void> tapKey(WidgetTester tester, String key) async {
  final finder = find.byKey(Key(key));
  await tester.scrollUntilVisible(
    finder,
    200,
    scrollable: find.byType(Scrollable).last,
  );
  await tester.tap(finder);
  await tester.pumpAndSettle();
}

/// Set up an account and get as far as the check.
Future<void> toTheCheck(WidgetTester tester) async {
  await tester.tap(find.byKey(const Key('create-account-button')));
  await tester.pumpAndSettle();
  await tapKey(tester, 'phrase-written');
}

Future<void> answer(WidgetTester tester, Map<int, String> words) async {
  for (final MapEntry(key: position, value: text) in words.entries) {
    await tester.enterText(find.byKey(Key('check-word-$position')), text);
  }
  await tapKey(tester, 'check-confirm');
}

/// A library whose adoption fails the way the core's does for a phrase with a
/// broken checksum, which the fake alone cannot: it only counts words, and the
/// screen will not submit anything but twelve.
class _RefusingLibrary extends FakeNoteLibrary {
  @override
  Future<void> adoptAccount(String words) async {
    calls.add('adoptAccount($words)');
    throw 'those are all real words, but not in that order';
  }
}

void main() {
  testWidgets('the drawer says a device is on its own, and opens the screen', (
    tester,
  ) async {
    final library = FakeNoteLibrary();
    await tester.pumpWidget(
      MaterialApp(home: NoteListScreen(library: library)),
    );
    await tester.pumpAndSettle();

    final scaffold = tester.firstState<ScaffoldState>(find.byType(Scaffold));
    scaffold.openDrawer();
    await tester.pumpAndSettle();
    expect(find.text('Only on this device'), findsOneWidget);

    await tester.tap(find.byKey(const Key('account-entry')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('account-no')), findsOneWidget);
  });

  testWidgets('shows all twelve words, numbered, once', (tester) async {
    final library = FakeNoteLibrary();
    await pumpAccount(tester, library);

    await tester.tap(find.byKey(const Key('create-account-button')));
    await tester.pumpAndSettle();

    expect(library.calls, contains('createAccount()'));
    for (var i = 1; i <= 12; i++) {
      final tile = find.byKey(Key('phrase-word-$i'));
      expect(tile, findsOneWidget);
      expect(
        find.descendant(of: tile, matching: find.textContaining(word(i))),
        findsOneWidget,
      );
    }
  });

  testWidgets('the right words pass the check, and the device has an account', (
    tester,
  ) async {
    final library = FakeNoteLibrary();
    await pumpAccount(tester, library);
    await toTheCheck(tester);

    // Case and stray spaces are not what the check is about.
    await answer(tester, {
      2: ' ${word(2).toUpperCase()} ',
      7: word(7),
      11: word(11),
    });

    expect(find.byType(RecoveryPhraseScreen), findsNothing);
    expect(find.byKey(const Key('account-yes')), findsOneWidget);
    expect(library.hasAccount, isTrue);
    // Minted once. A second createAccount would be a second phrase, and the
    // one on paper would be for an account nobody uses.
    expect(library.calls.where((c) => c == 'createAccount()'), hasLength(1));
  });

  testWidgets('a wrong word keeps the person on the check and says which', (
    tester,
  ) async {
    await pumpAccount(tester, FakeNoteLibrary());
    await toTheCheck(tester);

    await answer(tester, {2: word(2), 7: word(8), 11: word(11)});

    expect(find.byType(RecoveryPhraseScreen), findsOneWidget);
    expect(find.text('That is not word 7'), findsOneWidget);
    expect(find.text('That is not word 2'), findsNothing);
  });

  testWidgets('the words can be shown again from the check', (tester) async {
    await pumpAccount(tester, FakeNoteLibrary());
    await toTheCheck(tester);

    await tapKey(tester, 'check-show-again');

    expect(find.byKey(const Key('phrase-word-1')), findsOneWidget);
    // The same words, not a new phrase: the account is already minted.
    expect(find.textContaining(word(1)), findsOneWidget);
  });

  testWidgets('going back before the check asks first, and can stay', (
    tester,
  ) async {
    await pumpAccount(tester, FakeNoteLibrary());
    await tester.tap(find.byKey(const Key('create-account-button')));
    await tester.pumpAndSettle();

    // The system back gesture, which is how the phrase would be lost by
    // accident on a phone.
    await tester.binding.handlePopRoute();
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('phrase-leave-dialog')), findsOneWidget);

    await tester.tap(find.byKey(const Key('phrase-stay')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('phrase-word-1')), findsOneWidget);
  });

  testWidgets('leaving anyway is possible, but only by saying so', (
    tester,
  ) async {
    await pumpAccount(tester, FakeNoteLibrary());
    await toTheCheck(tester);

    await tester.tap(find.byType(BackButton));
    await tester.pumpAndSettle();
    expect(find.byType(RecoveryPhraseScreen), findsOneWidget);

    await tester.tap(find.byKey(const Key('phrase-leave')));
    await tester.pumpAndSettle();
    expect(find.byType(RecoveryPhraseScreen), findsNothing);
    expect(find.byKey(const Key('account-yes')), findsOneWidget);
  });

  testWidgets('the check picks three different words when left to itself', (
    tester,
  ) async {
    await tester.pumpWidget(
      const MaterialApp(
        home: RecoveryPhraseScreen(words: FakeNoteLibrary.phrase),
      ),
    );
    await tapKey(tester, 'phrase-written');

    expect(find.byType(TextField), findsNWidgets(backupCheckWords));
  });

  testWidgets('a second device takes twelve words and not eleven', (
    tester,
  ) async {
    final library = FakeNoteLibrary();
    await pumpAccount(tester, library);
    await tester.tap(find.byKey(const Key('adopt-account-button')));
    await tester.pumpAndSettle();

    final eleven = FakeNoteLibrary.phrase.take(11).join(' ');
    await tester.enterText(find.byKey(const Key('adopt-phrase-field')), eleven);
    await tester.pump();
    expect(find.text('11 of 12 words'), findsOneWidget);
    expect(
      tester
          .widget<FilledButton>(find.byKey(const Key('adopt-confirm')))
          .onPressed,
      isNull,
    );

    await tester.enterText(
      find.byKey(const Key('adopt-phrase-field')),
      FakeNoteLibrary.phrase.join('  \n'),
    );
    await tester.pump();
    await tester.tap(find.byKey(const Key('adopt-confirm')));
    await tester.pumpAndSettle();

    expect(library.hasAccount, isTrue);
    expect(find.byKey(const Key('account-yes')), findsOneWidget);
    // An empty grid is what comes next, so the screen says why.
    expect(find.byKey(const Key('account-message')), findsOneWidget);
  });

  testWidgets('a phrase the core refuses is refused in its words', (
    tester,
  ) async {
    final library = _RefusingLibrary();
    await pumpAccount(tester, library);
    await tester.tap(find.byKey(const Key('adopt-account-button')));
    await tester.pumpAndSettle();

    await tester.enterText(
      find.byKey(const Key('adopt-phrase-field')),
      FakeNoteLibrary.phrase.join(' '),
    );
    await tester.pump();
    await tester.tap(find.byKey(const Key('adopt-confirm')));
    await tester.pumpAndSettle();

    expect(find.byType(AdoptAccountScreen), findsOneWidget);
    expect(find.textContaining('not in that order'), findsOneWidget);
    expect(library.hasAccount, isFalse);
  });

  testWidgets('a device with an account offers neither button', (tester) async {
    final library = FakeNoteLibrary();
    await library.createAccount();
    await pumpAccount(tester, library);

    expect(find.byKey(const Key('account-yes')), findsOneWidget);
    expect(find.byKey(const Key('create-account-button')), findsNothing);
    expect(find.byKey(const Key('adopt-account-button')), findsNothing);
  });

  group('the relay the account belongs to', () {
    // Each relay has an account of its own, so a phrase restores only from
    // the relay it was made on. The screens say which that is.
    const relay = RelayProfile(id: 'relay', url: 'https://relay.example');
    const local = RelayProfile(id: 'local', url: null);

    testWidgets('the account screen names it', (tester) async {
      await tester.pumpWidget(
        MaterialApp(
          home: AccountScreen(library: FakeNoteLibrary(), profile: relay),
        ),
      );
      await tester.pumpAndSettle();
      await tester.scrollUntilVisible(
        find.byKey(const Key('account-relay')),
        200,
        scrollable: find.byType(Scrollable).last,
      );
      expect(find.text('https://relay.example'), findsOneWidget);
      expect(find.textContaining('choose the same relay'), findsOneWidget);
    });

    testWidgets('restoring says where the notes come from', (tester) async {
      await tester.pumpWidget(
        MaterialApp(
          home: AccountScreen(library: FakeNoteLibrary(), profile: relay),
        ),
      );
      await tester.pumpAndSettle();
      await tapKey(tester, 'adopt-account-button');
      expect(
        find.textContaining('Your notes come back from https://relay.example'),
        findsOneWidget,
      );
    });

    testWidgets('restoring with no relay says it cannot bring notes back', (
      tester,
    ) async {
      await tester.pumpWidget(
        MaterialApp(
          home: AdoptAccountScreen(library: FakeNoteLibrary(), profile: local),
        ),
      );
      await tester.pumpAndSettle();
      expect(
        find.textContaining('This device is not using a relay'),
        findsOneWidget,
      );
    });

    testWidgets('joining by link names the relay both people need', (
      tester,
    ) async {
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: JoinDialog(invites: FakeNoteLibrary(), profile: relay),
          ),
        ),
      );
      await tester.pumpAndSettle();
      expect(
        find.textContaining('Joining through https://relay.example'),
        findsOneWidget,
      );
    });
  });

  group('deleting the account', () {
    const relay = RelayProfile(id: 'relay', url: 'https://relay.example');

    Future<void> pumpWith(
      WidgetTester tester,
      FakeNoteLibrary library, {
      RelayProfile profile = relay,
      Future<void> Function()? onDeleted,
    }) async {
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: Builder(
              builder: (context) => TextButton(
                onPressed: () => Navigator.of(context).push(
                  MaterialPageRoute<void>(
                    builder: (_) => AccountScreen(
                      library: library,
                      profile: profile,
                      onDeleted: onDeleted,
                    ),
                  ),
                ),
                child: const Text('open'),
              ),
            ),
          ),
        ),
      );
      await tester.tap(find.text('open'));
      await tester.pumpAndSettle();
    }

    Future<void> openDelete(WidgetTester tester) async {
      final tile = find.byKey(const Key('delete-account'));
      await tester.scrollUntilVisible(
        tile,
        200,
        scrollable: find.byType(Scrollable).last,
      );
      await tester.ensureVisible(tile);
      await tester.pumpAndSettle();
      await tester.tap(tile);
      await tester.pumpAndSettle();
    }

    testWidgets('says what goes and what cannot, and needs the word typed', (
      tester,
    ) async {
      final library = FakeNoteLibrary()..setUpSharing();
      await pumpWith(tester, library);
      await openDelete(tester);
      expect(find.textContaining('https://relay.example'), findsWidgets);
      expect(
        find.textContaining('keep what is already on their devices'),
        findsOneWidget,
      );
      expect(find.textContaining('stay theirs'), findsOneWidget);
      expect(find.textContaining('up to 30 days'), findsOneWidget);

      final confirm = find.byKey(const Key('delete-account-confirm'));
      expect(tester.widget<FilledButton>(confirm).onPressed, isNull);
      await tester.enterText(
        find.byKey(const Key('delete-account-word')),
        'del',
      );
      await tester.pump();
      expect(tester.widget<FilledButton>(confirm).onPressed, isNull);
      await tester.tap(find.text('Cancel'));
      await tester.pumpAndSettle();
      expect(library.calls, isNot(contains('deleteAccount()')));
    });

    testWidgets('deletes, returns to the start and reopens the profile', (
      tester,
    ) async {
      final library = FakeNoteLibrary()..setUpSharing();
      var reopened = 0;
      await pumpWith(tester, library, onDeleted: () async => reopened++);
      await openDelete(tester);
      await tester.enterText(
        find.byKey(const Key('delete-account-word')),
        'Delete',
      );
      await tester.pump();
      await tester.tap(find.byKey(const Key('delete-account-confirm')));
      await tester.pumpAndSettle();

      expect(library.calls, contains('deleteAccount()'));
      expect(library.hasAccount, isFalse);
      expect(reopened, 1);
      expect(find.byType(AccountScreen), findsNothing);
      expect(find.byKey(const Key('account-deleted')), findsOneWidget);
    });

    testWidgets('a relay that refuses leaves the person where they were, '
        'told why', (tester) async {
      final library = FakeNoteLibrary()
        ..setUpSharing()
        ..deleteAccountFails = 'The relay could not delete this account';
      var reopened = 0;
      await pumpWith(tester, library, onDeleted: () async => reopened++);
      await openDelete(tester);
      await tester.enterText(
        find.byKey(const Key('delete-account-word')),
        'delete',
      );
      await tester.pump();
      await tester.tap(find.byKey(const Key('delete-account-confirm')));
      await tester.pumpAndSettle();

      expect(library.hasAccount, isTrue);
      expect(reopened, 0);
      expect(find.byType(AccountScreen), findsOneWidget);
      expect(
        find.textContaining('could not delete this account'),
        findsOneWidget,
      );
    });

    testWidgets('a profile with no relay says nothing was ever on one', (
      tester,
    ) async {
      await pumpWith(
        tester,
        FakeNoteLibrary(),
        profile: const RelayProfile(id: 'local', url: null),
      );
      await openDelete(tester);
      expect(
        find.textContaining('Nothing here was ever on a relay'),
        findsOneWidget,
      );
    });
  });
}
