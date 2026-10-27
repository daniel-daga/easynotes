/// Drives the *real* Rust core across the flutter_rust_bridge boundary.
///
/// The widget tests use a fake, so nothing else in `flutter test` would notice
/// if the bridge stopped working. This does: it loads the compiled native
/// library, edits a note through the generated bindings, and checks that the
/// state comes back out the way the UI will read it.
///
/// Skipped unless `EASYNOTES_NATIVE_LIB` points at the built cdylib, so the
/// suite still runs on a machine with no Rust toolchain. `just test-app` and CI
/// both set it.
library;

import 'dart:io';

import 'package:easynotes/core/browser_store.dart';
import 'package:easynotes/core/master_key.dart';
import 'package:easynotes/core/note_core.dart';
import 'package:easynotes/core/note_library.dart';
import 'package:easynotes/core/rust_note_core.dart';
import 'package:easynotes/core/rust_note_library.dart';
import 'package:easynotes/core/sync_status.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:easynotes/src/rust/api/notes.dart'
    show Library, NoteKindView, StorageArea, StorageBatch, StorageRow;
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:flutter_test/flutter_test.dart';

/// Label names, as a person would read them off a card.
List<String> names(List<Label> labels) => [for (final l in labels) l.name];

/// Every label with how many notes carry it, dropping the IDs — which are
/// random and so cannot be written down in a test.
List<(String, int)> counted(List<Tag> tags) => [
  for (final tag in tags) (tag.name, tag.notes),
];

void main() {
  final libPath = Platform.environment['EASYNOTES_NATIVE_LIB'];

  setUpAll(() async {
    if (libPath == null) return;
    await RustNoteCore.ensureInitialized(
      library: ExternalLibrary.open(libPath),
    );
  });

  /// A library on a fresh directory, torn down with it.
  ///
  /// The master key is made here rather than read from a keychain, which is
  /// what makes the store openable from a test at all — and which is the shape
  /// the app has too: the key comes from outside the core, and reopening the
  /// same directory means handing back the same key.
  Future<RustNoteLibrary> openLibraryInTempDir([
    Directory? existing,
    Uint8List? key,
  ]) async {
    final dir =
        existing ?? await Directory.systemTemp.createTemp('easynotes-test');
    final library = await RustNoteLibrary.open(
      dir.path,
      key ?? MasterKeyStore.newKey(),
    );
    addTearDown(() async {
      await library.dispose();
      if (existing == null) await dir.delete(recursive: true);
    });
    return library;
  }

  /// One new note in a library of its own, which is what most of these want.
  Future<NoteCore> openInTempDir() async {
    final library = await openLibraryInTempDir();
    final core = await library.openNote(await library.createNote());
    addTearDown(core.dispose);
    return core;
  }

  group(
    'the Rust core across the bridge',
    () {
      test(
        'overlapping lifecycle transitions do not leave a stale suspension',
        () async {
          final library = await openLibraryInTempDir();
          await Future.wait([
            library.setForeground(false),
            Future<void>.delayed(
              Duration.zero,
            ).then((_) => library.setForeground(true)),
          ]);
          expect(library.syncStatus.state, isNot(SyncState.paused));

          // Repeated transitions and disposal also retire the pending grace timer.
          await library.setForeground(false);
          await library.setForeground(true);
          await library.setForeground(false);
          await library.dispose();
        },
      );
      test('an empty note starts empty', () async {
        final core = await openInTempDir();
        expect(core.current.title, isEmpty);
        expect(core.current.items, isEmpty);
      });

      test('edits come back on the watch stream', () async {
        // The whole architecture rests on this: the UI never mutates state, it
        // waits to be told. If the stream does not fire, the app renders nothing.
        final core = await openInTempDir();
        // Subscribed before the edits, so nothing is missed, and awaited rather
        // than pumped: the watcher is a Rust task, and how soon it runs is the
        // machine's business rather than this test's.
        final arrived = core
            .watch()
            .firstWhere(
              (view) =>
                  view.title == 'Groceries' &&
                  view.items.map((i) => i.text).join(',') == 'milk,bread',
            )
            .timeout(
              const Duration(seconds: 10),
              onTimeout: () => fail('the watch stream never caught up'),
            );

        await core.setTitle('Groceries');
        await core.addItem('milk');
        await core.addItem('bread');

        // A watch channel keeps only the latest value, so an intermediate view
        // may or may not be delivered. The last one may not be skipped.
        await arrived;
      });

      test('items keep the order they were added in', () async {
        final core = await openInTempDir();
        for (final text in ['milk', 'bread', 'eggs']) {
          await core.addItem(text);
        }
        expect(core.current.items.map((i) => i.text), [
          'milk',
          'bread',
          'eggs',
        ]);
      });

      test(
        'a text note keeps its body and switches to a list and back',
        () async {
          final library = await openLibraryInTempDir();
          final id = await library.createNote(kind: NoteKind.text);
          final core = await library.openNote(id);
          addTearDown(core.dispose);
          expect(core.current.kind, NoteKind.text);

          await core.editBody('', 'milk\nbread');
          // The same text again is not an edit, and must not fail as one.
          await core.editBody('milk\nbread', 'milk\nbread');
          expect(core.current.body, 'milk\nbread');
          expect(library.notes.single.kind, NoteKind.text);
          expect(library.notes.single.preview, ['milk', 'bread']);

          await core.setKind(NoteKind.checklist);
          expect(core.current.kind, NoteKind.checklist);
          expect(core.current.body, isEmpty);
          expect(core.current.items.map((i) => i.text), ['milk', 'bread']);

          await core.setKind(NoteKind.text);
          expect(core.current.body, 'milk\nbread');
          expect(core.current.items, isEmpty);
        },
      );

      test('checking, renaming and deleting an item all round-trip', () async {
        final core = await openInTempDir();
        final milk = await core.addItem('milk');
        final eggs = await core.addItem('eggs');

        await core.setChecked(milk, true);
        await core.setItemText(eggs, 'eggs (12)');
        expect(
          core.current.items.firstWhere((i) => i.id == milk).checked,
          isTrue,
        );
        expect(
          core.current.items.firstWhere((i) => i.id == eggs).text,
          'eggs (12)',
        );

        await core.removeItem(milk);
        expect(core.current.items.map((i) => i.text), ['eggs (12)']);
      });

      test('a text write for a removed item says so by name', () async {
        final core = await openInTempDir();
        final milk = await core.addItem('milk');
        await core.removeItem(milk);

        // What a keystroke queued behind a backspace removal meets. The screen
        // tells it apart from any other refusal by name and drops the write.
        await expectLater(
          core.editItemText(milk, 'milk', 'milk and'),
          throwsA(isA<UnknownItemException>()),
        );
      });

      test('moving an item lands it where the UI asked', () async {
        final core = await openInTempDir();
        await core.addItem('one');
        await core.addItem('two');
        final three = await core.addItem('three');

        await core.moveItem(three, 0);
        expect(core.current.items.map((i) => i.text), ['three', 'one', 'two']);
      });

      test('a relaunch restores the list from disk', () async {
        // Demo step 6, through the bridge: same directory, new library, same
        // notes — and the note IDs it had, because a grid that renamed everything
        // on a restart would be a different grid.
        final dir = await Directory.systemTemp.createTemp('easynotes-restart');
        addTearDown(() => dir.delete(recursive: true));
        // The same key both times, as a keychain would give.
        final key = MasterKeyStore.newKey();

        final first = await RustNoteLibrary.open(dir.path, key);
        final id = await first.createNote();
        final note = await first.openNote(id);
        await note.setTitle('Groceries');
        for (final text in ['milk', 'bread', 'eggs']) {
          await note.addItem(text);
        }
        await note.dispose();
        await first.dispose();

        final second = await openLibraryInTempDir(dir, key);
        expect(second.notes.map((n) => n.id), [id]);
        expect(second.notes.single.title, 'Groceries');
        expect(second.notes.single.items, 3);

        final reopened = await second.openNote(id);
        addTearDown(reopened.dispose);
        expect(reopened.current.items.map((i) => i.text), [
          'milk',
          'bread',
          'eggs',
        ]);
      });

      test('several notes live side by side, newest first', () async {
        final library = await openLibraryInTempDir();
        final groceries = await library.openNote(await library.createNote());
        addTearDown(groceries.dispose);
        await groceries.setTitle('Groceries');
        final hardware = await library.openNote(await library.createNote());
        addTearDown(hardware.dispose);
        await hardware.setTitle('Hardware');
        await hardware.addItem('screws');

        expect(library.notes.map((n) => n.title), ['Hardware', 'Groceries']);
        expect(library.notes.first.preview, ['screws']);
        // Editing one does not touch the other.
        expect(groceries.current.items, isEmpty);
      });

      test('the note list arrives on its own stream', () async {
        final library = await openLibraryInTempDir();
        final arrived = library
            .watchNotes()
            .firstWhere(
              (notes) => notes.length == 1 && notes.single.title == 'Groceries',
            )
            .timeout(
              const Duration(seconds: 10),
              onTimeout: () => fail('the note list stream never fired'),
            );

        final note = await library.openNote(await library.createNote());
        addTearDown(note.dispose);
        await note.setTitle('Groceries');

        await arrived;
      });

      // -- search ------------------------------------------------------------
      //
      // Item 7 of the second milestone, over the real index: the text is inside an
      // envelope inside the CRDT, so these are the only tests that prove the
      // whole path from a query typed on a screen to a note found on a disk.

      test('a search finds a note by something on it', () async {
        final library = await openLibraryInTempDir();
        final groceries = await library.openNote(await library.createNote());
        addTearDown(groceries.dispose);
        await groceries.setTitle('Groceries');
        for (final item in ['milk', 'bread', 'eggs', 'coffee', 'asparagus']) {
          await groceries.addItem(item);
        }
        final hardware = await library.openNote(await library.createNote());
        addTearDown(hardware.dispose);
        await hardware.setTitle('Hardware');
        await hardware.addItem('screws');

        final hits = await library.search('aspara');
        expect(hits, hasLength(1));
        expect(hits.single.note.title, 'Groceries');
        // The fifth item, which the four-item preview does not show — so the
        // line has to come back with the hit or the card says nothing.
        expect(hits.single.matched, 'asparagus');
        expect(hits.single.note.preview, isNot(contains('asparagus')));

        // Both words, and they need not be on the same line.
        expect(await library.search('groceries milk'), hasLength(1));
        expect(await library.search('screws asparagus'), isEmpty);
        expect(await library.search('cortado'), isEmpty);
      });

      test('a relaunch can search a note it has not opened', () async {
        final dir = await Directory.systemTemp.createTemp('easynotes-search');
        addTearDown(() => dir.delete(recursive: true));
        final key = MasterKeyStore.newKey();

        {
          final library = await openLibraryInTempDir(dir, key);
          final core = await library.openNote(await library.createNote());
          addTearDown(core.dispose);
          await core.setTitle('Groceries');
          await core.addItem('asparagus');
        } // the app is killed here

        // Nothing has opened the note in this process, which is the whole
        // point: the title and the items are read out of the sealed index.
        final library = await openLibraryInTempDir(dir, key);
        expect(library.notes.map((n) => n.title), ['Groceries']);
        final hits = await library.search('aspara');
        expect(hits, hasLength(1));
        expect(hits.single.matched, 'asparagus');

        // And the note still opens, with everything that was on it.
        final core = await library.openNote(hits.single.note.id);
        addTearDown(core.dispose);
        expect(core.current.title, 'Groceries');
        expect(core.current.items.map((i) => i.text), ['asparagus']);
      });

      // -- the master key ----------------------------------------------------
      //
      // Item 3b of the second milestone: the key that seals everything local lives in
      // the OS keychain, and Dart is what can reach one. These run against the
      // plugin's own test double — what they are about is the order of
      // preference, which is the part that would cost someone their notes.

      test('a device that had its key in a file keeps using that key', () async {
        FlutterSecureStorage.setMockInitialValues({});
        final dir = await Directory.systemTemp.createTemp('easynotes-adopt');
        addTearDown(() => dir.delete(recursive: true));

        // A device as it was before the keychain: a key in a file, and a database
        // that opens with that key and no other.
        final existing = MasterKeyStore.newKey();
        await File('${dir.path}/master.key').writeAsBytes(existing);
        final before = await RustNoteLibrary.open(dir.path, existing);
        final note = await before.openNote(await before.createNote());
        await note.setTitle('Groceries');
        await note.dispose();
        await before.dispose();

        // What the first launch after the update does.
        final adopted = await MasterKeyStore().forDirectory(dir.path);
        expect(
          adopted,
          existing,
          reason: 'the key beside the database was not adopted',
        );

        final after = await RustNoteLibrary.open(dir.path, adopted);
        addTearDown(after.dispose);
        expect(after.notes.single.title, 'Groceries');
      });

      test('a device with no key gets one, and finds it again', () async {
        FlutterSecureStorage.setMockInitialValues({});
        final dir = await Directory.systemTemp.createTemp('easynotes-newkey');
        addTearDown(() => dir.delete(recursive: true));

        final store = MasterKeyStore();
        final first = await store.forDirectory(dir.path);
        expect(first, hasLength(masterKeyBytes));
        // The second launch reads the keychain rather than minting another key,
        // which is the difference between an app that keeps notes and one that
        // loses them every time it starts.
        expect(await store.forDirectory(dir.path), first);
        expect(
          File('${dir.path}/master.key').existsSync(),
          isFalse,
          reason: 'the key was written beside the database after all',
        );
      });

      test('the wrong key does not quietly open an empty device', () async {
        final dir = await Directory.systemTemp.createTemp('easynotes-wrongkey');
        addTearDown(() => dir.delete(recursive: true));

        final right = await RustNoteLibrary.open(
          dir.path,
          MasterKeyStore.newKey(),
        );
        final note = await right.openNote(await right.createNote());
        await note.setTitle('Groceries');
        await note.dispose();
        await right.dispose();

        // Someone whose notes are right there on the disk should be told that
        // this key does not open them, not shown an app with nothing in it.
        expect(
          () => RustNoteLibrary.open(dir.path, MasterKeyStore.newKey()),
          throwsA(anything),
        );
      });

      test('a watcher started after its handle stopped still ends', () async {
        // Dart starts a watcher and stops it in that order, but the bridge
        // runs the two calls on different threads, and the stop can land
        // first. Its one wake then went to nobody, the watcher parked for
        // good, and cancelling the stream waited on it forever: a note or
        // library disposed right after it was opened hung the dispose. Here
        // the stop comes first on purpose, for every watcher there is.
        final library = Library.openInBrowser(
          notes: const [],
          keys: const [],
          masterKey: MasterKeyStore.newKey(),
          mediaDatabase: 'unused',
        );
        addTearDown(library.close);
        final note = await library.note(
          id: await library.createNote(kind: NoteKindView.checklist),
        );

        await note.stopWatching();
        await library.stopWatching();
        for (final stream in <Stream<Object?>>[
          note.watch(),
          note.watchSync(),
          library.watchNotes(),
          library.watchSync(),
        ]) {
          // Each still says what it has, and then closes.
          await expectLater(stream, emitsInOrder([anything, emitsDone]));
        }
      });

      test('a warm restart retires the library it never disposed', () async {
        // Android backs out of the app by destroying its isolate and keeping
        // the process, and nothing disposes the library on the way out. The
        // next launch opens the same directory in the same process; before
        // this, that made a second library on one SQLite file, with the first
        // one's sync engine still running beside it. Here the first library is
        // simply never disposed, which is all a dead isolate amounts to.
        final dir = await Directory.systemTemp.createTemp('easynotes-warm');
        addTearDown(() => dir.delete(recursive: true));
        final key = MasterKeyStore.newKey();

        final orphan = await RustNoteLibrary.open(dir.path, key);
        final id = await orphan.createNote();

        final relaunched = await openLibraryInTempDir(dir, key);
        expect(relaunched.notes.map((n) => n.id), [id]);
        await relaunched.createNote();

        // The orphan is closed, not still writing to the file behind the new
        // library's back.
        expect(() => orphan.createNote(), throwsA(anything));
        await orphan.dispose();
      });

      test('a colour, a pin and the trash survive a relaunch', () async {
        // Across the bridge and onto the disk: the account document is a real
        // document, so what it holds has to come back the way a note's items do.
        final dir = await Directory.systemTemp.createTemp(
          'easynotes-placement',
        );
        addTearDown(() => dir.delete(recursive: true));
        final key = MasterKeyStore.newKey();

        final before = await RustNoteLibrary.open(dir.path, key);
        final coloured = await before.createNote();
        final thrownAway = await before.createNote();
        await before.setColour(coloured, NoteColour.teal);
        await before.setPinned(coloured, true);
        await before.setTrashed(thrownAway, true);
        await before.setLayout(NoteLayout.list);
        await before.dispose();

        final after = await RustNoteLibrary.open(dir.path, key);
        addTearDown(after.dispose);
        expect(after.layout, NoteLayout.list);
        final notes = {for (final note in after.notes) note.id: note};
        expect(notes[coloured]!.colour, NoteColour.teal);
        expect(notes[coloured]!.pinned, isTrue);
        expect(notes[thrownAway]!.trashed, isTrue);
        expect(notes[thrownAway]!.inGrid, isFalse);
        expect(
          after.notes.first.id,
          coloured,
          reason: 'a pinned note belongs at the top',
        );
      });

      test('a note dropped on another lands beside it, and stays', () async {
        // What the grid's drag reports, over the real order keys. It names the
        // note it landed on rather than a position, because the screen it
        // landed on is filtered and the order is over every note this device
        // has — and it has to be the same order after a relaunch.
        final dir = await Directory.systemTemp.createTemp('easynotes-move');
        addTearDown(() => dir.delete(recursive: true));
        final key = MasterKeyStore.newKey();

        final before = await RustNoteLibrary.open(dir.path, key);
        final a = await before.createNote();
        final b = await before.createNote();
        final c = await before.createNote();
        // Newest first, so the grid reads c, b, a.
        expect(before.notes.map((note) => note.id), [c, b, a]);

        // Dragged downwards: c lands after the card it was dropped on.
        await before.moveNoteTo(c, b);
        expect(before.notes.map((note) => note.id), [b, c, a]);

        // And dragged upwards, before it.
        await before.moveNoteTo(a, b);
        expect(before.notes.map((note) => note.id), [a, b, c]);
        await before.dispose();

        final after = await RustNoteLibrary.open(dir.path, key);
        addTearDown(after.dispose);
        expect(after.notes.map((note) => note.id), [a, b, c]);
      });

      test('labels survive a relaunch, and a label is its notes', () async {
        // A label is a placement like a colour, so the same question: across the
        // bridge, into the account document, onto the disk, and back.
        final dir = await Directory.systemTemp.createTemp('easynotes-labels');
        addTearDown(() => dir.delete(recursive: true));
        final key = MasterKeyStore.newKey();

        final before = await RustNoteLibrary.open(dir.path, key);
        final shopping = await before.createNote();
        final chores = await before.createNote();
        // What comes back is the label, not the name: the name is the part
        // that can change.
        expect(await before.addTag(shopping, 'errands'), isNotNull);
        await before.addTag(shopping, 'weekly shop');
        await before.addTag(chores, 'errands');
        await before.dispose();

        final after = await RustNoteLibrary.open(dir.path, key);
        addTearDown(after.dispose);
        final notes = {for (final note in after.notes) note.id: note};
        expect(names(notes[shopping]!.tags), ['errands', 'weekly shop']);
        expect(names(notes[chores]!.tags), ['errands']);
        // A label is counted by the notes that carry it, and sorted for a menu.
        expect(counted(after.tags), [('errands', 2), ('weekly shop', 1)]);

        // Taking it off the last note leaves the label, carried by nobody. That
        // is what a label having an identity of its own buys, and it is why
        // renaming one is possible at all.
        final weekly = after.tags.firstWhere((t) => t.name == 'weekly shop');
        await after.removeTag(shopping, weekly.id);
        expect(counted(after.tags), [('errands', 2), ('weekly shop', 0)]);
      });

      test('a label can be renamed and every note keeps it', () async {
        final library = await openLibraryInTempDir();
        final one = await library.createNote();
        final two = await library.createNote();
        await library.addTag(one, 'Recipes');
        await library.addTag(two, 'Recipes');
        final recipes = library.tags.single;

        expect(await library.renameTag(recipes.id, 'Dinners'), recipes.id);

        expect(counted(library.tags), [('Dinners', 2)]);
        for (final note in library.notes) {
          expect(names(note.tags), ['Dinners']);
        }
      });

      test('renaming a label onto another merges the two', () async {
        final library = await openLibraryInTempDir();
        final one = await library.createNote();
        final two = await library.createNote();
        await library.addTag(one, 'shopping');
        await library.addTag(two, 'errands');
        final shopping = library.tags.firstWhere((t) => t.name == 'shopping');
        final errands = library.tags.firstWhere((t) => t.name == 'errands');

        // The survivor is the label that already had the name, not the one
        // being renamed — which the UI has to know, because it is what tells it
        // a merge happened rather than a rename.
        expect(await library.renameTag(shopping.id, 'Errands'), errands.id);
        expect(counted(library.tags), [('errands', 2)]);
      });

      test('a label survives a relaunch with the name it was given', () async {
        final dir = await Directory.systemTemp.createTemp('easynotes-rename');
        addTearDown(() => dir.delete(recursive: true));
        final key = MasterKeyStore.newKey();

        final before = await RustNoteLibrary.open(dir.path, key);
        final note = await before.createNote();
        await before.addTag(note, 'Recipes');
        await before.renameTag(before.tags.single.id, 'Dinners');
        await before.dispose();

        final after = await RustNoteLibrary.open(dir.path, key);
        addTearDown(after.dispose);
        expect(counted(after.tags), [('Dinners', 1)]);
        expect(names(after.notes.single.tags), ['Dinners']);
      });

      test('throwing a label away takes it off every note', () async {
        final library = await openLibraryInTempDir();
        final one = await library.createNote();
        final two = await library.createNote();
        await library.addTag(one, 'errands');
        await library.addTag(two, 'errands');
        await library.addTag(one, 'recipes');
        final errands = library.tags.firstWhere((t) => t.name == 'errands');

        await library.forgetTag(errands.id);

        expect(counted(library.tags), [('recipes', 1)]);
        // The notes are still there; they have just lost the label.
        expect(library.notes.length, 2);
      });

      test('a label can be made before any note carries it', () async {
        final library = await openLibraryInTempDir();
        final someday = await library.makeTag('Someday');
        expect(counted(library.tags), [('Someday', 0)]);

        // And it is the label a note gets when that name is typed.
        final note = await library.createNote();
        expect(await library.addTag(note, 'someday'), someday);
        expect(counted(library.tags), [('Someday', 1)]);
      });

      test(
        'a label typed in another casing is the label already there',
        () async {
          // Two chips that look the same would be two keys in the document, so the
          // core reuses the one this device has rather than making a second.
          final library = await openLibraryInTempDir();
          final first = await library.createNote();
          final second = await library.createNote();
          await library.addTag(first, 'Recipes');

          final recipes = library.tags.single;
          expect(await library.addTag(second, '  recipes '), recipes.id);
          expect(counted(library.tags), [('Recipes', 2)]);

          // And a name that is not a name reaches nothing.
          expect(await library.addTag(second, '   '), isNull);
          expect(counted(library.tags), [('Recipes', 2)]);
        },
      );

      test('a label is cut at the length the field stops at', () async {
        // `maxLabelLength` is written in Dart and in Rust, because a `maxLength`
        // has to be a constant. This is what holds the two numbers together.
        final library = await openLibraryInTempDir();
        final id = await library.createNote();
        await library.addTag(id, 'x' * (maxLabelLength + 10));
        expect(library.tags.single.name.length, maxLabelLength);
      });

      test('a note deleted for good takes its labels with it', () async {
        final library = await openLibraryInTempDir();
        final gone = await library.createNote();
        final kept = await library.createNote();
        await library.addTag(gone, 'errands');
        await library.addTag(gone, 'holiday');
        await library.addTag(kept, 'errands');

        await library.forgetNote(gone);

        // The shared label is down to one note; the one only that note had is
        // carried by nobody, and is still a label — forgetting a note does not
        // throw away the labels it happened to be the last holder of.
        expect(counted(library.tags), [('errands', 1), ('holiday', 0)]);
        expect(names(library.notes.single.tags), ['errands']);
      });

      test('a note deleted for good is gone after a relaunch', () async {
        // The whole of emptying the trash, from the app's own API and onto a real
        // disk: the note has to be gone from the list, and still gone when the
        // app is opened again — which is the part a store that only forgot in
        // memory would pass everything else and fail here.
        final dir = await Directory.systemTemp.createTemp('easynotes-forget');
        addTearDown(() => dir.delete(recursive: true));
        final key = MasterKeyStore.newKey();

        final before = await RustNoteLibrary.open(dir.path, key);
        final kept = await before.openNote(await before.createNote());
        await kept.setTitle('Groceries');
        await kept.dispose();
        final gone = await before.createNote();
        final goneNote = await before.openNote(gone);
        await goneNote.setTitle('Tax papers');
        await goneNote.addItem('shred this');
        await goneNote.dispose();

        await before.setTrashed(gone, true);
        expect(await before.emptyTrash(), 1);
        expect(before.notes.map((note) => note.title), ['Groceries']);
        await before.dispose();

        final after = await RustNoteLibrary.open(dir.path, key);
        addTearDown(after.dispose);
        expect(after.notes.map((note) => note.title), ['Groceries']);
        // Not findable either. The index entry held the note's whole text, so a
        // delete that left it behind would leave the note searchable with no
        // card to open.
        expect(await after.search('shred'), isEmpty);
        expect(await after.search('milk'), isEmpty);
        // And the trash is empty rather than holding a note nothing can draw.
        expect(await after.emptyTrash(), 0);
      });

      test(
        'deleting one note for good leaves the rest of the device alone',
        () async {
          final library = await openLibraryInTempDir();
          final kept = await library.createNote();
          final keptNote = await library.openNote(kept);
          await keptNote.setTitle('Groceries');
          await keptNote.addItem('milk');
          final gone = await library.createNote();

          await library.setColour(kept, NoteColour.teal);
          await library.setPinned(kept, true);
          await library.forgetNote(gone);

          // The note beside it keeps its content and its placement: forgetting one
          // note rewrites the account document, and a rewrite that dropped the
          // wrong keys would show up here.
          final notes = {for (final note in library.notes) note.id: note};
          expect(notes.keys, [kept]);
          expect(notes[kept]!.colour, NoteColour.teal);
          expect(notes[kept]!.pinned, isTrue);
          expect(notes[kept]!.items, 1);
          await keptNote.dispose();
        },
      );

      test('a selection is changed and deleted across the bridge', () async {
        final library = await openLibraryInTempDir();
        final a = await library.createNote();
        final b = await library.createNote();
        final kept = await library.createNote();
        NoteSummary of(String id) =>
            library.notes.firstWhere((note) => note.id == id);

        await library.setPinned(a, true);
        await library.setColourMany([a, b], NoteColour.red);
        final label = await library.addTagMany([a, b], ' Errands ');
        expect(label, isNotNull);
        expect(library.tags.single.name, 'Errands');
        expect(library.tags.single.notes, 2);

        await library.setArchivedMany([a, b], true);
        for (final id in [a, b]) {
          expect(of(id).colour, NoteColour.red);
          expect(of(id).archived, isTrue);
          expect(of(id).pinned, isFalse, reason: 'archiving unpins');
        }
        await library.setArchivedMany([a, b], false);
        await library.setPinnedMany([a], true);
        expect(of(a).pinned, isTrue);
        await library.removeTagMany([a, b], label!);
        expect(library.tags.single.notes, 0);

        await library.setTrashedMany([a, b], true);
        expect(of(a).trashed && of(b).trashed, isTrue);
        expect(await library.forgetNotes([a, b]), 2);
        expect(library.notes.map((note) => note.id), [kept]);
      });

      test('nothing readable is written to disk', () async {
        final dir = await Directory.systemTemp.createTemp('easynotes-secrets');
        addTearDown(() => dir.delete(recursive: true));

        final library = await openLibraryInTempDir(dir);
        final core = await library.openNote(await library.createNote());
        addTearDown(core.dispose);
        await core.setTitle('Groceries');
        await core.addItem('asparagus');

        for (final file in dir.listSync(recursive: true).whereType<File>()) {
          final bytes = file.readAsBytesSync();
          for (final secret in ['Groceries', 'asparagus']) {
            expect(
              String.fromCharCodes(bytes).contains(secret),
              isFalse,
              reason: '$secret is in plaintext in ${file.path}',
            );
          }
        }
      });
    },
    skip: libPath == null
        ? 'set EASYNOTES_NATIVE_LIB to the built cdylib to run these'
        : null,
  );

  group(
    'the store a browser gets',
    () {
      // Everything a tab does to keep its notes, driven over the real bridge on
      // a machine with no browser on it. `MemoryBrowserStore` is a map, which is
      // all `IndexedDbStore` is once the requests and events are taken off it —
      // and the point of putting the key layout, the sealing and the batching in
      // the Rust core is that a map is enough to hold them to what they promise.
      //
      // What is *not* covered here is IndexedDB itself: whether a browser
      // applies a batch in one transaction, in the order it was handed them,
      // and how it says no. That needs a browser: `just web-smoke` and
      // `just web-storage-failure`.

      /// Open a library over [db], as a tab would, and tear it down.
      Future<RustNoteLibrary> openTab(
        BrowserStore db,
        Uint8List masterKey,
      ) async {
        final library = await RustNoteLibrary.openInBrowser(db, masterKey);
        addTearDown(library.dispose);
        return library;
      }

      test('a note survives the tab being closed', () async {
        final db = MemoryBrowserStore();
        final key = MasterKeyStore.newKey();

        final first = await openTab(db, key);
        final id = await first.createNote();
        final note = await first.openNote(id);
        await note.setTitle('Groceries');
        await note.addItem('asparagus');
        await note.dispose();
        // Closing the tab, which is the last chance those writes have.
        await first.dispose();

        final second = await openTab(db, key);
        expect(second.notes.single.title, 'Groceries');
        final reopened = await second.openNote(id);
        addTearDown(reopened.dispose);
        expect(reopened.current.items.map((i) => i.text), ['asparagus']);
      });

      test('the device keeps its identity across a tab', () async {
        // A tab that minted a new identity every launch would look like a
        // forger to every peer it already shares a list with, and its own log
        // would fail its own replay check.
        final db = MemoryBrowserStore();
        final key = MasterKeyStore.newKey();

        final first = await openTab(db, key);
        final id = await first.createNote();
        final note = await first.openNote(id);
        await note.addItem('milk');
        await note.dispose();
        await first.dispose();

        // The second tab appends to the same log under the same identity, and
        // the note opens — which it would not if the author or the counter had
        // restarted.
        final second = await openTab(db, key);
        final reopened = await second.openNote(id);
        addTearDown(reopened.dispose);
        await reopened.addItem('bread');
        await second.flush();

        final third = await openTab(db, key);
        final again = await third.openNote(id);
        addTearDown(again.dispose);
        expect(again.current.items.map((i) => i.text), ['milk', 'bread']);
      });

      test('a note thrown away for good leaves no row behind', () async {
        final db = MemoryBrowserStore();
        final key = MasterKeyStore.newKey();

        final library = await openTab(db, key);
        final id = await library.createNote();
        final note = await library.openNote(id);
        await note.setTitle('Groceries');
        await note.addItem('asparagus');
        await note.dispose();
        await library.setTrashed(id, true);
        await library.forgetNote(id);
        await library.flush();

        for (final area in StorageArea.values) {
          for (final row in await db.rows(area)) {
            expect(
              row.key.contains(id),
              isFalse,
              reason: '${row.key} is still there after the note was forgotten',
            );
          }
        }
        expect(library.notes, isEmpty);
      });

      test('nothing a browser holds is plaintext', () async {
        final db = MemoryBrowserStore();
        final library = await openTab(db, MasterKeyStore.newKey());
        final note = await library.openNote(await library.createNote());
        addTearDown(note.dispose);
        await note.setTitle('Groceries');
        await note.addItem('asparagus');
        await library.flush();

        for (final area in StorageArea.values) {
          for (final row in await db.rows(area)) {
            final raw = String.fromCharCodes(row.key.codeUnits + row.value);
            for (final secret in ['Groceries', 'asparagus']) {
              expect(raw.contains(secret), isFalse, reason: row.key);
            }
          }
        }
      });

      test('a write the browser refuses is not kept quiet', () async {
        // The one case where a person is looking at a note that is not being
        // kept. The core hands each batch over exactly once and keeps no copy,
        // so a refusal here is an edit that is gone — and the only place it
        // exists at all is the failure this asserts on.
        final db = _RefusingStore(MemoryBrowserStore());
        final library = await openTab(db, MasterKeyStore.newKey());
        expect(library.storageError, isNull);

        // Whatever this library says next, in order, so that the value the UI
        // starts with and the value it is pushed are both covered.
        final seen = library.watchStorageError().take(2).toList();

        db.refuse = true;
        final note = await library.openNote(await library.createNote());
        addTearDown(note.dispose);
        await note.addItem('asparagus');
        await library.flush();

        expect(library.storageError, contains('out of space'));
        expect(await seen, [null, library.storageError]);

        // And it stays: a device that has started refusing writes has not got
        // the edit back by refusing the next one differently.
        db.message = 'the browser abandoned the write';
        await note.addItem('bread');
        await library.flush();
        expect(library.storageError, contains('out of space'));
      });

      test('nothing is written after a write the browser refused', () async {
        // A browser that refuses one batch and takes the next would leave a
        // hole: a later write on top of an earlier one that is gone. What is
        // kept has to be a prefix of what the core wrote, because that is the
        // only crash the core knows how to come back from.
        final db = _RefusingStore(MemoryBrowserStore());
        final library = await openTab(db, MasterKeyStore.newKey());
        final id = await library.createNote();
        final note = await library.openNote(id);
        addTearDown(note.dispose);
        await library.flush();

        db
          ..refuse = true
          ..once = true;
        await note.addItem('asparagus');
        await library.flush();
        expect(db.refused, isTrue);
        await note.addItem('bread');
        await library.setTrashed(id, true);
        await library.flush();

        expect(library.storageError, contains('out of space'));
        expect(db.keptSinceRefusal, 0);
      });

      /// A tab over [db] with [notes] notes in its trash and one note that is
      /// not, each with an item, all of it written. Returns the trashed IDs and
      /// the kept one.
      Future<(RustNoteLibrary, List<String>, String)> aTrashOf(
        BrowserStore db,
        Uint8List key,
        int notes,
      ) async {
        final library = await openTab(db, key);
        Future<String> make(String title) async {
          final id = await library.createNote();
          final note = await library.openNote(id);
          await note.setTitle(title);
          await note.addItem('milk');
          await note.dispose();
          return id;
        }

        final thrown = [for (var i = 0; i < notes; i++) await make('Old $i')];
        final kept = await make('Kept');
        for (final id in thrown) {
          await library.setTrashed(id, true);
        }
        await library.flush();
        return (library, thrown, kept);
      }

      /// No row of either area names [id].
      Future<void> noRowsOf(BrowserStore db, String id) async {
        for (final area in StorageArea.values) {
          for (final row in await db.rows(area)) {
            expect(row.key.contains(id), isFalse, reason: row.key);
          }
        }
      }

      /// A reopened tab holds [id] whole and in the trash, or holds nothing
      /// of it at all. Returns whether it is still there.
      Future<bool> wholeOrGone(
        RustNoteLibrary tab,
        BrowserStore db,
        String id,
      ) async {
        final summary = tab.notes.where((n) => n.id == id).firstOrNull;
        if (summary == null) {
          await tab.flush();
          await noRowsOf(db, id);
          return false;
        }
        expect(summary.trashed, isTrue, reason: 'still in the trash');
        final note = await tab.openNote(id);
        expect(note.current.items.map((i) => i.text), ['milk']);
        await note.dispose();
        return true;
      }

      test(
        'a deletion the browser refuses anywhere never happened or finishes',
        () async {
          // The web half of the core's crash test: the tab stops being able to
          // write at every batch a deletion makes, and is opened again over
          // what the browser kept. Either the deletion was never accepted, and
          // the note is all there, or its marker was kept and the next launch
          // finishes it. Never a note half gone.
          for (var batches = 0; ; batches++) {
            final db = _RefusingStore(MemoryBrowserStore());
            final key = MasterKeyStore.newKey();
            final (tab, thrown, _) = await aTrashOf(db, key, 1);
            final id = thrown.single;

            db.refuseAfter(batches);
            Object? failed;
            try {
              await tab.forgetNote(id);
            } catch (e) {
              failed = e;
            }
            await tab.dispose();
            db.refuseAfter(1 << 30);
            db.refuse = false;

            final again = await openTab(db, key);
            final there = await wholeOrGone(again, db, id);
            if (failed == null) {
              expect(there, isFalse, reason: 'batches=$batches');
              break;
            }
            expect(db.refused, isTrue);
            // And whatever was left, deleting it again finishes.
            if (there) await again.forgetNote(id);
            await again.flush();
            await noRowsOf(db, id);
            expect(again.notes.map((n) => n.id), isNot(contains(id)));
            expect(batches, lessThan(50));
          }
        },
      );

      test(
        'emptying a trash the browser refuses partway loses nothing',
        () async {
          // Each note in the trash is its own marker. A refusal partway leaves
          // the notes already marked gone and the rest still whole in the
          // trash, and emptying it again takes exactly those. The note that
          // was never in the trash is never touched.
          for (var batches = 0; ; batches++) {
            final db = _RefusingStore(MemoryBrowserStore());
            final key = MasterKeyStore.newKey();
            final (tab, thrown, kept) = await aTrashOf(db, key, 3);

            db.refuseAfter(batches);
            Object? failed;
            try {
              expect(await tab.emptyTrash(), 3);
            } catch (e) {
              failed = e;
            }
            await tab.dispose();
            db.refuseAfter(1 << 30);
            db.refuse = false;

            final again = await openTab(db, key);
            var left = 0;
            for (final id in thrown) {
              if (await wholeOrGone(again, db, id)) left++;
            }
            if (failed == null) expect(left, 0, reason: 'batches=$batches');
            expect(await again.emptyTrash(), left, reason: 'batches=$batches');
            for (final id in thrown) {
              await noRowsOf(db, id);
            }
            final survivor = again.notes.singleWhere((n) => n.id == kept);
            expect(survivor.trashed, isFalse);
            final note = await again.openNote(kept);
            expect(note.current.items.map((i) => i.text), ['milk']);
            await note.dispose();
            if (failed == null) break;
            expect(batches, lessThan(100));
          }
        },
      );

      test('the wrong key does not quietly open an empty tab', () async {
        final db = MemoryBrowserStore();
        final library = await openTab(db, MasterKeyStore.newKey());
        await library.createNote();
        await library.dispose();

        await expectLater(
          RustNoteLibrary.openInBrowser(db, MasterKeyStore.newKey()),
          throwsA(anything),
        );
      });
    },
    skip: libPath == null
        ? 'set EASYNOTES_NATIVE_LIB to the built cdylib to run these'
        : null,
  );
}

/// A [BrowserStore] that can be told to start refusing writes.
///
/// What a browser out of quota, or one whose database has been cleared under
/// the tab, looks like from here: reads still work, and `apply` throws. It
/// wraps a real map rather than replacing one so that everything before the
/// refusal is stored exactly as it would have been — which is also what makes
/// it a crash: open another tab over the same map and it reads what was kept.
class _RefusingStore implements BrowserStore {
  _RefusingStore(this._inner);

  final MemoryBrowserStore _inner;
  bool refuse = false;
  String message = 'out of space';

  /// Accept this many more batches, then refuse; null means no countdown.
  int? _left;

  /// Refuse only the next refusal, and accept whatever comes after it, as a
  /// browser that recovered would.
  bool once = false;

  /// Batches kept since the last refusal.
  int keptSinceRefusal = 0;

  /// Accept [batches] more, and refuse every one after that.
  void refuseAfter(int batches) => _left = batches;

  bool get refused => _refused;
  bool _refused = false;

  @override
  Future<List<StorageRow>> rows(StorageArea area) => _inner.rows(area);

  @override
  String get mediaDatabase => _inner.mediaDatabase;

  @override
  Future<void> apply(StorageBatch batch) async {
    final left = _left;
    if (left != null) {
      if (left == 0) refuse = true;
      _left = left - 1;
    }
    if (refuse) {
      _refused = true;
      keptSinceRefusal = 0;
      if (once) refuse = false;
      throw StateError(message);
    }
    await _inner.apply(batch);
    keptSinceRefusal++;
  }

  @override
  Future<void> close() => _inner.close();
}
