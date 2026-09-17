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

import 'package:easynotes/core/note_core.dart';
import 'package:easynotes/core/rust_note_core.dart';
import 'package:easynotes/core/rust_note_library.dart';
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
  Future<RustNoteLibrary> openLibraryInTempDir([Directory? existing]) async {
    final dir =
        existing ?? await Directory.systemTemp.createTemp('easynotes-test');
    final library = await RustNoteLibrary.open(dir.path);
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

        final first = await RustNoteLibrary.open(dir.path);
        final id = await first.createNote();
        final note = first.openNote(id);
        await note.setTitle('Groceries');
        for (final text in ['milk', 'bread', 'eggs']) {
          await note.addItem(text);
        }
        await note.dispose();
        await first.dispose();

        final second = await openLibraryInTempDir(dir);
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
}
