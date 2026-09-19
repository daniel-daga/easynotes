/// [NoteLibrary] backed by the real Rust core through flutter_rust_bridge.
///
/// This and [RustNoteCore] are the whole of the bridge's surface in the app.
/// Nothing above them imports anything generated, so regenerating the bindings
/// cannot ripple into the UI.
library;

import 'dart:async';
import 'dart:typed_data';

import '../src/rust/api/notes.dart' as rust;
import 'browser_store.dart';
import 'note_core.dart';
import 'note_library.dart';
import 'rust_note_core.dart';
import 'sync_status.dart';

class RustNoteLibrary implements NoteLibrary {
  RustNoteLibrary._(this._library, {required this.canJoin, BrowserStore? db})
    : _db = db {
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
    if (_db != null) {
      _drain = Timer.periodic(writeInterval, (_) => _writeOut());
    }
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

  /// Open the notes [db] is holding, unlocked by [masterKey].
  ///
  /// What a browser gets. The rows are read here and handed to the core in one
  /// go, because that read is the one thing about a browser's storage that has
  /// to be waited for and this is where the app is already waiting. From then
  /// on the core answers from memory and leaves its writes in a queue that
  /// [_writeOut] empties — see `browser_store.dart` for why they come back
  /// through Dart rather than being written by Rust.
  ///
  /// [db] is a parameter for the same reason [masterKey] is: so a test can
  /// drive the whole of this over a map, on a machine with no browser on it.
  static Future<RustNoteLibrary> openInBrowser(
    BrowserStore db,
    Uint8List masterKey,
  ) async {
    await RustNoteCore.ensureInitialized();
    final library = RustNoteLibrary._(
      rust.Library.openInBrowser(
        notes: await db.rows(rust.StorageArea.notes),
        keys: await db.rows(rust.StorageArea.keys),
        masterKey: masterKey,
      ),
      canJoin: true,
      db: db,
    );
    // Opening a device for the first time writes its identity, and nothing has
    // asked for those bytes to be kept yet.
    await library.flush();
    return library;
  }

  /// Open notes that live only in memory and are gone when the tab closes.
  ///
  /// What the widget tests use, and what a browser falls back to when it will
  /// not give the app a database — some private-browsing modes will not, and it
  /// can be switched off outright. Everything above the store is the same Rust
  /// the native build runs.
  ///
  /// [canJoin] is false here and nowhere else: a tab like this can reach a
  /// relay perfectly well, but a list joined into it would be gone on the next
  /// reload, and a share that evaporates is worse than a button that is not
  /// there.
  static Future<RustNoteLibrary> openEphemeral() async {
    await RustNoteCore.ensureInitialized();
    return RustNoteLibrary._(rust.Library.openEphemeral(), canJoin: false);
  }

  /// How often the writes the core has queued are handed to the browser.
  ///
  /// The cost of being wrong in each direction is not symmetric, and neither is
  /// large: too long and a tab closed in the gap loses the last edits, too short
  /// and a timer that mostly finds an empty queue runs more often. Fifty
  /// milliseconds is below what anyone notices and well under the time a person
  /// takes to close a tab after typing. The native store has no equivalent gap —
  /// it fsyncs before `append` returns — and [ADR
  /// 0011](../../../docs/adr/0011-the-web-store.md) says why a browser cannot.
  static const writeInterval = Duration(milliseconds: 50);

  final rust.Library _library;

  /// Where this browser keeps the notes, or null on a device with a disk and in
  /// a tab whose notes are only in memory.
  final BrowserStore? _db;
  Timer? _drain;

  /// The write in progress, so that two ticks cannot apply batches out of order.
  Future<void> _writing = Future.value();
  Object? _writeError;
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

  /// Read straight through to Rust, like [notes]: the account document is the
  /// only copy of this.
  @override
  List<Tag> get tags => [
    for (final tag in _library.tags()) Tag(name: tag.name, notes: tag.notes),
  ];

  @override
  Future<String?> addTag(String noteId, String tag) =>
      _library.addTag(id: noteId, tag: tag);

  @override
  Future<void> removeTag(String noteId, String tag) =>
      _library.removeTag(id: noteId, tag: tag);

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

  /// Hand every queued write to the browser and wait for it to land.
  ///
  /// Called on a timer, at open, and at close. It is also the only thing that
  /// surfaces a failed write: the core hands each batch over exactly once and
  /// does not keep a copy, so a browser that refuses one — out of quota, or a
  /// database deleted under the tab — has lost that edit, and the app should
  /// say so rather than carry on looking saved.
  Future<void> flush() {
    _writeOut();
    return _writing;
  }

  /// Whether a write has failed since this library was opened, and which.
  ///
  /// Nothing draws this yet. It is here because the queue is the only place the
  /// failure exists — see the Slice 2 plan, item 4.
  Object? get writeError => _writeError;

  void _writeOut() {
    final db = _db;
    if (db == null) return;
    final batches = _library.takeStorageBatches();
    if (batches.isEmpty) return;
    // Chained rather than awaited in place: the batches are ordered, and a
    // second tick starting before the first finished would let a later write
    // overtake an earlier one.
    _writing = _writing.then((_) async {
      for (final batch in batches) {
        await db.apply(batch);
      }
    }).catchError((Object error) {
      _writeError ??= error;
    });
  }

  @override
  Future<void> dispose() async {
    // The relay connections go first: their engines hold a reference to the
    // core and would otherwise keep applying updates to notes nothing is
    // watching.
    await _library.disconnect();
    // Whatever is still queued, before anything stops: this is the last chance
    // those bytes have to reach the browser.
    _drain?.cancel();
    await flush();
    await _db?.close();
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
    tags: s.tags,
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
