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

import 'package:easynotes/core/note_view.dart';
import 'package:easynotes/core/rust_note_core.dart';
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

  Future<RustNoteCore> openInTempDir() async {
    final dir = await Directory.systemTemp.createTemp('easynotes-test');
    final core = await RustNoteCore.open(dir.path);
    addTearDown(() async {
      await core.dispose();
      await dir.delete(recursive: true);
    });
    return core;
  }

  group('the Rust core across the bridge', () {
    test('an empty note starts empty', () async {
      final core = await openInTempDir();
      expect(core.current.title, isEmpty);
      expect(core.current.items, isEmpty);
    });

    test('edits come back on the watch stream', () async {
      // The whole architecture rests on this: the UI never mutates state, it
      // waits to be told. If the stream does not fire, the app renders nothing.
      final core = await openInTempDir();
      final views = <NoteView>[];
      final subscription = core.watch().listen(views.add);
      addTearDown(subscription.cancel);

      await core.setTitle('Groceries');
      await core.addItem('milk');
      await core.addItem('bread');
      await pumpEventQueue();

      expect(views, isNotEmpty, reason: 'the watch stream never fired');
      final last = views.last;
      expect(last.title, 'Groceries');
      expect(last.items.map((i) => i.text), ['milk', 'bread']);
    });

    test('items keep the order they were added in', () async {
      final core = await openInTempDir();
      for (final text in ['milk', 'bread', 'eggs']) {
        await core.addItem(text);
      }
      expect(core.current.items.map((i) => i.text), ['milk', 'bread', 'eggs']);
    });

    test('checking, renaming and deleting an item all round-trip', () async {
      final core = await openInTempDir();
      final milk = await core.addItem('milk');
      final eggs = await core.addItem('eggs');

      await core.setChecked(milk, true);
      await core.setItemText(eggs, 'eggs (12)');
      expect(core.current.items.firstWhere((i) => i.id == milk).checked, isTrue);
      expect(core.current.items.firstWhere((i) => i.id == eggs).text, 'eggs (12)');

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
      // Demo step 6, through the bridge: same directory, new core, same list.
      final dir = await Directory.systemTemp.createTemp('easynotes-restart');
      addTearDown(() => dir.delete(recursive: true));

      final first = await RustNoteCore.open(dir.path);
      await first.setTitle('Groceries');
      for (final text in ['milk', 'bread', 'eggs']) {
        await first.addItem(text);
      }
      await first.dispose();

      final second = await RustNoteCore.open(dir.path);
      addTearDown(second.dispose);
      expect(second.current.title, 'Groceries');
      expect(second.current.items.map((i) => i.text), ['milk', 'bread', 'eggs']);
    });

    test('nothing readable is written to disk', () async {
      final dir = await Directory.systemTemp.createTemp('easynotes-secrets');
      addTearDown(() => dir.delete(recursive: true));

      final core = await RustNoteCore.open(dir.path);
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
          : null);
}
