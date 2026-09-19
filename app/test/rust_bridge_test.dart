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
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:easynotes/src/rust/api/notes.dart'
    show StorageArea, StorageBatch, StorageRow;
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:flutter_test/flutter_test.dart';

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
    final core = library.openNote(await library.createNote());
    addTearDown(core.dispose);
    return core;
  }

  group(
    'the Rust core across the bridge',
    () {
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

      test('moving an item lands it where the UI asked', () async {
        final core = await openInTempDir();
        await core.addItem('one');
        await core.addItem('two');
        final three = await core.addItem('three');

        await core.moveItem(three, 0);
        expect(core.current.items.map((i) => i.text), ['three', 'one', 'two']);
      });

      test('the share blob is short enough for a QR code', () async {
        final core = await openInTempDir();
        final blob = await core.exportShareBlob();
        expect(blob.length, lessThanOrEqualTo(120));
        expect(blob, matches(RegExp(r'^[A-Za-z0-9_-]+$')));
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
        final note = first.openNote(id);
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

        final reopened = second.openNote(id);
        addTearDown(reopened.dispose);
        expect(reopened.current.items.map((i) => i.text), [
          'milk',
          'bread',
          'eggs',
        ]);
      });

      test('several notes live side by side, newest first', () async {
        final library = await openLibraryInTempDir();
        final groceries = library.openNote(await library.createNote());
        addTearDown(groceries.dispose);
        await groceries.setTitle('Groceries');
        final hardware = library.openNote(await library.createNote());
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

        final note = library.openNote(await library.createNote());
        addTearDown(note.dispose);
        await note.setTitle('Groceries');

        await arrived;
      });

      test('joining adds a note rather than replacing one', () async {
        final owner = await openLibraryInTempDir();
        final shared = owner.openNote(await owner.createNote());
        addTearDown(shared.dispose);
        await shared.setTitle('Shared');
        final blob = await shared.exportShareBlob();

        final guest = await openLibraryInTempDir();
        final own = guest.openNote(await guest.createNote());
        addTearDown(own.dispose);
        await own.setTitle('Mine');
        final joined = await guest.join(blob);

        expect(guest.notes, hasLength(2));
        expect(guest.notes.map((n) => n.id), contains(joined));
        expect(guest.notes.map((n) => n.title), contains('Mine'));
      });

      // -- search ------------------------------------------------------------
      //
      // Item 7 of the Slice 2 plan, over the real index: the text is inside an
      // envelope inside the CRDT, so these are the only tests that prove the
      // whole path from a query typed on a screen to a note found on a disk.

      test('a search finds a note by something on it', () async {
        final library = await openLibraryInTempDir();
        final groceries = library.openNote(await library.createNote());
        addTearDown(groceries.dispose);
        await groceries.setTitle('Groceries');
        for (final item in ['milk', 'bread', 'eggs', 'coffee', 'asparagus']) {
          await groceries.addItem(item);
        }
        final hardware = library.openNote(await library.createNote());
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
          final core = library.openNote(await library.createNote());
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
        final core = library.openNote(hits.single.note.id);
        addTearDown(core.dispose);
        expect(core.current.title, 'Groceries');
        expect(core.current.items.map((i) => i.text), ['asparagus']);
      });

      // -- the master key ----------------------------------------------------
      //
      // Item 3b of the Slice 2 plan: the key that seals everything local lives in
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
        final note = before.openNote(await before.createNote());
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
        final note = right.openNote(await right.createNote());
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

      test('labels survive a relaunch, and a label is its notes', () async {
        // A label is a placement like a colour, so the same question: across the
        // bridge, into the account document, onto the disk, and back.
        final dir = await Directory.systemTemp.createTemp('easynotes-labels');
        addTearDown(() => dir.delete(recursive: true));
        final key = MasterKeyStore.newKey();

        final before = await RustNoteLibrary.open(dir.path, key);
        final shopping = await before.createNote();
        final chores = await before.createNote();
        expect(await before.addTag(shopping, 'errands'), 'errands');
        await before.addTag(shopping, 'weekly shop');
        await before.addTag(chores, 'errands');
        await before.dispose();

        final after = await RustNoteLibrary.open(dir.path, key);
        addTearDown(after.dispose);
        final notes = {for (final note in after.notes) note.id: note};
        expect(notes[shopping]!.tags, ['errands', 'weekly shop']);
        expect(notes[chores]!.tags, ['errands']);
        // A label is counted by the notes that carry it, and sorted for a menu.
        expect(after.tags, const [
          Tag(name: 'errands', notes: 2),
          Tag(name: 'weekly shop', notes: 1),
        ]);

        // Taking it off the last note takes the label away: there is no list of
        // labels apart from the notes.
        await after.removeTag(shopping, 'weekly shop');
        expect(after.tags, const [Tag(name: 'errands', notes: 2)]);
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

          expect(await library.addTag(second, '  recipes '), 'Recipes');
          expect(library.tags, const [Tag(name: 'Recipes', notes: 2)]);

          // And a name that is not a name reaches nothing.
          expect(await library.addTag(second, '   '), isNull);
          expect(library.tags, const [Tag(name: 'Recipes', notes: 2)]);
        },
      );

      test('a label is cut at the length the field stops at', () async {
        // `maxLabelLength` is written in Dart and in Rust, because a `maxLength`
        // has to be a constant. This is what holds the two numbers together.
        final library = await openLibraryInTempDir();
        final id = await library.createNote();
        final stored = await library.addTag(id, 'x' * (maxLabelLength + 10));
        expect(stored!.length, maxLabelLength);
      });

      test('a label is not written into the note it is about', () async {
        // The same property a colour has, and the reason labels are in the
        // account document: a household sharing a list does not share one
        // person's filing.
        final library = await openLibraryInTempDir();
        final id = await library.createNote();
        final core = library.openNote(id);
        addTearDown(core.dispose);
        await core.setTitle('Groceries');
        final blob = await core.exportShareBlob();
        await library.addTag(id, 'errands');

        final guestDir = await Directory.systemTemp.createTemp(
          'easynotes-guest-labels',
        );
        addTearDown(() => guestDir.delete(recursive: true));
        final guest = await openLibraryInTempDir(guestDir);
        final joined = await guest.join(blob);
        expect(
          guest.notes.singleWhere((note) => note.id == joined).tags,
          isEmpty,
          reason: "one person's label reached another person's device",
        );
        expect(guest.tags, isEmpty);
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
        // gone, because a label alive with nothing under it would be replayed at
        // every launch for ever.
        expect(library.tags, const [Tag(name: 'errands', notes: 1)]);
        expect(library.notes.single.tags, ['errands']);
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
        final kept = before.openNote(await before.createNote());
        await kept.setTitle('Groceries');
        await kept.dispose();
        final gone = await before.createNote();
        final goneNote = before.openNote(gone);
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
          final keptNote = library.openNote(kept);
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

      test('a placement is not written into the note it is about', () async {
        // The property the account document exists for. A colour is this
        // person's, so it must not reach the note — which is the thing everyone
        // else on the list has a key to.
        final dir = await Directory.systemTemp.createTemp('easynotes-private');
        addTearDown(() => dir.delete(recursive: true));

        final library = await openLibraryInTempDir(dir);
        final id = await library.createNote();
        final core = library.openNote(id);
        addTearDown(core.dispose);
        await core.setTitle('Groceries');
        final blob = await core.exportShareBlob();
        await library.setColour(id, NoteColour.pink);

        // A guest joining with that blob gets the note's key and nothing else.
        // What it opens is the note's log, and the colour is not in it.
        final guestDir = await Directory.systemTemp.createTemp(
          'easynotes-guest',
        );
        addTearDown(() => guestDir.delete(recursive: true));
        final guest = await openLibraryInTempDir(guestDir);
        final joined = await guest.join(blob);
        expect(
          guest.notes.singleWhere((note) => note.id == joined).colour,
          isNull,
          reason: "one person's colour reached another person's device",
        );
      });

      test('nothing readable is written to disk', () async {
        final dir = await Directory.systemTemp.createTemp('easynotes-secrets');
        addTearDown(() => dir.delete(recursive: true));

        final library = await openLibraryInTempDir(dir);
        final core = library.openNote(await library.createNote());
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
      // applies a batch in one transaction, and in the order it was handed them.
      // That needs a browser, and `docs/plan-slice-02.md` says so.

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
        final note = first.openNote(id);
        await note.setTitle('Groceries');
        await note.addItem('asparagus');
        await note.dispose();
        // Closing the tab, which is the last chance those writes have.
        await first.dispose();

        final second = await openTab(db, key);
        expect(second.notes.single.title, 'Groceries');
        final reopened = second.openNote(id);
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
        final note = first.openNote(id);
        await note.addItem('milk');
        await note.dispose();
        await first.dispose();

        // The second tab appends to the same log under the same identity, and
        // the note opens — which it would not if the author or the counter had
        // restarted.
        final second = await openTab(db, key);
        final reopened = second.openNote(id);
        addTearDown(reopened.dispose);
        await reopened.addItem('bread');
        await second.flush();

        final third = await openTab(db, key);
        final again = third.openNote(id);
        addTearDown(again.dispose);
        expect(again.current.items.map((i) => i.text), ['milk', 'bread']);
      });

      test('a note thrown away for good leaves no row behind', () async {
        final db = MemoryBrowserStore();
        final key = MasterKeyStore.newKey();

        final library = await openTab(db, key);
        final id = await library.createNote();
        final note = library.openNote(id);
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
        final note = library.openNote(await library.createNote());
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
        final note = library.openNote(await library.createNote());
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
/// refusal is stored exactly as it would have been.
class _RefusingStore implements BrowserStore {
  _RefusingStore(this._inner);

  final MemoryBrowserStore _inner;
  bool refuse = false;
  String message = 'out of space';

  @override
  Future<List<StorageRow>> rows(StorageArea area) => _inner.rows(area);

  @override
  Future<void> apply(StorageBatch batch) async {
    if (refuse) throw StateError(message);
    await _inner.apply(batch);
  }

  @override
  Future<void> close() => _inner.close();
}
