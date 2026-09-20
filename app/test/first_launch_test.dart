/// The very first launch on a device that has never held a note.
///
/// Its own file because that is the only way to get a cold bridge: `flutter
/// test` gives each file its own isolate, and every other test group here
/// initialises the bridge in `setUpAll`, which is exactly what hid the bug this
/// covers.
///
/// What went wrong on a real Android device: `openLibrary` read the master key
/// before opening a library, [MasterKeyStore.forDirectory] crossed the bridge to
/// look for a Slice 1 `master.key` file, and the bridge was not up yet — so the
/// app died on launch with `flutter_rust_bridge has not been initialized`. Two
/// things kept it hidden. The keychain read returns early once a key exists, so
/// only a *first* launch reaches the crossing and any relaunch looks healthy.
/// And no other platform runs the line: a browser has no file to adopt, and the
/// desktop app is only ever driven by tests that hand the core a key of their
/// own.
///
/// Skipped unless `EASYNOTES_NATIVE_LIB` points at the built cdylib, like the
/// other tests that need the real core.
library;

import 'dart:io';

import 'package:easynotes/core/master_key.dart';
import 'package:easynotes/core/rust_note_core.dart';
import 'package:easynotes/core/rust_note_library.dart';
import 'package:easynotes/core/sync_status.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  final libPath = Platform.environment['EASYNOTES_NATIVE_LIB'];

  setUpAll(() {
    if (libPath == null) return;
    // Named, but deliberately *not* initialised: the point is that the code
    // under test brings the bridge up on its own. Initialising here would pass
    // whether or not it does.
    RustNoteCore.defaultLibrary = ExternalLibrary.open(libPath);
  });

  group(
    'the first launch on a fresh device',
    () {
      test('reads the master key before anything has opened the bridge', () async {
        FlutterSecureStorage.setMockInitialValues({});
        final dir = await Directory.systemTemp.createTemp('easynotes-first');
        addTearDown(() => dir.delete(recursive: true));

        // The first bridge crossing in this isolate, and the app's first call on
        // a device with an empty keychain. It threw before the fix.
        final key = await MasterKeyStore().forDirectory(dir.path);
        expect(key, hasLength(masterKeyBytes));

        // And the key it minted actually opens a library, so this is the whole
        // of a first launch rather than a call that merely returned.
        final library = await RustNoteLibrary.open(dir.path, key);
        addTearDown(library.dispose);
        expect(library.notes, isEmpty);
      });

      test('an empty device is not a device that cannot reach the relay', () async {
        FlutterSecureStorage.setMockInitialValues({});
        final dir = await Directory.systemTemp.createTemp('easynotes-empty');
        addTearDown(() => dir.delete(recursive: true));

        final library = await RustNoteLibrary.open(
          dir.path,
          await MasterKeyStore().forDirectory(dir.path),
        );
        addTearDown(library.dispose);

        // The first screen anyone sees. `offline` here would read as "Not
        // syncing" — a fault report from a device that has nothing to report.
        expect(library.syncStatus.state, SyncState.empty);
        expect(library.syncStatus.pending, 0);
      });
    },
    skip: libPath == null
        ? 'set EASYNOTES_NATIVE_LIB to the built cdylib to run these'
        : null,
  );
}
