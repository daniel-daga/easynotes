/// [NoteLibrary] backed by the real Rust core through flutter_rust_bridge.
///
/// This and [RustNoteCore] are the whole of the bridge's surface in the app.
/// Nothing above them imports anything generated, so regenerating the bindings
/// cannot ripple into the UI.
library;

import 'dart:async';

import '../src/rust/api/notes.dart' as rust;
import 'note_core.dart';
import 'note_library.dart';
import 'rust_note_core.dart';
import 'sync_status.dart';

class RustNoteLibrary implements NoteLibrary {
  RustNoteLibrary._(this._library, {required this.canJoin}) {
    // One Rust watcher per stream, fanned out to however many widgets want it —
    // for the reason [RustNoteCore] gives: a Rust watcher parks on a channel, so
    // cancelling its subscription has to wait for the Rust side to wake, and a
    // widget disposing would hang. [dispose] is the one place that talks to Rust
    // about stopping.
    _upstream = _library.watchNotes().listen(
      (notes) => _notes.add(notes.map(_toSummary).toList()),
      onError: _notes.addError,
    );
    _syncUpstream = _library.watchSync().listen(
      (status) => _sync.add(RustNoteCore.toStatus(status)),
      onError: _sync.addError,
    );
  }

  /// Open the notes stored under [directory], on a device with a disk.
  static Future<RustNoteLibrary> open(String directory) async {
    await RustNoteCore.ensureInitialized();
    return RustNoteLibrary._(rust.Library.open(dir: directory), canJoin: true);
  }

  /// Open notes that live only in memory and are gone when the tab closes.
  ///
  /// The web build uses this: a browser has no filesystem, and backing the op
  /// log with IndexedDB is item 4 of the Slice 2 plan. Everything above the
  /// store — crypto, CRDT, the watch streams — is the same Rust the native
  /// build runs, so this is a stepping stone and not a mock. See
  /// `docs/adr/0002-web-core.md`.
  static Future<RustNoteLibrary> openEphemeral() async {
    await RustNoteCore.ensureInitialized();
    return RustNoteLibrary._(rust.Library.openEphemeral(), canJoin: false);
  }

  final rust.Library _library;
  final _notes = StreamController<List<NoteSummary>>.broadcast();
  late final StreamSubscription<List<rust.NoteSummaryView>> _upstream;
  final _sync = StreamController<SyncStatus>.broadcast();
  late final StreamSubscription<rust.SyncStatus> _syncUpstream;

  @override
  final bool canJoin;

  /// Read straight through to Rust rather than caching. A cached copy is only
  /// fresh while something is listening to [watchNotes], and a caller that reads
  /// this without subscribing would silently get a stale list.
  @override
  List<NoteSummary> get notes => _library.notes().map(_toSummary).toList();

  @override
  Stream<List<NoteSummary>> watchNotes() async* {
    yield notes;
    yield* _notes.stream;
  }

  @override
  Future<String> createNote() => _library.createNote();

  @override
  Future<String> join(String blob) => _library.join(blob: blob);

  @override
  NoteCore openNote(String noteId) => RustNoteCore(_library.note(id: noteId));

  @override
  Future<void> connect(String url) => _library.connect(relay: url);

  @override
  Future<void> disconnect() => _library.disconnect();

  @override
  SyncStatus get syncStatus => RustNoteCore.toStatus(_library.syncStatus());

  @override
  Stream<SyncStatus> watchSync() async* {
    yield syncStatus;
    yield* _sync.stream;
  }

  @override
  Future<void> dispose() async {
    // The relay connections go first: their engines hold a reference to the
    // core and would otherwise keep applying updates to notes nothing is
    // watching.
    await _library.disconnect();
    // Then stop the Rust watchers: cancelling an upstream subscription waits for
    // that stream to close, and neither closes on its own.
    await _library.stopWatching();
    await _upstream.cancel();
    await _syncUpstream.cancel();
    await _notes.close();
    await _sync.close();
  }

  static NoteSummary _toSummary(rust.NoteSummaryView s) => NoteSummary(
    id: s.id,
    title: s.title,
    items: s.items,
    checked: s.checked,
    preview: s.preview,
  );
}
