/// [NoteCore] backed by the real Rust core through flutter_rust_bridge.
///
/// One note, handed out by [RustNoteLibrary]; the device and its relay are
/// there. The screen talks to [NoteCore]; nothing above this file and its
/// library counterpart imports anything generated, so regenerating the bindings
/// cannot ripple into the UI.
library;

import 'dart:async';

import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';

import '../src/rust/api/notes.dart' as rust;
import '../src/rust/frb_generated.dart';
import 'note_core.dart';
import 'note_view.dart';
import 'sync_status.dart';

class RustNoteCore implements NoteCore {
  /// Wraps a note handle the library opened. Made by [RustNoteLibrary.open] and
  /// not by anyone else, which is what keeps one core behind every note.
  RustNoteCore(this._note) {
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
    // Same reasoning for the sync status: one Rust watcher, fanned out here.
    _syncUpstream = _note.watchSync().listen(
      (status) => _sync.add(toStatus(status)),
      onError: _sync.addError,
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

  final rust.Note _note;
  final _controller = StreamController<NoteView>.broadcast();
  late final StreamSubscription<rust.NoteView> _upstream;
  final _sync = StreamController<SyncStatus>.broadcast();
  late final StreamSubscription<rust.SyncStatus> _syncUpstream;

  /// Which note this is, so a screen can say what it is showing.
  String get id => _note.id();

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
  SyncStatus get syncStatus => toStatus(_note.syncStatus());

  @override
  Stream<SyncStatus> watchSync() async* {
    yield syncStatus;
    yield* _sync.stream;
  }

  @override
  Future<void> dispose() async {
    // Stop the Rust watchers: cancelling an upstream subscription waits for that
    // stream to close, and neither closes on its own. The note itself stays open
    // in the library — another screen may be showing it, and the relay keeps
    // syncing it either way.
    await _note.stopWatching();
    await _upstream.cancel();
    await _syncUpstream.cancel();
    await _controller.close();
    await _sync.close();
  }

  /// Shared with [RustNoteLibrary], whose status comes over the same wire.
  ///
  /// `lastSeq` is narrowed here, and this is the only place it could be. The
  /// bridge gives an `i64` as a `PlatformInt64`, which is an `int` natively and
  /// a `BigInt` in a browser, so one Dart `int` field cannot take both — and
  /// widening [SyncStatus] to `PlatformInt64` would put a generated type in the
  /// layer this file exists to keep them out of. Narrowing is safe because the
  /// value is a relay sequence number the app only ever shows: a note would need
  /// nine quadrillion envelopes to reach the edge of a browser's integers, and
  /// `toInt` clamps rather than wraps if one ever did.
  static SyncStatus toStatus(rust.SyncStatus s) => SyncStatus(
    state: switch (s.state) {
      rust.SyncState.offline => SyncState.offline,
      rust.SyncState.connecting => SyncState.connecting,
      rust.SyncState.online => SyncState.online,
      rust.SyncState.synced => SyncState.synced,
    },
    pending: s.pending,
    lastSeq: s.lastSeq.toInt(),
    lastError: s.lastError,
  );

  static NoteView _toView(rust.NoteView v) => NoteView(
    title: v.title,
    items: [
      for (final i in v.items)
        ItemView(id: i.id, text: i.text, checked: i.checked, order: i.order),
    ],
  );
}
