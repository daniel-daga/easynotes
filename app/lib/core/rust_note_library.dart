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
import 'page_visibility_stub.dart'
    if (dart.library.js_interop) 'page_visibility.dart'
    as page;
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
      // And once more the moment the tab goes away, because [writeInterval] is
      // a window in which an edit is on the screen and nowhere else.
      _stopWatchingPage = page.whenPageHidden(_writeOut);
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
  ///
  /// The window is narrower than this number, because a page being hidden
  /// empties the queue too rather than waiting for the next tick. It is not
  /// closed: what the browser then has is a transaction it has been asked to
  /// run, and nothing in `visibilitychange` can wait for one to finish.
  static const writeInterval = Duration(milliseconds: 50);

  final rust.Library _library;

  /// Where this browser keeps the notes, or null on a device with a disk and in
  /// a tab whose notes are only in memory.
  final BrowserStore? _db;
  Timer? _drain;

  /// Stops the `visibilitychange` listener, and does nothing off the web.
  void Function()? _stopWatchingPage;

  /// The write in progress, so that two ticks cannot apply batches out of order.
  Future<void> _writing = Future.value();
  String? _storageError;
  final _storage = StreamController<String?>.broadcast();
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
  List<Reminder> get reminders => [
    for (final reminder in _library.reminders()) _toReminder(reminder),
  ];

  @override
  Stream<List<Reminder>> watchReminders() async* {
    yield reminders;
    await for (final _ in watchNotes()) {
      yield reminders;
    }
  }

  @override
  Future<String> createReminder({
    required String noteId,
    required DateTime fireAt,
    required ReminderRepeat repeat,
    required String timezone,
  }) => _library.createReminder(
    note: noteId,
    fireAt: fireAt.millisecondsSinceEpoch.toString(),
    repeat: _toRustRepeat(repeat),
    timezone: timezone,
  );

  @override
  Future<void> setReminder(Reminder reminder) => _library.setReminder(
    reminder: reminder.id,
    note: reminder.noteId,
    fireAt: reminder.fireAt.millisecondsSinceEpoch.toString(),
    repeat: _toRustRepeat(reminder.repeat),
    timezone: reminder.timezone,
  );

  @override
  Future<void> forgetReminder(String reminderId) =>
      _library.forgetReminder(reminder: reminderId);

  @override
  Future<String> createNote({NoteKind kind = NoteKind.checklist}) =>
      _library.createNote(kind: RustNoteCore.toRustKind(kind));

  // -- invites --------------------------------------------------------------

  @override
  People people(String noteId) {
    final view = _library.people(id: noteId);
    return People(
      shared: view.shared,
      canManage: view.canManage,
      people: [
        for (final p in view.people)
          Person(
            member: p.member,
            invite: p.invite,
            name: p.name,
            owner: p.owner,
            me: p.me,
            removing: p.removing,
          ),
      ],
      adding: view.adding,
    );
  }

  static Invite _toInvite(rust.InviteView invite) => Invite(
    id: invite.id,
    noteId: invite.note,
    link: invite.link,
    expiresIn: Duration(seconds: invite.expiresInSecs),
  );

  @override
  Future<Invite> createInvite(String noteId) async =>
      _toInvite(await _library.createInvite(note: noteId));

  @override
  List<Invite> invitesFor(String noteId) =>
      _library.invites(note: noteId).map(_toInvite).toList();

  @override
  List<Invite> get allInvites => _library.allInvites().map(_toInvite).toList();

  @override
  Future<InviteStatus> checkInvite(String inviteId) async {
    final status = await _library.checkInvite(invite: inviteId);
    return InviteStatus(
      open: status.open,
      expiresIn: Duration(seconds: status.expiresInSecs),
      lettingIn: status.lettingIn,
      joined: status.joined,
    );
  }

  @override
  Future<void> withdrawInvite(String inviteId) =>
      _library.withdrawInvite(invite: inviteId);

  @override
  Future<void> removePerson(String noteId, Person person) => _library
      .removePerson(id: noteId, member: person.member, invite: person.invite);

  @override
  Future<void> stopSharing(String noteId) => _library.stopSharing(id: noteId);

  @override
  List<Removal> takeRemovals() => [
    for (final r in _library.takeRemovals())
      Removal(noteId: r.note, title: r.title),
  ];

  static PendingJoin _toJoin(rust.PendingJoinView join) =>
      PendingJoin(inviteId: join.invite, name: join.name);

  @override
  Future<PendingJoin> requestJoin(String link) async =>
      _toJoin(await _library.requestJoin(link: link));

  @override
  List<PendingJoin> get pendingJoins =>
      _library.pendingJoins().map(_toJoin).toList();

  @override
  Future<JoinCheck> checkJoin(String inviteId) async {
    final check = await _library.checkJoin(invite: inviteId);
    return JoinCheck(
      switch (check.state) {
        rust.JoinStateView.waiting => JoinState.waiting,
        rust.JoinStateView.opening => JoinState.opening,
        rust.JoinStateView.joined => JoinState.joined,
        rust.JoinStateView.declined => JoinState.declined,
        rust.JoinStateView.gone => JoinState.gone,
      },
      noteId: check.note,
      message: check.message,
    );
  }

  @override
  Future<void> cancelJoin(String inviteId) =>
      _library.cancelJoin(invite: inviteId);

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
  Future<void> moveNoteTo(String noteId, String targetId) =>
      _library.moveNoteTo(id: noteId, target: targetId);

  /// Read straight through to Rust, like [notes]: the account document is the
  /// only copy of this.
  @override
  List<Tag> get tags => [
    for (final tag in _library.tags())
      Tag(id: tag.id, name: tag.name, notes: tag.notes),
  ];

  @override
  Future<String?> addTag(String noteId, String name) =>
      _library.addTag(id: noteId, tag: name);

  @override
  Future<String> makeTag(String name) => _library.makeTag(name: name);

  @override
  Future<void> removeTag(String noteId, String labelId) =>
      _library.removeTag(id: noteId, label: labelId);

  @override
  Future<String?> renameTag(String labelId, String name) =>
      _library.renameTag(label: labelId, name: name);

  @override
  Future<void> forgetTag(String labelId) => _library.forgetTag(label: labelId);

  @override
  Future<void> setArchived(String noteId, bool archived) =>
      _library.setArchived(id: noteId, archived: archived);

  @override
  Future<void> setTrashed(String noteId, bool trashed) =>
      _library.setTrashed(id: noteId, trashed: trashed);

  @override
  Future<void> forgetNote(String noteId) async {
    await _library.forgetNote(id: noteId);
    await flush();
    if (_storageError != null) throw StateError(_storageError!);
  }

  @override
  Future<int> emptyTrash() async {
    final count = await _library.emptyTrash();
    await flush();
    if (_storageError != null) throw StateError(_storageError!);
    return count;
  }

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
  bool get tickedShown => _library.tickedShown();

  @override
  Future<void> setTickedShown(bool shown) =>
      _library.setTickedShown(shown: shown);

  @override
  bool get hasAccount => _library.hasAccount();

  @override
  Future<List<String>> createAccount() => _library.createAccount();

  @override
  Future<void> adoptAccount(String phrase) =>
      _library.adoptAccount(phrase: phrase);

  @override
  String? get profileName => _library.profileName();

  @override
  Future<void> setProfileName(String name) =>
      _library.setProfileName(name: name);

  @override
  Future<void> connect(String url) => _library.connect(relay: url);

  @override
  Future<void> disconnect() => _library.disconnect();

  @override
  void sendHeldEdits() => _library.sendHeldEdits();

  @override
  SyncStatus get syncStatus => RustNoteCore.toStatus(_library.syncStatus());

  @override
  Stream<SyncStatus> watchSync() async* {
    yield syncStatus;
    yield* _sync.stream;
  }

  /// Hand every queued write to the browser and wait for it to land.
  ///
  /// Called on a timer, at open, when the page is hidden, and at close. A
  /// refusal goes to [storageError] rather than out of here: the timer is what
  /// calls this most of the time and there is nobody at the other end of that
  /// call to throw to.
  Future<void> flush() {
    _writeOut();
    return _writing;
  }

  @override
  String? get storageError => _storageError;

  @override
  Stream<String?> watchStorageError() async* {
    yield _storageError;
    yield* _storage.stream;
  }

  void _writeOut() {
    final db = _db;
    if (db == null) return;
    final batches = _library.takeStorageBatches();
    if (batches.isEmpty) return;
    // Chained rather than awaited in place: the batches are ordered, and a
    // second tick starting before the first finished would let a later write
    // overtake an earlier one.
    _writing = _writing
        .then((_) async {
          for (final batch in batches) {
            await db.apply(batch);
          }
        })
        .catchError(_writeFailed);
  }

  /// The browser refused a batch, which means an edit is gone.
  ///
  /// The first one is the one reported, for the reason [storageError] latches:
  /// a database that has started refusing writes will refuse the next one too,
  /// and the tenth message says nothing the first did not. Nothing here retries
  /// — there is nothing left to retry, the batch was handed over and dropped.
  void _writeFailed(Object error) {
    if (_storageError != null) return;
    _storageError = '$error';
    _storage.add(_storageError);
  }

  /// Set by the first [dispose]. Disposing twice is ordinary — a caller that
  /// closes explicitly often has a teardown registered as well — and it used to
  /// be harmless because every step was idempotent on its own. Closing the store
  /// is not: the second pass would ask a shut library to stop watching and be
  /// told, correctly, that it is closed.
  bool _disposed = false;

  @override
  Future<void> dispose() async {
    if (_disposed) return;
    _disposed = true;
    // The relay connections go first: their engines hold a reference to the
    // core and would otherwise keep applying updates to notes nothing is
    // watching.
    await _library.disconnect();
    // Whatever is still queued, before anything stops: this is the last chance
    // those bytes have to reach the browser.
    _drain?.cancel();
    _stopWatchingPage?.call();
    await flush();
    await _db?.close();
    // Then stop the Rust watchers: cancelling an upstream subscription waits for
    // that stream to close, and neither closes on its own.
    await _library.stopWatching();
    await _upstream.cancel();
    await _syncUpstream.cancel();
    await _notes.close();
    await _sync.close();
    await _storage.close();
    // And last, the store itself. This has to be asked for: the bridge frees
    // the Rust object when Dart collects it, which is not a moment this app
    // chooses, so until this call existed the database stayed open long after
    // `dispose()` returned. Last because everything above may still read.
    await _library.close();
  }

  static NoteSummary _toSummary(rust.NoteSummaryView s) => NoteSummary(
    collaborators: [
      for (final p in s.collaborators) Collaborator(id: p.id, name: p.name),
    ],
    id: s.id,
    kind: RustNoteCore.fromRustKind(s.kind),
    title: s.title,
    items: s.items,
    checked: s.checked,
    preview: s.preview,
    colour: _fromRustColour(s.colour),
    pinned: s.pinned,
    archived: s.archived,
    trashed: s.trashed,
    tags: [for (final tag in s.tags) Label(id: tag.id, name: tag.name)],
  );

  static Reminder _toReminder(rust.ReminderView reminder) => Reminder(
    id: reminder.id,
    noteId: reminder.note,
    fireAt: DateTime.fromMillisecondsSinceEpoch(
      int.parse(reminder.fireAt),
      isUtc: true,
    ),
    repeat: switch (reminder.repeat) {
      rust.ReminderRepeat.never => ReminderRepeat.never,
      rust.ReminderRepeat.daily => ReminderRepeat.daily,
      rust.ReminderRepeat.weekly => ReminderRepeat.weekly,
      rust.ReminderRepeat.monthly => ReminderRepeat.monthly,
      rust.ReminderRepeat.yearly => ReminderRepeat.yearly,
    },
    timezone: reminder.timezone,
  );

  static rust.ReminderRepeat _toRustRepeat(ReminderRepeat repeat) =>
      switch (repeat) {
        ReminderRepeat.never => rust.ReminderRepeat.never,
        ReminderRepeat.daily => rust.ReminderRepeat.daily,
        ReminderRepeat.weekly => rust.ReminderRepeat.weekly,
        ReminderRepeat.monthly => rust.ReminderRepeat.monthly,
        ReminderRepeat.yearly => rust.ReminderRepeat.yearly,
      };

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
