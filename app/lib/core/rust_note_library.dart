/// [NoteLibrary] backed by the real Rust core through flutter_rust_bridge.
///
/// This and [RustNoteCore] are the whole of the bridge's surface in the app.
/// Nothing above them imports anything generated, so regenerating the bindings
/// cannot ripple into the UI.
library;

import 'dart:async';
import 'dart:typed_data';

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

  /// Open the notes stored under [directory], unlocked by [masterKey].
  ///
  /// The key comes from [MasterKeyStore], which is to say from the OS keychain.
  /// It is a parameter rather than something this reads for itself so that a
  /// test can open a library without a keychain to put a key in — and because
  /// the one place that decides where the key lives should be easy to find.
  static Future<RustNoteLibrary> open(
    String directory,
    Uint8List masterKey,
  ) async {
    await RustNoteCore.ensureInitialized();
    return RustNoteLibrary._(
      rust.Library.open(dir: directory, masterKey: masterKey),
      canJoin: true,
    );
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
  Future<List<SearchHit>> search(String query) async {
    final hits = await _library.search(query: query);
    return [
      for (final hit in hits)
        SearchHit(note: _toSummary(hit.note), matched: hit.matched),
    ];
  }

  @override
  NoteCore openNote(String noteId) => RustNoteCore(_library.note(id: noteId));

  @override
  Future<void> setColour(String noteId, NoteColour? colour) =>
      _library.setColour(id: noteId, colour: _toRustColour(colour));

  @override
  Future<void> setPinned(String noteId, bool pinned) =>
      _library.setPinned(id: noteId, pinned: pinned);

  @override
  Future<void> setArchived(String noteId, bool archived) =>
      _library.setArchived(id: noteId, archived: archived);

  @override
  Future<void> setTrashed(String noteId, bool trashed) =>
      _library.setTrashed(id: noteId, trashed: trashed);

  @override
  Future<void> forgetNote(String noteId) => _library.forgetNote(id: noteId);

  @override
  Future<int> emptyTrash() => _library.emptyTrash();

  /// Read straight through to Rust, like [notes]: the account document is the
  /// only copy of this, and a cached one would be stale the moment another
  /// screen changed it.
  @override
  NoteLayout get layout => switch (_library.layout()) {
    rust.NoteLayout.grid => NoteLayout.grid,
    rust.NoteLayout.list => NoteLayout.list,
  };

  @override
  Future<void> setLayout(NoteLayout layout) => _library.setLayout(
    layout: switch (layout) {
      NoteLayout.grid => rust.NoteLayout.grid,
      NoteLayout.list => rust.NoteLayout.list,
    },
  );

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
    colour: _fromRustColour(s.colour),
    pinned: s.pinned,
    archived: s.archived,
    trashed: s.trashed,
  );

  /// The two palettes are the same list in two places, which is what keeps the
  /// UI from importing anything generated. A colour the Rust side adds and this
  /// one has not heard of would not compile, which is the point of writing it
  /// out rather than going through an index.
  static NoteColour? _fromRustColour(rust.NoteColour? colour) =>
      switch (colour) {
        null => null,
        rust.NoteColour.red => NoteColour.red,
        rust.NoteColour.orange => NoteColour.orange,
        rust.NoteColour.yellow => NoteColour.yellow,
        rust.NoteColour.green => NoteColour.green,
        rust.NoteColour.teal => NoteColour.teal,
        rust.NoteColour.blue => NoteColour.blue,
        rust.NoteColour.purple => NoteColour.purple,
        rust.NoteColour.pink => NoteColour.pink,
        rust.NoteColour.brown => NoteColour.brown,
        rust.NoteColour.grey => NoteColour.grey,
      };

  static rust.NoteColour? _toRustColour(NoteColour? colour) => switch (colour) {
    null => null,
    NoteColour.red => rust.NoteColour.red,
    NoteColour.orange => rust.NoteColour.orange,
    NoteColour.yellow => rust.NoteColour.yellow,
    NoteColour.green => rust.NoteColour.green,
    NoteColour.teal => rust.NoteColour.teal,
    NoteColour.blue => rust.NoteColour.blue,
    NoteColour.purple => rust.NoteColour.purple,
    NoteColour.pink => rust.NoteColour.pink,
    NoteColour.brown => rust.NoteColour.brown,
    NoteColour.grey => rust.NoteColour.grey,
  };
}
