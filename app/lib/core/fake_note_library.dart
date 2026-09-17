/// An in-memory [NoteLibrary] for widget tests.
///
/// The same bargain as [FakeNoteCore]: it is not a CRDT and not a store, and it
/// does not pretend to be. What it has to get right is the contract the note
/// list screen depends on — a note created or joined appears in the list, an
/// edit to a note changes its card, and the list arrives on a stream rather than
/// being handed over to be kept.
library;

import 'dart:async';

import 'fake_note_core.dart';
import 'note_core.dart';
import 'note_library.dart';
import 'sync_status.dart';

class FakeNoteLibrary implements NoteLibrary {
  FakeNoteLibrary({Map<String, FakeNoteCore>? notes, this.canJoin = true}) {
    if (notes != null) {
      // In the order they are written: a test that writes `{'a': …, 'b': …}`
      // means "a was made first", and the real library lists newest first.
      for (final entry in notes.entries) {
        _add(entry.key, entry.value);
      }
    }
    _emit();
  }

  final _notes = StreamController<List<NoteSummary>>.broadcast();
  final _sync = StreamController<SyncStatus>.broadcast();

  /// Newest first, as the real one is.
  final List<String> _order = [];
  final Map<String, FakeNoteCore> _cores = {};
  final Map<String, StreamSubscription<void>> _watchers = {};
  int _counter = 0;

  /// Every call the screen made, in order. Tests assert on this to show that a
  /// gesture reached the library rather than only changing pixels.
  final List<String> calls = [];

  List<NoteSummary> _summaries = const [];
  SyncStatus _status = const SyncStatus.offline();

  @override
  final bool canJoin;

  /// The core behind one note, for a test that wants to edit it from underneath
  /// the screen.
  FakeNoteCore core(String id) => _cores[id]!;

  @override
  List<NoteSummary> get notes => _summaries;

  @override
  Stream<List<NoteSummary>> watchNotes() async* {
    yield _summaries;
    yield* _notes.stream;
  }

  @override
  Future<String> createNote() async {
    calls.add('createNote()');
    final id = 'note-${_counter++}';
    _add(id, FakeNoteCore());
    _emit();
    return id;
  }

  @override
  Future<String> join(String blob) async {
    calls.add('join($blob)');
    final id = 'joined-${_counter++}';
    _add(id, FakeNoteCore(title: 'Shared list'));
    _emit();
    return id;
  }

  @override
  NoteCore openNote(String noteId) {
    calls.add('openNote($noteId)');
    return _cores[noteId] ?? (throw StateError('no note $noteId'));
  }

  @override
  Future<void> connect(String url) async {
    calls.add('connect($url)');
    pushStatus(const SyncStatus(state: SyncState.connecting));
  }

  @override
  Future<void> disconnect() async {
    calls.add('disconnect()');
    pushStatus(const SyncStatus.offline());
  }

  @override
  SyncStatus get syncStatus => _status;

  @override
  Stream<SyncStatus> watchSync() async* {
    yield _status;
    yield* _sync.stream;
  }

  /// Say what the engines would say, without a network to say it with.
  void pushStatus(SyncStatus status) {
    _status = status;
    _sync.add(status);
  }

  @override
  Future<void> dispose() async {
    for (final watcher in _watchers.values) {
      await watcher.cancel();
    }
    await _notes.close();
    await _sync.close();
  }

  void _add(String id, FakeNoteCore core) {
    _order.insert(0, id);
    _cores[id] = core;
    // The real list is refreshed by the core on every edit, wherever it came
    // from. Here that means watching each note, so that a test editing a core
    // directly — standing in for a peer's edit arriving — moves the card.
    _watchers[id] = core.watch().listen((_) => _emit());
  }

  void _emit() {
    _summaries = [
      for (final id in _order)
        NoteSummary(
          id: id,
          title: _cores[id]!.current.title,
          items: _cores[id]!.current.items.length,
          checked: _cores[id]!.current.checkedCount,
          preview: [
            for (final item in _cores[id]!.current.items.take(4)) item.text,
          ],
        ),
    ];
    _notes.add(_summaries);
  }
}
