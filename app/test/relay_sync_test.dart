/// Demo steps 1–3 from the app's own API, through a real relay.
///
/// `relay/tests/engine.rs` proves the sync engine in Rust, and the widget tests
/// prove the screen against a fake. This is the seam between them: two cores
/// opened the way the app opens them, connected the way the app connects them,
/// talking to a relay process over a real socket. It is what Slice 1's third gap
/// asked for — the demo run from the app's side of the bridge rather than only
/// headlessly.
///
/// Skipped unless `EASYNOTES_NATIVE_LIB` and `EASYNOTES_RELAY_BIN` are set.
/// `just test-app` and CI set both.
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:easynotes/core/note_core.dart';
import 'package:easynotes/core/note_view.dart';
import 'package:easynotes/core/rust_note_core.dart';
import 'package:easynotes/core/rust_note_library.dart';
import 'package:easynotes/core/sync_status.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:flutter_test/flutter_test.dart';

/// Long enough that a loaded machine is slow rather than red.
const patience = Duration(seconds: 20);

void main() {
  final libPath = Platform.environment['EASYNOTES_NATIVE_LIB'];
  final relayPath = Platform.environment['EASYNOTES_RELAY_BIN'];
  final skip = libPath == null || relayPath == null
      ? 'set EASYNOTES_NATIVE_LIB and EASYNOTES_RELAY_BIN to run these'
      : null;

  Process? relay;
  late String relayUrl;

  setUpAll(() async {
    if (skip != null) return;
    await RustNoteCore.ensureInitialized(
      library: ExternalLibrary.open(libPath!),
    );
    // Port 0: the OS picks, the relay logs what it got, and tests can run in
    // parallel without fighting over a port.
    final started = await Process.start(
      relayPath!,
      const [],
      environment: {
        'RELAY_ADDR': '127.0.0.1:0',
        'RUST_LOG': 'easynotes_relay=info',
        'NO_COLOR': '1',
      },
    );
    relay = started;
    relayUrl = 'http://${await _listeningAddress(started)}';
  });

  tearDownAll(() {
    // Not guarded by `skip`: if `setUpAll` failed after starting the relay, this
    // is the only thing that will stop it.
    relay?.kill();
  });

  /// A device: its own directory, its own library, connected to the relay the
  /// way the app connects one.
  Future<RustNoteLibrary> device() async {
    final dir = await Directory.systemTemp.createTemp('easynotes-relay-test');
    final library = await RustNoteLibrary.open(dir.path);
    addTearDown(() async {
      await library.dispose();
      await dir.delete(recursive: true);
    });
    await library.connect(relayUrl);
    return library;
  }

  /// A device with one new note on it, which is what most of these want.
  Future<(RustNoteLibrary, NoteCore)> open() async {
    final library = await device();
    final core = library.openNote(await library.createNote());
    addTearDown(core.dispose);
    return (library, core);
  }

  Future<(RustNoteLibrary, NoteCore)> join(String blob) async {
    final library = await device();
    final core = library.openNote(await library.join(blob));
    addTearDown(core.dispose);
    return (library, core);
  }

  group('two devices through a relay', () {
    test(
      'a device that joins sees the list, and edits flow both ways',
      () async {
        final (_, a) = await open();
        await a.setTitle('Groceries');
        for (final item in ['milk', 'bread', 'eggs']) {
          await a.addItem(item);
        }

        // Demo step 2: B joins from the share blob, having never been connected
        // when the edits were made.
        final (_, b) = await join(await a.exportShareBlob());
        await _until(
          b.watch(),
          (view) =>
              view.items.map((i) => i.text).toList().join(',') ==
              'milk,bread,eggs',
          'B never received A\'s list',
        );
        expect(b.current.title, 'Groceries');

        // Demo step 3: B checks an item and A sees it, with nobody polling.
        final bread = b.current.items.firstWhere((i) => i.text == 'bread');
        await b.setChecked(bread.id, true);
        await _until(
          a.watch(),
          (view) => view.items.any((i) => i.text == 'bread' && i.checked),
          'A never saw B check an item',
        );
      },
    );

    test(
      'the app reports itself synced once the relay has everything',
      () async {
        final (_, a) = await open();
        await a.addItem('milk');
        await _untilStatus(
          a.watchSync(),
          () => a.syncStatus,
          (status) => status.state == SyncState.synced && status.pending == 0,
          'A never reported itself synced',
        );
        expect(a.syncStatus.lastError, isNull);
      },
    );

    test('an unreachable relay leaves the app working and says so', () async {
      final (library, a) = await open();
      // Nothing listens on this port; the app must carry on regardless.
      await library.connect('http://127.0.0.1:1');
      await a.addItem('milk');

      await _untilStatus(
        a.watchSync(),
        () => a.syncStatus,
        (status) => status.state == SyncState.offline && status.pending > 0,
        'A never noticed it could not reach the relay',
      );
      expect(a.current.items.single.text, 'milk');
      expect(a.syncStatus.lastError, isNotNull);
    });

    test('a device syncs every note it holds, not just the open one', () async {
      // The note list's half of sync: the library connects each note, including
      // one made after it connected, and its one status button is the sum of
      // them.
      final (library, groceries) = await open();
      await groceries.setTitle('Groceries');
      final hardware = library.openNote(await library.createNote());
      addTearDown(hardware.dispose);
      await hardware.addItem('screws');

      await _untilStatus(
        library.watchSync(),
        () => library.syncStatus,
        (status) => status.state == SyncState.synced && status.pending == 0,
        'the device never reported every note synced',
      );

      // Both notes really are on the relay: a second device joining each one
      // gets its content, and the first knows nothing about the second.
      final (_, joinedHardware) = await join(await hardware.exportShareBlob());
      await _until(
        joinedHardware.watch(),
        (view) => view.items.any((i) => i.text == 'screws'),
        'the second note never reached the relay',
      );
      expect(joinedHardware.current.title, isEmpty);
    });
  }, skip: skip);
}

/// The address the relay logged. Reading it beats guessing a port, and lets the
/// OS pick one so tests never collide.
Future<String> _listeningAddress(Process relay) async {
  final line = await relay.stdout
      .transform(utf8.decoder)
      .transform(const LineSplitter())
      // The log is decorated for a terminal; strip that before matching.
      .map((l) => l.replaceAll(RegExp(r'\x1B\[[0-9;]*m'), ''))
      .firstWhere((l) => l.contains('relay listening'))
      .timeout(patience);
  final addr = RegExp(r'addr=([\d.]+:\d+)').firstMatch(line);
  if (addr == null) {
    throw StateError('could not read the relay address from: $line');
  }
  return addr.group(1)!;
}

Future<void> _until(
  Stream<NoteView> views,
  bool Function(NoteView) ready,
  String complaint,
) async {
  try {
    await views.firstWhere(ready).timeout(patience);
  } on TimeoutException {
    fail(complaint);
  }
}

Future<void> _untilStatus(
  Stream<SyncStatus> statuses,
  SyncStatus Function() current,
  bool Function(SyncStatus) ready,
  String complaint,
) async {
  try {
    await statuses.firstWhere(ready).timeout(patience);
  } on TimeoutException {
    fail('$complaint (last status: ${current()})');
  }
}
