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

/// A [StorageHealth] on its own, for a screen that wants one without a library.
///
/// The checklist screen is the case: it is given one note and the device's
/// answer to "is this being kept?", and nothing else.
class FakeStorageHealth implements StorageHealth {
  final _errors = StreamController<String?>.broadcast();
  String? _error;

  @override
  String? get storageError => _error;

  @override
  Stream<String?> watchStorageError() async* {
    yield _error;
    yield* _errors.stream;
  }

  /// Say that a write was refused, without a browser to refuse it.
  void push(String error) {
    _error = error;
    _errors.add(error);
  }

  Future<void> dispose() => _errors.close();
}

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
  final _storageErrors = StreamController<String?>.broadcast();
  String? _storageError;

  /// Newest first, as the real one is.
  final List<String> _order = [];
  final Map<String, FakeNoteCore> _cores = {};
  final Map<String, StreamSubscription<void>> _watchers = {};
  final Map<String, _Placement> _placements = {};
  NoteLayout _layout = NoteLayout.grid;
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

  /// The same rule the core follows: every whitespace-separated word has to
  /// appear somewhere in the note, case ignored, and the line that comes back is
  /// the first one — title, then items — holding any of them.
  @override
  Future<List<SearchHit>> search(String query) async {
    calls.add('search($query)');
    final terms = query.toLowerCase().split(RegExp(r'\s+'))
      ..removeWhere((term) => term.isEmpty);
    if (terms.isEmpty) return const [];
    final hits = <SearchHit>[];
    for (final note in _summaries) {
      final view = _cores[note.id]!.current;
      final lines = [view.title, for (final item in view.items) item.text];
      bool holds(String term) =>
          lines.any((line) => line.toLowerCase().contains(term));
      if (!terms.every(holds)) continue;
      final matched = lines.firstWhere(
        (line) => terms.any((term) => line.toLowerCase().contains(term)),
      );
      hits.add(SearchHit(note: note, matched: matched));
    }
    return hits;
  }

  @override
  NoteCore openNote(String noteId) {
    calls.add('openNote($noteId)');
    return _cores[noteId] ?? (throw StateError('no note $noteId'));
  }

  @override
  Future<void> setColour(String noteId, NoteColour? colour) async {
    calls.add('setColour($noteId, ${colour?.name})');
    _placement(noteId).colour = colour;
    _emit();
  }

  @override
  Future<void> setPinned(String noteId, bool pinned) async {
    calls.add('setPinned($noteId, $pinned)');
    _placement(noteId).pinned = pinned;
    _emit();
  }

  /// Counted off the notes, as the real one does: a label exists because a note
  /// carries it, and sorted so a menu holds still.
  @override
  List<Tag> get tags {
    final counts = <String, int>{};
    for (final placement in _placements.values) {
      for (final tag in placement.tags) {
        counts[tag] = (counts[tag] ?? 0) + 1;
      }
    }
    final names = counts.keys.toList()
      ..sort((a, b) {
        final folded = a.toLowerCase().compareTo(b.toLowerCase());
        return folded != 0 ? folded : a.compareTo(b);
      });
    return [for (final name in names) Tag(name: name, notes: counts[name]!)];
  }

  /// The core's rules, kept here so the screen sees the same thing from both:
  /// whitespace collapsed, nothing for a name that is not one, and a label
  /// differing only in case from one already here *is* that one.
  @override
  Future<String?> addTag(String noteId, String tag) async {
    calls.add('addTag($noteId, $tag)');
    final name = tag.trim().replaceAll(RegExp(r'\s+'), ' ');
    if (name.isEmpty) return null;
    final existing = tags.firstWhere(
      (held) => held.name.toLowerCase() == name.toLowerCase(),
      orElse: () => Tag(name: name, notes: 0),
    );
    _placement(noteId).tags.add(existing.name);
    _emit();
    return existing.name;
  }

  @override
  Future<void> removeTag(String noteId, String tag) async {
    calls.add('removeTag($noteId, $tag)');
    _placement(noteId).tags.removeWhere(
      (held) => held.toLowerCase() == tag.trim().toLowerCase(),
    );
    _emit();
  }

  @override
  Future<void> setArchived(String noteId, bool archived) async {
    calls.add('setArchived($noteId, $archived)');
    final placement = _placement(noteId);
    placement.archived = archived;
    // The core's rule, kept here so the screen sees the same thing from both: a
    // note cannot be both held at the top and put out of the way.
    if (archived) placement.pinned = false;
    _emit();
  }

  @override
  Future<void> setTrashed(String noteId, bool trashed) async {
    calls.add('setTrashed($noteId, $trashed)');
    final placement = _placement(noteId);
    placement.trashed = trashed;
    if (trashed) placement.pinned = false;
    _emit();
  }

  @override
  Future<void> forgetNote(String noteId) async {
    calls.add('forgetNote($noteId)');
    _forget(noteId);
    _emit();
  }

  @override
  Future<int> emptyTrash() async {
    calls.add('emptyTrash()');
    final trashed = [
      for (final id in _order)
        if (_placements[id]?.trashed ?? false) id,
    ];
    for (final id in trashed) {
      _forget(id);
    }
    _emit();
    return trashed.length;
  }

  @override
  NoteLayout get layout => _layout;

  @override
  Future<void> setLayout(NoteLayout layout) async {
    calls.add('setLayout(${layout.name})');
    _layout = layout;
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
  String? get storageError => _storageError;

  @override
  Stream<String?> watchStorageError() async* {
    yield _storageError;
    yield* _storageErrors.stream;
  }

  /// Say that a write was refused, without a browser to refuse it.
  ///
  /// It says exactly what it is told and latches nothing. Latching is the real
  /// library's decision and `rust_bridge_test.dart` is where it is held; a fake
  /// that made it too would be a second copy of a rule, and a screen test that
  /// passed because of it would be testing this file.
  void pushStorageError(String error) {
    _storageError = error;
    _storageErrors.add(error);
  }

  @override
  Future<void> dispose() async {
    for (final watcher in _watchers.values) {
      await watcher.cancel();
    }
    await _notes.close();
    await _sync.close();
    await _storageErrors.close();
  }

  void _add(String id, FakeNoteCore core) {
    _order.insert(0, id);
    _cores[id] = core;
    // The real list is refreshed by the core on every edit, wherever it came
    // from. Here that means watching each note, so that a test editing a core
    // directly — standing in for a peer's edit arriving — moves the card.
    _watchers[id] = core.watch().listen((_) => _emit());
  }

  _Placement _placement(String id) =>
      _placements.putIfAbsent(id, _Placement.new);

  /// Everything the real library forgets: the note, this person's placement of
  /// it, and the watcher behind its card. Not `_emit`: the callers above do that
  /// once, so emptying a trash of ten notes is one list and not ten.
  void _forget(String id) {
    _order.remove(id);
    _cores.remove(id);
    _placements.remove(id);
    _watchers.remove(id)?.cancel();
  }

  void _emit() {
    final summaries = [
      for (final id in _order)
        NoteSummary(
          id: id,
          title: _cores[id]!.current.title,
          items: _cores[id]!.current.items.length,
          checked: _cores[id]!.current.checkedCount,
          preview: [
            for (final item in _cores[id]!.current.items.take(4)) item.text,
          ],
          colour: _placements[id]?.colour,
          pinned: _placements[id]?.pinned ?? false,
          archived: _placements[id]?.archived ?? false,
          trashed: _placements[id]?.trashed ?? false,
          tags: [...?_placements[id]?.tags]..sort(),
        ),
    ];
    // Pinned first, as the core sorts them; the rest keep the order they were
    // made in. A stable sort, so pinning one note does not reshuffle the others.
    final pinned = summaries.where((s) => s.pinned);
    final rest = summaries.where((s) => !s.pinned);
    _summaries = [...pinned, ...rest];
    _notes.add(_summaries);
  }
}

/// What the fake remembers about a note, standing in for the account document.
class _Placement {
  NoteColour? colour;
  bool pinned = false;
  bool archived = false;
  bool trashed = false;
  final Set<String> tags = {};
}
