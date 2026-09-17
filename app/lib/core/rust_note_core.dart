/// [NoteCore] backed by the real Rust core through flutter_rust_bridge.
///
/// This class is the whole of the bridge's surface in the app. The screen talks
/// to [NoteCore]; nothing above this file imports anything generated, so
/// regenerating the bindings cannot ripple into the UI.
library;

import 'dart:async';

import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';

import '../src/rust/api/notes.dart' as rust;
import '../src/rust/frb_generated.dart';
import 'note_core.dart';
import 'note_view.dart';

class RustNoteCore implements NoteCore {
  RustNoteCore._(this._note) {
    // Exactly one Rust watcher per note, fanned out to however many widgets
    // want it.
    //
    // Handing the generated stream straight to the UI would be simpler and
    // wrong: a Rust watcher blocks on the view channel, so cancelling a
    // subscription waits for the Rust side to notice, which it only does on the
    // next edit. A widget disposing would hang. The broadcast controller keeps
    // subscribe and unsubscribe on the Dart side, where they are instant, and
    // [dispose] is the one place that has to talk to Rust about it.
    _upstream = _note.watch().listen(
      (view) => _controller.add(_toView(view)),
      onError: _controller.addError,
    );
  }

  static Future<void>? _initialized;

  /// Load the native library. Safe to call repeatedly: the bridge refuses a
  /// second initialisation, and opening a second note must not bring the app
  /// down.
  ///
  /// [library] is for tests, which load the cdylib from the Cargo target
  /// directory rather than from wherever the bundled app would put it.
  static Future<void> ensureInitialized({ExternalLibrary? library}) =>
      _initialized ??= RustLib.init(externalLibrary: library);

  /// Open the note stored under [directory], creating it on first run.
  static Future<RustNoteCore> open(String directory) async {
    await ensureInitialized();
    return RustNoteCore._(rust.Note.open(dir: directory));
  }

  /// Open a note that lives only in memory.
  ///
  /// The web build uses this: a browser has no filesystem, and backing the op
  /// log with IndexedDB is the part of M4 this slice did not finish. Everything
  /// above the store — crypto, CRDT, the watch stream — is the same Rust the
  /// native build runs, so this is a stepping stone and not a mock. See
  /// `docs/adr/0002-web-core.md`.
  static Future<RustNoteCore> openEphemeral() async {
    await ensureInitialized();
    return RustNoteCore._(rust.Note.openEphemeral());
  }

  final rust.Note _note;
  final _controller = StreamController<NoteView>.broadcast();
  late final StreamSubscription<rust.NoteView> _upstream;

  /// Read straight through to Rust rather than caching. A cached copy is only
  /// fresh while something is listening to [watch], and a caller that reads
  /// `current` without subscribing would silently get stale data.
  @override
  NoteView get current => _toView(_note.view());

  @override
  Stream<NoteView> watch() async* {
    yield current;
    yield* _controller.stream;
  }

  @override
  Future<void> setTitle(String title) => _note.setTitle(title: title);

  @override
  Future<String> addItem(String text) => _note.addItem(text: text);

  @override
  Future<void> setItemText(String itemId, String text) =>
      _note.setItemText(item: itemId, text: text);

  @override
  Future<void> setChecked(String itemId, bool checked) =>
      _note.setChecked(item: itemId, checked: checked);

  @override
  Future<void> moveItem(String itemId, int toIndex) =>
      _note.moveItem(item: itemId, toIndex: toIndex);

  @override
  Future<void> removeItem(String itemId) => _note.removeItem(item: itemId);

  @override
  Future<String> exportShareBlob() async => _note.shareBlob();

  @override
  Future<void> dispose() async {
    // Stop the Rust watcher first: cancelling the upstream subscription waits
    // for that stream to close, and it will not close on its own.
    await _note.stopWatching();
    await _upstream.cancel();
    await _controller.close();
  }

  static NoteView _toView(rust.NoteView v) => NoteView(
        title: v.title,
        items: [
          for (final i in v.items)
            ItemView(id: i.id, text: i.text, checked: i.checked, order: i.order),
        ],
      );
}
